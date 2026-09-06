use std::collections::{HashMap, HashSet};

use cosmic_client_toolkit::workspace::Workspace;
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
    workspace::v2::client::zcosmic_workspace_handle_v2,
};
use wayland_client::WEnum;
use wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1;

use crate::application::ports::{DesktopSnapshot, DesktopWindowSnapshot, DesktopWorkspaceSnapshot};

use super::super::errors::CosmicError;
use super::capabilities::{ReadCapability, require_read_capability};
use super::state::CosmicReadState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTilingState {
    Enabled,
    FloatingOnly,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeWorkspace {
    pub(crate) identity: Option<String>,
    pub(crate) name: String,
    pub(crate) position: Option<u32>,
    pub(crate) focused: bool,
    pub(crate) tiling: NativeTilingState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeWindow {
    pub(crate) identity: String,
    pub(crate) application: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) focused: bool,
    pub(crate) workspace_identities: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NativeSnapshot {
    pub(crate) workspaces: Vec<NativeWorkspace>,
    pub(crate) windows: Vec<NativeWindow>,
}

pub(crate) fn from_wayland_state(state: &CosmicReadState) -> Result<DesktopSnapshot, CosmicError> {
    let operation = "observe";
    for capability in [
        ReadCapability::WorkspaceEnumeration,
        ReadCapability::ForeignToplevelEnumeration,
        ReadCapability::CosmicToplevelInformation,
        ReadCapability::AvailableSeats,
        ReadCapability::TargetWorkspaceOutputAssociation,
    ] {
        require_read_capability(state.capabilities, capability, operation)?;
    }
    let mut workspace_identities = HashMap::new();
    let mut native_workspaces = Vec::new();

    for workspace in state.workspace_state.workspaces() {
        let identity = protocol_workspace_identity(workspace, operation)?;
        workspace_identities.insert(workspace.handle.clone(), identity.clone());
        native_workspaces.push(NativeWorkspace {
            identity: workspace.id.clone(),
            name: workspace.name.clone(),
            position: workspace.coordinates.first().copied(),
            focused: workspace
                .state
                .contains(ext_workspace_handle_v1::State::Active),
            tiling: protocol_tiling_state(workspace.tiling.as_ref()),
        });
    }

    let mut native_windows = Vec::new();
    for toplevel in state.toplevel_info_state.toplevels() {
        let workspace_identities = toplevel
            .workspace
            .iter()
            .map(|handle| {
                workspace_identities.get(handle).cloned().ok_or_else(|| {
                    invalid_protocol_data(
                        operation,
                        format!(
                            "window '{}' references a workspace that is absent from the workspace list",
                            toplevel.identifier
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        native_windows.push(NativeWindow {
            identity: toplevel.identifier.clone(),
            application: optional_protocol_text(
                Some(&toplevel.app_id),
                "application ID",
                operation,
            )?,
            title: optional_protocol_text(Some(&toplevel.title), "window title", operation)?,
            focused: toplevel
                .state
                .contains(&zcosmic_toplevel_handle_v1::State::Activated),
            workspace_identities,
        });
    }

    from_native_snapshot(NativeSnapshot {
        workspaces: native_workspaces,
        windows: native_windows,
    })
}

pub(crate) fn from_native_snapshot(native: NativeSnapshot) -> Result<DesktopSnapshot, CosmicError> {
    let operation = "observe";
    let mut workspace_ids = HashSet::new();
    let mut workspaces = Vec::with_capacity(native.workspaces.len());

    for workspace in native.workspaces {
        let name = required_protocol_text(&workspace.name, "workspace name", operation)?;
        let identity = match workspace.identity {
            Some(identity) => required_protocol_text(&identity, "workspace identity", operation)?,
            None => name.clone(),
        };
        if !workspace_ids.insert(identity.clone()) {
            return Err(invalid_protocol_data(
                operation,
                format!("duplicate workspace identity '{identity}'"),
            ));
        }
        workspaces.push(DesktopWorkspaceSnapshot {
            identity,
            name: Some(name),
            position: workspace.position,
            focused: workspace.focused,
            tiling_enabled: match workspace.tiling {
                NativeTilingState::Enabled => Some(true),
                NativeTilingState::FloatingOnly => Some(false),
                NativeTilingState::Unknown => None,
            },
        });
    }

    let mut window_ids = HashSet::new();
    let mut windows = Vec::with_capacity(native.windows.len());
    for window in native.windows {
        let identity = required_protocol_text(&window.identity, "window identity", operation)?;
        if !window_ids.insert(identity.clone()) {
            return Err(invalid_protocol_data(
                operation,
                format!("duplicate window identity '{identity}'"),
            ));
        }
        let application =
            optional_protocol_text(window.application.as_deref(), "application ID", operation)?;
        let title = optional_protocol_text(window.title.as_deref(), "window title", operation)?;
        let mut linked_workspaces = HashSet::new();
        for workspace_identity in window.workspace_identities {
            let workspace_identity = required_protocol_text(
                &workspace_identity,
                "window workspace identity",
                operation,
            )?;
            if !workspace_ids.contains(&workspace_identity) {
                return Err(invalid_protocol_data(
                    operation,
                    format!(
                        "window '{identity}' references unknown workspace '{workspace_identity}'"
                    ),
                ));
            }
            if !linked_workspaces.insert(workspace_identity.clone()) {
                return Err(invalid_protocol_data(
                    operation,
                    format!(
                        "window '{identity}' contains a duplicate workspace association '{workspace_identity}'"
                    ),
                ));
            }
        }
        let workspace_identity = if linked_workspaces.len() == 1 {
            linked_workspaces.into_iter().next()
        } else {
            None
        };
        windows.push(DesktopWindowSnapshot {
            identity,
            application,
            title,
            project_path: None,
            workspace_identity,
            focused: window.focused,
        });
    }

    Ok(DesktopSnapshot {
        workspaces,
        windows,
    })
}

fn protocol_workspace_identity(
    workspace: &Workspace,
    operation: &str,
) -> Result<String, CosmicError> {
    let name = required_protocol_text(&workspace.name, "workspace name", operation)?;
    match workspace.id.as_deref() {
        Some(identity) => required_protocol_text(identity, "workspace identity", operation),
        None => Ok(name),
    }
}

fn protocol_tiling_state(
    tiling: Option<&WEnum<zcosmic_workspace_handle_v2::TilingState>>,
) -> NativeTilingState {
    match tiling {
        Some(WEnum::Value(zcosmic_workspace_handle_v2::TilingState::TilingEnabled)) => {
            NativeTilingState::Enabled
        }
        Some(WEnum::Value(zcosmic_workspace_handle_v2::TilingState::FloatingOnly)) => {
            NativeTilingState::FloatingOnly
        }
        Some(WEnum::Value(_)) => NativeTilingState::Unknown,
        Some(WEnum::Unknown(_)) | None => NativeTilingState::Unknown,
    }
}

fn required_protocol_text(
    value: &str,
    field: &str,
    operation: &str,
) -> Result<String, CosmicError> {
    if value.is_empty() {
        return Err(invalid_protocol_data(
            operation,
            format!("{field} must not be empty"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_protocol_data(
            operation,
            format!("{field} contains a control character"),
        ));
    }
    Ok(value.to_owned())
}

fn optional_protocol_text(
    value: Option<&str>,
    field: &str,
    operation: &str,
) -> Result<Option<String>, CosmicError> {
    match value {
        None | Some("") => Ok(None),
        Some(value) => required_protocol_text(value, field, operation).map(Some),
    }
}

fn invalid_protocol_data(operation: &str, detail: impl Into<String>) -> CosmicError {
    CosmicError::InvalidProtocolData {
        operation: operation.to_owned(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(identity: Option<&str>, name: &str, tiling: NativeTilingState) -> NativeWorkspace {
        NativeWorkspace {
            identity: identity.map(str::to_owned),
            name: name.to_owned(),
            position: Some(2),
            focused: false,
            tiling,
        }
    }

    fn window(identity: &str, workspaces: &[&str]) -> NativeWindow {
        NativeWindow {
            identity: identity.to_owned(),
            application: Some("org.example.Editor".to_owned()),
            title: Some("Project".to_owned()),
            focused: true,
            workspace_identities: workspaces.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[allow(clippy::manual_unwrap_or_default)]
    fn snapshot(native: NativeSnapshot) -> DesktopSnapshot {
        let result = from_native_snapshot(native);
        assert!(result.is_ok(), "unexpected snapshot error: {result:?}");
        match result {
            Ok(value) => value,
            Err(_) => DesktopSnapshot::default(),
        }
    }

    fn error(native: NativeSnapshot) -> CosmicError {
        let result = from_native_snapshot(native);
        assert!(result.is_err(), "expected snapshot conversion to fail");
        match result {
            Err(error) => error,
            Ok(_) => invalid_protocol_data("observe", "test expected an invalid fixture"),
        }
    }

    #[test]
    fn prefers_protocol_workspace_id_and_preserves_name() {
        let value = snapshot(NativeSnapshot {
            workspaces: vec![workspace(
                Some("workspace-7"),
                "Main",
                NativeTilingState::Enabled,
            )],
            windows: Vec::new(),
        });

        assert_eq!(value.workspaces[0].identity, "workspace-7");
        assert_eq!(value.workspaces[0].name.as_deref(), Some("Main"));
    }

    #[test]
    fn falls_back_to_workspace_name_without_protocol_id() {
        let value = snapshot(NativeSnapshot {
            workspaces: vec![workspace(None, "Main", NativeTilingState::Unknown)],
            windows: Vec::new(),
        });

        assert_eq!(value.workspaces[0].identity, "Main");
    }

    #[test]
    fn rejects_duplicate_workspace_identity() {
        let value = error(NativeSnapshot {
            workspaces: vec![
                workspace(Some("same"), "Main", NativeTilingState::Unknown),
                workspace(Some("same"), "Other", NativeTilingState::Unknown),
            ],
            windows: Vec::new(),
        });

        assert!(
            matches!(value, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("duplicate workspace identity"))
        );
    }

    #[test]
    fn rejects_duplicate_window_identity() {
        let value = error(NativeSnapshot {
            workspaces: vec![workspace(Some("main"), "Main", NativeTilingState::Unknown)],
            windows: vec![window("same", &[]), window("same", &[])],
        });

        assert!(
            matches!(value, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("duplicate window identity"))
        );
    }

    #[test]
    fn converts_focus_position_and_project_defaults() {
        let mut focused_workspace = workspace(Some("main"), "Main", NativeTilingState::Unknown);
        focused_workspace.focused = true;
        let value = snapshot(NativeSnapshot {
            workspaces: vec![focused_workspace],
            windows: vec![window("window-1", &["main"])],
        });

        assert!(value.workspaces[0].focused);
        assert_eq!(value.workspaces[0].position, Some(2));
        assert!(value.windows[0].focused);
        assert_eq!(
            value.windows[0].application.as_deref(),
            Some("org.example.Editor")
        );
        assert_eq!(value.windows[0].title.as_deref(), Some("Project"));
        assert_eq!(value.windows[0].workspace_identity.as_deref(), Some("main"));
        assert_eq!(value.windows[0].project_path, None);
    }

    #[test]
    fn converts_enabled_disabled_and_unknown_tiling() {
        let value = snapshot(NativeSnapshot {
            workspaces: vec![
                workspace(Some("enabled"), "Enabled", NativeTilingState::Enabled),
                workspace(
                    Some("floating"),
                    "Floating",
                    NativeTilingState::FloatingOnly,
                ),
                workspace(Some("unknown"), "Unknown", NativeTilingState::Unknown),
            ],
            windows: Vec::new(),
        });

        assert_eq!(
            value
                .workspace("enabled")
                .and_then(|item| item.tiling_enabled),
            Some(true)
        );
        assert_eq!(
            value
                .workspace("floating")
                .and_then(|item| item.tiling_enabled),
            Some(false)
        );
        assert_eq!(
            value
                .workspace("unknown")
                .and_then(|item| item.tiling_enabled),
            None
        );
    }

    #[test]
    fn uses_none_for_zero_or_multiple_workspace_associations() {
        let value = snapshot(NativeSnapshot {
            workspaces: vec![
                workspace(Some("one"), "One", NativeTilingState::Unknown),
                workspace(Some("two"), "Two", NativeTilingState::Unknown),
            ],
            windows: vec![window("zero", &[]), window("multiple", &["one", "two"])],
        });

        assert_eq!(
            value
                .window("zero")
                .and_then(|item| item.workspace_identity.as_deref()),
            None
        );
        assert_eq!(
            value
                .window("multiple")
                .and_then(|item| item.workspace_identity.as_deref()),
            None
        );
    }

    #[test]
    fn rejects_missing_empty_and_control_character_identities() {
        let missing = error(NativeSnapshot {
            workspaces: Vec::new(),
            windows: vec![window("", &[])],
        });
        assert!(
            matches!(missing, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("window identity"))
        );

        let empty_workspace_identity = error(NativeSnapshot {
            workspaces: vec![workspace(Some(""), "Main", NativeTilingState::Unknown)],
            windows: Vec::new(),
        });
        assert!(
            matches!(empty_workspace_identity, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("workspace identity"))
        );

        let control = error(NativeSnapshot {
            workspaces: vec![workspace(
                Some("main\n"),
                "Main",
                NativeTilingState::Unknown,
            )],
            windows: Vec::new(),
        });
        assert!(
            matches!(control, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("control character"))
        );
    }

    #[test]
    fn rejects_malformed_names_and_inconsistent_workspace_references() {
        let empty_name = error(NativeSnapshot {
            workspaces: vec![workspace(Some("main"), "", NativeTilingState::Unknown)],
            windows: Vec::new(),
        });
        assert!(
            matches!(empty_name, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("workspace name"))
        );

        let unknown_reference = error(NativeSnapshot {
            workspaces: vec![workspace(Some("main"), "Main", NativeTilingState::Unknown)],
            windows: vec![window("window", &["missing"])],
        });
        assert!(
            matches!(unknown_reference, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("unknown workspace"))
        );
    }

    #[test]
    fn rejects_control_characters_in_optional_window_data() {
        let mut invalid_window = window("window", &[]);
        invalid_window.title = Some("Project\u{7}".to_owned());
        let value = error(NativeSnapshot {
            workspaces: Vec::new(),
            windows: vec![invalid_window],
        });

        assert!(
            matches!(value, CosmicError::InvalidProtocolData { detail, .. } if detail.contains("window title"))
        );
    }
}
