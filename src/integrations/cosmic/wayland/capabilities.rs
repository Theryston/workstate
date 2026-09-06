use cosmic_protocols::{
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    workspace::v2::client::{zcosmic_workspace_handle_v2, zcosmic_workspace_manager_v2},
};
use smithay_client_toolkit::registry::RegistryState;
use wayland_client::{
    Proxy, WEnum,
    globals::Global,
    protocol::{wl_output, wl_seat},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1,
    workspace::v1::client::ext_workspace_manager_v1,
};

use super::super::errors::CosmicError;

const MINIMUM_WORKSPACE_MANAGER_VERSION: u32 = 1;
const MINIMUM_COSMIC_WORKSPACE_MANAGER_VERSION: u32 = 1;
const MINIMUM_FOREIGN_TOPLEVEL_LIST_VERSION: u32 = 1;
const MINIMUM_COSMIC_TOPLEVEL_INFO_VERSION: u32 = 3;
const MINIMUM_OUTPUT_VERSION: u32 = 1;
const MINIMUM_SEAT_VERSION: u32 = 1;
#[allow(dead_code)]
const MINIMUM_TOPLEVEL_MANAGER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadCapabilities {
    pub(crate) workspace_enumeration: bool,
    pub(crate) foreign_toplevel_enumeration: bool,
    pub(crate) cosmic_toplevel_information: bool,
    pub(crate) workspace_manager_version: u32,
    pub(crate) cosmic_workspace_manager_version: u32,
    pub(crate) foreign_toplevel_list_version: u32,
    pub(crate) cosmic_toplevel_info_version: u32,
    pub(crate) output_version: u32,
    pub(crate) seat_version: u32,
    pub(crate) output_count: usize,
    pub(crate) seat_count: usize,
    pub(crate) target_workspace_output_association: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ManagementCapabilities {
    pub(crate) global_available: bool,
    pub(crate) close: bool,
    pub(crate) activate: bool,
    pub(crate) move_to_external_workspace: bool,
}

#[allow(dead_code)]
impl ManagementCapabilities {
    pub(crate) fn with_global() -> Self {
        Self {
            global_available: true,
            ..Self::default()
        }
    }

    pub(crate) fn from_protocol(
        values: impl IntoIterator<
            Item = WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>,
        >,
    ) -> Self {
        let mut capabilities = Self::with_global();
        for value in values {
            let WEnum::Value(value) = value else {
                continue;
            };
            match value {
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Close => {
                    capabilities.close = true;
                }
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Activate => {
                    capabilities.activate = true;
                }
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::MoveToExtWorkspace => {
                    capabilities.move_to_external_workspace = true;
                }
                _ => {}
            }
        }
        capabilities
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagementCapability {
    ToplevelManagement,
    WindowClose,
    WindowActivation,
    MoveToExternalWorkspace,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadCapability {
    WorkspaceEnumeration,
    ForeignToplevelEnumeration,
    CosmicToplevelInformation,
    AvailableSeats,
    TargetWorkspaceOutputAssociation,
}

impl ManagementCapability {
    fn label(self) -> &'static str {
        match self {
            Self::ToplevelManagement => "toplevel management",
            Self::WindowClose => "window close",
            Self::WindowActivation => "window activation",
            Self::MoveToExternalWorkspace => "move to external workspace",
        }
    }
}

impl ReadCapability {
    fn label(self) -> &'static str {
        match self {
            Self::WorkspaceEnumeration => "workspace enumeration",
            Self::ForeignToplevelEnumeration => "foreign toplevel enumeration",
            Self::CosmicToplevelInformation => "COSMIC toplevel information",
            Self::AvailableSeats => "available seats",
            Self::TargetWorkspaceOutputAssociation => "target workspace output association",
        }
    }
}

pub(crate) fn validate_read_capabilities(
    globals: &[Global],
    operation: &str,
) -> Result<ReadCapabilities, CosmicError> {
    let workspace_manager_version = require_global::<
        ext_workspace_manager_v1::ExtWorkspaceManagerV1,
    >(globals, MINIMUM_WORKSPACE_MANAGER_VERSION, operation)?;
    let cosmic_workspace_manager_version =
        require_global::<zcosmic_workspace_manager_v2::ZcosmicWorkspaceManagerV2>(
            globals,
            MINIMUM_COSMIC_WORKSPACE_MANAGER_VERSION,
            operation,
        )?;
    let foreign_toplevel_list_version =
        require_global::<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>(
            globals,
            MINIMUM_FOREIGN_TOPLEVEL_LIST_VERSION,
            operation,
        )?;
    let cosmic_toplevel_info_version = require_global_by_name(
        globals,
        "zcosmic_toplevel_info_v1",
        MINIMUM_COSMIC_TOPLEVEL_INFO_VERSION,
        operation,
    )?;
    let output_version =
        require_global::<wl_output::WlOutput>(globals, MINIMUM_OUTPUT_VERSION, operation)?;
    let seat_version = require_global::<wl_seat::WlSeat>(globals, MINIMUM_SEAT_VERSION, operation)?;
    let output_count = count_globals::<wl_output::WlOutput>(globals);
    let seat_count = count_globals::<wl_seat::WlSeat>(globals);

    Ok(ReadCapabilities {
        workspace_enumeration: true,
        foreign_toplevel_enumeration: true,
        cosmic_toplevel_information: true,
        workspace_manager_version,
        cosmic_workspace_manager_version,
        foreign_toplevel_list_version,
        cosmic_toplevel_info_version,
        output_version,
        seat_version,
        output_count,
        seat_count,
        target_workspace_output_association: output_count > 0,
    })
}

#[allow(dead_code)]
pub(crate) fn validate_mutation_capabilities(
    globals: &[Global],
    operation: &str,
) -> Result<u32, CosmicError> {
    require_global::<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1>(
        globals,
        MINIMUM_TOPLEVEL_MANAGER_VERSION,
        operation,
    )
}

#[allow(dead_code)]
pub(crate) fn require_management_capability(
    capabilities: ManagementCapabilities,
    capability: ManagementCapability,
    operation: &str,
) -> Result<(), CosmicError> {
    let available = match capability {
        ManagementCapability::ToplevelManagement => capabilities.global_available,
        ManagementCapability::WindowClose => capabilities.close,
        ManagementCapability::WindowActivation => capabilities.activate,
        ManagementCapability::MoveToExternalWorkspace => capabilities.move_to_external_workspace,
    };
    if available {
        return Ok(());
    }

    Err(CosmicError::CapabilityUnavailable {
        operation: operation.to_owned(),
        capability: capability.label().to_owned(),
        detail: "the compositor did not advertise this capability".to_owned(),
    })
}

pub(crate) fn require_read_capability(
    capabilities: ReadCapabilities,
    capability: ReadCapability,
    operation: &str,
) -> Result<(), CosmicError> {
    let available = match capability {
        ReadCapability::WorkspaceEnumeration => capabilities.workspace_enumeration,
        ReadCapability::ForeignToplevelEnumeration => capabilities.foreign_toplevel_enumeration,
        ReadCapability::CosmicToplevelInformation => capabilities.cosmic_toplevel_information,
        ReadCapability::AvailableSeats => capabilities.seat_count > 0,
        ReadCapability::TargetWorkspaceOutputAssociation => {
            capabilities.target_workspace_output_association
        }
    };
    if available {
        return Ok(());
    }

    Err(CosmicError::CapabilityUnavailable {
        operation: operation.to_owned(),
        capability: capability.label().to_owned(),
        detail: "the detected COSMIC session does not provide it".to_owned(),
    })
}

#[allow(dead_code)]
pub(crate) fn require_workspace_tiling_capability(
    supported: bool,
    operation: &str,
) -> Result<(), CosmicError> {
    if supported {
        return Ok(());
    }

    Err(CosmicError::CapabilityUnavailable {
        operation: operation.to_owned(),
        capability: "workspace tiling".to_owned(),
        detail: "the selected workspace does not advertise tiling mutations".to_owned(),
    })
}

#[allow(dead_code)]
pub(crate) fn workspace_supports_tiling(
    capabilities: zcosmic_workspace_handle_v2::WorkspaceCapabilities,
) -> bool {
    capabilities.contains(zcosmic_workspace_handle_v2::WorkspaceCapabilities::SetTilingState)
}

fn require_global<I: Proxy>(
    globals: &[Global],
    minimum_version: u32,
    operation: &str,
) -> Result<u32, CosmicError> {
    require_global_by_name(globals, I::interface().name, minimum_version, operation)
}

fn require_global_by_name(
    globals: &[Global],
    interface: &str,
    minimum_version: u32,
    operation: &str,
) -> Result<u32, CosmicError> {
    let advertised = globals
        .iter()
        .filter(|global| global.interface == interface)
        .map(|global| global.version)
        .max();
    let Some(version) = advertised else {
        return Err(CosmicError::RequiredGlobalMissing {
            operation: operation.to_owned(),
            global: interface.to_owned(),
        });
    };
    if version < minimum_version {
        return Err(CosmicError::ProtocolVersionUnsupported {
            operation: operation.to_owned(),
            protocol: interface.to_owned(),
            required: minimum_version,
            advertised: Some(version),
        });
    }
    Ok(version)
}

fn count_globals<I: Proxy>(globals: &[Global]) -> usize {
    globals
        .iter()
        .filter(|global| global.interface == I::interface().name)
        .count()
}

pub(crate) fn validate_registry_capabilities(
    registry: &RegistryState,
    operation: &str,
) -> Result<ReadCapabilities, CosmicError> {
    let globals = registry.globals().cloned().collect::<Vec<_>>();
    validate_read_capabilities(&globals, operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(interface: &str, version: u32) -> Global {
        Global {
            name: 1,
            interface: interface.to_owned(),
            version,
        }
    }

    fn complete_globals() -> Vec<Global> {
        vec![
            global("ext_workspace_manager_v1", 1),
            global("zcosmic_workspace_manager_v2", 2),
            global("ext_foreign_toplevel_list_v1", 1),
            global("zcosmic_toplevel_info_v1", 3),
            global("wl_output", 4),
            global("wl_seat", 9),
        ]
    }

    #[test]
    fn validates_read_globals_and_records_versions() {
        let result = validate_read_capabilities(&complete_globals(), "observe");
        assert!(result.is_ok(), "unexpected capability error: {result:?}");

        let capabilities = match result {
            Ok(value) => value,
            Err(_) => return,
        };

        assert_eq!(capabilities.workspace_manager_version, 1);
        assert_eq!(capabilities.cosmic_workspace_manager_version, 2);
        assert_eq!(capabilities.foreign_toplevel_list_version, 1);
        assert_eq!(capabilities.cosmic_toplevel_info_version, 3);
        assert!(capabilities.workspace_enumeration);
        assert!(capabilities.foreign_toplevel_enumeration);
        assert!(capabilities.cosmic_toplevel_information);
        assert_eq!(capabilities.output_version, 4);
        assert_eq!(capabilities.seat_version, 9);
        assert_eq!(capabilities.output_count, 1);
        assert_eq!(capabilities.seat_count, 1);
        assert!(capabilities.target_workspace_output_association);
    }

    #[test]
    fn reports_missing_required_global() {
        let mut globals = complete_globals();
        globals.retain(|global| global.interface != "zcosmic_toplevel_info_v1");

        let result = validate_read_capabilities(&globals, "observe");

        assert!(matches!(
            result,
            Err(CosmicError::RequiredGlobalMissing { global, .. })
                if global == "zcosmic_toplevel_info_v1"
        ));
    }

    #[test]
    fn reports_unsupported_required_version() {
        let mut globals = complete_globals();
        if let Some(global) = globals
            .iter_mut()
            .find(|global| global.interface == "zcosmic_toplevel_info_v1")
        {
            global.version = 2;
        }

        let result = validate_read_capabilities(&globals, "observe");

        assert!(matches!(
            result,
            Err(CosmicError::ProtocolVersionUnsupported {
                protocol,
                required: 3,
                advertised: Some(2),
                ..
            }) if protocol == "zcosmic_toplevel_info_v1"
        ));
    }

    #[test]
    fn reports_missing_mutation_global_separately() {
        let result = validate_mutation_capabilities(&complete_globals(), "move-window");

        assert!(matches!(
            result,
            Err(CosmicError::RequiredGlobalMissing { global, .. })
                if global == "zcosmic_toplevel_manager_v1"
        ));
    }

    #[test]
    fn decodes_management_capabilities_without_relying_on_unknown_values() {
        let values = vec![
            WEnum::Value(
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Close,
            ),
            WEnum::Value(
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Activate,
            ),
            WEnum::Value(
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::MoveToExtWorkspace,
            ),
            WEnum::Unknown(99),
        ];

        let capabilities = ManagementCapabilities::from_protocol(values);

        assert!(capabilities.global_available);
        assert!(capabilities.close);
        assert!(capabilities.activate);
        assert!(capabilities.move_to_external_workspace);
    }

    #[test]
    fn reports_the_requested_management_capability_in_errors() {
        let result = require_management_capability(
            ManagementCapabilities::with_global(),
            ManagementCapability::WindowClose,
            "close-window",
        );

        assert!(matches!(
            result,
            Err(CosmicError::CapabilityUnavailable {
                operation,
                capability,
                ..
            }) if operation == "close-window" && capability == "window close"
        ));
    }

    #[test]
    fn tracks_workspace_tiling_capability_as_a_scoped_mutation_check() {
        assert!(workspace_supports_tiling(
            zcosmic_workspace_handle_v2::WorkspaceCapabilities::SetTilingState
        ));
        assert!(require_workspace_tiling_capability(false, "set-tiling").is_err());
    }
}
