use cosmic_client_toolkit::{
    delegate_toplevel_info, delegate_toplevel_manager, delegate_workspace,
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    workspace::{WorkspaceHandler, WorkspaceState},
};
use smithay_client_toolkit::{
    delegate_output, delegate_registry, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    seat::{Capability, SeatHandler, SeatState},
};
use wayland_client::{
    Connection, QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{wl_output, wl_seat},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1;

use super::super::errors::CosmicError;
use super::capabilities::{
    ManagementCapabilities, ReadCapabilities, validate_mutation_capabilities,
    validate_registry_capabilities,
};

pub(crate) struct CosmicReadState {
    pub(crate) registry_state: RegistryState,
    pub(crate) output_state: OutputState,
    pub(crate) seat_state: SeatState,
    pub(crate) workspace_state: WorkspaceState,
    pub(crate) toplevel_info_state: ToplevelInfoState,
    pub(crate) workspaces_done: bool,
    pub(crate) toplevels_done: bool,
    pub(crate) capabilities: ReadCapabilities,
    operation: String,
    protocol_error: Option<CosmicError>,
}

#[allow(dead_code)]
pub(crate) struct CosmicMutationState {
    pub(crate) read: CosmicReadState,
    pub(crate) toplevel_manager_state: ToplevelManagerState,
    pub(crate) management_capabilities: ManagementCapabilities,
}

impl CosmicReadState {
    pub(crate) fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        operation: &str,
    ) -> Result<Self, CosmicError> {
        let registry_state = RegistryState::new(globals);
        let capabilities = validate_registry_capabilities(&registry_state, operation)?;
        let output_state = OutputState::new(globals, qh);
        let seat_state = SeatState::new(globals, qh);
        let workspace_state = WorkspaceState::new(&registry_state, qh);
        let toplevel_info_state =
            ToplevelInfoState::try_new(&registry_state, qh).ok_or_else(|| {
                CosmicError::RequiredGlobalMissing {
                    operation: operation.to_owned(),
                    global: "zcosmic_toplevel_info_v1 or ext_foreign_toplevel_list_v1".to_owned(),
                }
            })?;

        Ok(Self {
            registry_state,
            output_state,
            seat_state,
            workspace_state,
            toplevel_info_state,
            workspaces_done: false,
            toplevels_done: false,
            capabilities,
            operation: operation.to_owned(),
            protocol_error: None,
        })
    }

    pub(crate) fn take_protocol_error(&mut self) -> Option<CosmicError> {
        self.protocol_error.take()
    }
}

#[allow(dead_code)]
impl CosmicMutationState {
    pub(crate) fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        operation: &str,
    ) -> Result<Self, CosmicError> {
        let registry_state = RegistryState::new(globals);
        let capabilities = validate_registry_capabilities(&registry_state, operation)?;
        let advertised_globals = registry_state.globals().cloned().collect::<Vec<_>>();
        validate_mutation_capabilities(&advertised_globals, operation)?;
        let output_state = OutputState::new(globals, qh);
        let seat_state = SeatState::new(globals, qh);
        let workspace_state = WorkspaceState::new(&registry_state, qh);
        let toplevel_info_state =
            ToplevelInfoState::try_new(&registry_state, qh).ok_or_else(|| {
                CosmicError::RequiredGlobalMissing {
                    operation: operation.to_owned(),
                    global: "zcosmic_toplevel_info_v1 or ext_foreign_toplevel_list_v1".to_owned(),
                }
            })?;
        let toplevel_manager_state = ToplevelManagerState::try_new(&registry_state, qh)
            .ok_or_else(|| CosmicError::RequiredGlobalMissing {
                operation: operation.to_owned(),
                global: "zcosmic_toplevel_manager_v1".to_owned(),
            })?;

        Ok(Self {
            read: CosmicReadState {
                registry_state,
                output_state,
                seat_state,
                workspace_state,
                toplevel_info_state,
                workspaces_done: false,
                toplevels_done: false,
                capabilities,
                operation: operation.to_owned(),
                protocol_error: None,
            },
            toplevel_manager_state,
            management_capabilities: ManagementCapabilities::with_global(),
        })
    }

    pub(crate) fn take_protocol_error(&mut self) -> Option<CosmicError> {
        self.read.take_protocol_error()
    }
}

trait ProtocolErrorSink {
    fn operation_name(&self) -> &str;
    fn protocol_error_mut(&mut self) -> &mut Option<CosmicError>;

    fn record_invalid_protocol_data(&mut self, detail: impl Into<String>) {
        if self.protocol_error_mut().is_none() {
            *self.protocol_error_mut() = Some(CosmicError::InvalidProtocolData {
                operation: self.operation_name().to_owned(),
                detail: detail.into(),
            });
        }
    }
}

impl ProtocolErrorSink for CosmicReadState {
    fn operation_name(&self) -> &str {
        &self.operation
    }

    fn protocol_error_mut(&mut self) -> &mut Option<CosmicError> {
        &mut self.protocol_error
    }
}

impl ProtocolErrorSink for CosmicMutationState {
    fn operation_name(&self) -> &str {
        &self.read.operation
    }

    fn protocol_error_mut(&mut self) -> &mut Option<CosmicError> {
        &mut self.read.protocol_error
    }
}

impl ProvidesRegistryState for CosmicReadState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    smithay_client_toolkit::registry_handlers!(OutputState, SeatState);
}

impl OutputHandler for CosmicReadState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for CosmicReadState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl WorkspaceHandler for CosmicReadState {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        let incomplete_workspace = self
            .workspace_state
            .workspaces()
            .any(|workspace| workspace.name.is_empty());
        if incomplete_workspace {
            self.record_invalid_protocol_data("a workspace completed without a name");
        }
        self.workspaces_done = true;
    }
}

impl ToplevelInfoHandler for CosmicReadState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if self.toplevel_info_state.info(toplevel).is_none() {
            self.record_invalid_protocol_data(
                "a new toplevel was not present in the completed list",
            );
        }
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if self.toplevel_info_state.info(toplevel).is_none() {
            self.record_invalid_protocol_data(
                "an updated toplevel was not present in the completed list",
            );
        }
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }

    fn info_done(&mut self, _: &Connection, _: &QueueHandle<Self>) {
        let incomplete_toplevel = self
            .toplevel_info_state
            .toplevels()
            .any(|toplevel| toplevel.identifier.is_empty());
        if incomplete_toplevel {
            self.record_invalid_protocol_data(
                "a toplevel completed without a stable protocol identifier",
            );
        }
        self.toplevels_done = true;
    }
}

impl ProvidesRegistryState for CosmicMutationState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.read.registry_state
    }

    smithay_client_toolkit::registry_handlers!(OutputState, SeatState);
}

impl OutputHandler for CosmicMutationState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.read.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for CosmicMutationState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.read.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl WorkspaceHandler for CosmicMutationState {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.read.workspace_state
    }

    fn done(&mut self) {
        let incomplete_workspace = self
            .read
            .workspace_state
            .workspaces()
            .any(|workspace| workspace.name.is_empty());
        if incomplete_workspace {
            self.record_invalid_protocol_data("a workspace completed without a name");
        }
        self.read.workspaces_done = true;
    }
}

impl ToplevelInfoHandler for CosmicMutationState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.read.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if self.read.toplevel_info_state.info(toplevel).is_none() {
            self.record_invalid_protocol_data(
                "a new toplevel was not present in the completed list",
            );
        }
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if self.read.toplevel_info_state.info(toplevel).is_none() {
            self.record_invalid_protocol_data(
                "an updated toplevel was not present in the completed list",
            );
        }
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }

    fn info_done(&mut self, _: &Connection, _: &QueueHandle<Self>) {
        let incomplete_toplevel = self
            .read
            .toplevel_info_state
            .toplevels()
            .any(|toplevel| toplevel.identifier.is_empty());
        if incomplete_toplevel {
            self.record_invalid_protocol_data(
                "a toplevel completed without a stable protocol identifier",
            );
        }
        self.read.toplevels_done = true;
    }
}

impl ToplevelManagerHandler for CosmicMutationState {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager_state
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        capabilities: Vec<
            WEnum<
                cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1,
            >,
        >,
    ) {
        self.management_capabilities = ManagementCapabilities::from_protocol(capabilities);
    }
}

delegate_output!(CosmicReadState);
delegate_registry!(CosmicReadState);
delegate_seat!(CosmicReadState);
delegate_workspace!(CosmicReadState);
delegate_toplevel_info!(CosmicReadState);

delegate_output!(CosmicMutationState);
delegate_registry!(CosmicMutationState);
delegate_seat!(CosmicMutationState);
delegate_workspace!(CosmicMutationState);
delegate_toplevel_info!(CosmicMutationState);
delegate_toplevel_manager!(CosmicMutationState);
