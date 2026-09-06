use std::time::{Duration, Instant};

use cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2;
use polling::{Event, Events, Poller};
use wayland_client::{Connection, EventQueue, globals::registry_queue_init};

use crate::application::ports::{DesktopOperationOutcome, DesktopSnapshot};

use super::super::errors::CosmicError;
use super::capabilities::{
    ManagementCapability, ReadCapability, require_external_workspace_move_capability,
    require_management_capability, require_management_protocol_version, require_read_capability,
    require_workspace_tiling_capability, workspace_supports_tiling,
};
use super::operations;
use super::snapshot;
use super::state::{CosmicMutationState, CosmicReadState};

pub(crate) struct ReadSession {
    pub(crate) event_queue: EventQueue<CosmicReadState>,
    pub(crate) state: CosmicReadState,
}

pub(crate) struct MutationSession {
    pub(crate) event_queue: EventQueue<CosmicMutationState>,
    pub(crate) state: CosmicMutationState,
}

pub(crate) fn observe(
    timeout: Duration,
) -> Result<crate::application::ports::DesktopSnapshot, CosmicError> {
    let operation = "observe";
    let timeout_ms = duration_millis(timeout);
    let deadline =
        Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| CosmicError::OperationTimedOut {
                operation: operation.to_owned(),
                timeout_ms,
            })?;
    let mut session = connect_read_session(operation)?;
    {
        let mut event_queue = WaylandEventQueue {
            event_queue: &mut session.event_queue,
        };
        synchronize(
            &mut session.state,
            &mut event_queue,
            |state| state.workspaces_done && state.toplevels_done,
            deadline,
            timeout_ms,
            operation,
        )?;
    }
    if let Some(error) = session.state.take_protocol_error() {
        return Err(error);
    }
    snapshot::from_wayland_state(&session.state)
}

fn connect_read_session(operation: &str) -> Result<ReadSession, CosmicError> {
    let connection =
        Connection::connect_to_env().map_err(|source| CosmicError::ConnectionFailed {
            operation: operation.to_owned(),
            detail: source.to_string(),
        })?;
    let (globals, event_queue) =
        registry_queue_init::<CosmicReadState>(&connection).map_err(|source| {
            CosmicError::InitialSynchronizationFailed {
                operation: operation.to_owned(),
                detail: source.to_string(),
            }
        })?;
    let qh = event_queue.handle();
    let state = CosmicReadState::new(&globals, &qh, operation)?;

    Ok(ReadSession { event_queue, state })
}

pub(crate) fn set_tiling(
    workspace_identity: String,
    enabled: bool,
    timeout: Duration,
) -> Result<DesktopOperationOutcome, CosmicError> {
    let operation = "set-tiling";
    let timeout_ms = duration_millis(timeout);
    let deadline = operation_deadline(timeout, operation, timeout_ms)?;
    let mut session = connect_mutation_session(operation)?;
    synchronize_mutation_session(&mut session, deadline, timeout_ms, operation)?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::ToplevelManagement,
        operation,
    )?;

    let (workspace_handle, current_tiling, supports_tiling) = {
        let workspace =
            operations::resolve_workspace(&session.state, &workspace_identity, operation)?;
        (
            workspace.cosmic_handle.clone(),
            operations::workspace_tiling_enabled(workspace),
            workspace_supports_tiling(workspace.cosmic_capabilities),
        )
    };
    require_workspace_tiling_capability(supports_tiling, operation)?;
    let Some(workspace_handle) = workspace_handle else {
        return Err(CosmicError::CapabilityUnavailable {
            operation: operation.to_owned(),
            capability: "COSMIC workspace handle".to_owned(),
            detail: "the selected workspace did not provide a native COSMIC handle".to_owned(),
        });
    };
    if current_tiling == Some(enabled) {
        return Ok(DesktopOperationOutcome::unchanged(Some(workspace_identity)));
    }

    let requested_state = if enabled {
        zcosmic_workspace_handle_v2::TilingState::TilingEnabled
    } else {
        zcosmic_workspace_handle_v2::TilingState::FloatingOnly
    };
    workspace_handle.set_tiling_state(requested_state);
    commit_workspace_request(&session.state, operation)?;
    flush_mutation_session(&mut session, operation)?;

    session.state.read.workspaces_done = false;
    let refreshed = refresh_snapshot(
        &mut session,
        deadline,
        timeout_ms,
        operation,
        workspace_refresh_complete,
        &workspace_identity,
    )?;
    confirm_tiling(&refreshed, &workspace_identity, enabled, operation)?;

    Ok(DesktopOperationOutcome::changed(Some(workspace_identity)))
}

pub(crate) fn move_window(
    window_identity: String,
    workspace_identity: String,
    timeout: Duration,
) -> Result<DesktopOperationOutcome, CosmicError> {
    let operation = "move-window";
    let timeout_ms = duration_millis(timeout);
    let deadline = operation_deadline(timeout, operation, timeout_ms)?;
    let mut session = connect_mutation_session(operation)?;
    synchronize_mutation_session(&mut session, deadline, timeout_ms, operation)?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::ToplevelManagement,
        operation,
    )?;

    let (window_handle, target_workspace_handle, target_output, already_in_target) = {
        let window = operations::resolve_window(&session.state, &window_identity, operation)?;
        let window_handle =
            window
                .cosmic_toplevel
                .clone()
                .ok_or_else(|| CosmicError::CapabilityUnavailable {
                    operation: operation.to_owned(),
                    capability: "COSMIC toplevel handle".to_owned(),
                    detail: format!("window '{window_identity}' did not provide a native handle"),
                })?;
        let workspace =
            operations::resolve_workspace(&session.state, &workspace_identity, operation)?;
        let target_workspace_handle = workspace.handle.clone();
        let target_output =
            operations::target_output(&session.state, workspace, operation, &workspace_identity)?;
        let already_in_target =
            window.workspace.len() == 1 && window.workspace.contains(&target_workspace_handle);
        (
            window_handle,
            target_workspace_handle,
            target_output,
            already_in_target,
        )
    };
    require_external_workspace_move_capability(
        session.state.management_capabilities,
        session.state.toplevel_manager_version,
        operation,
    )?;
    if already_in_target {
        return Ok(DesktopOperationOutcome::unchanged(Some(window_identity)));
    }

    session
        .state
        .toplevel_manager_state
        .manager
        .move_to_ext_workspace(&window_handle, &target_workspace_handle, &target_output);
    flush_mutation_session(&mut session, operation)?;

    session.state.read.toplevels_done = false;
    let refreshed = refresh_snapshot(
        &mut session,
        deadline,
        timeout_ms,
        operation,
        toplevel_refresh_complete,
        &window_identity,
    )?;
    confirm_window_placement(&refreshed, &window_identity, &workspace_identity, operation)?;

    Ok(DesktopOperationOutcome::changed(Some(window_identity)))
}

pub(crate) fn close_window(
    window_identity: String,
    timeout: Duration,
) -> Result<DesktopOperationOutcome, CosmicError> {
    let operation = "close-window";
    let timeout_ms = duration_millis(timeout);
    let deadline = operation_deadline(timeout, operation, timeout_ms)?;
    let mut session = connect_mutation_session(operation)?;
    synchronize_mutation_session(&mut session, deadline, timeout_ms, operation)?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::ToplevelManagement,
        operation,
    )?;
    let window_handle = operations::resolve_window(&session.state, &window_identity, operation)?
        .cosmic_toplevel
        .clone()
        .ok_or_else(|| CosmicError::CapabilityUnavailable {
            operation: operation.to_owned(),
            capability: "COSMIC toplevel handle".to_owned(),
            detail: format!("window '{window_identity}' did not provide a native handle"),
        })?;
    require_management_protocol_version(
        session.state.toplevel_manager_version,
        ManagementCapability::WindowClose,
        operation,
    )?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::WindowClose,
        operation,
    )?;
    session
        .state
        .toplevel_manager_state
        .manager
        .close(&window_handle);
    flush_mutation_session(&mut session, operation)?;
    Ok(DesktopOperationOutcome::changed(Some(window_identity)))
}

pub(crate) fn focus_window(
    window_identity: String,
    timeout: Duration,
) -> Result<DesktopOperationOutcome, CosmicError> {
    let operation = "focus-window";
    let timeout_ms = duration_millis(timeout);
    let deadline = operation_deadline(timeout, operation, timeout_ms)?;
    let mut session = connect_mutation_session(operation)?;
    synchronize_mutation_session(&mut session, deadline, timeout_ms, operation)?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::ToplevelManagement,
        operation,
    )?;
    let window_handle = operations::resolve_window(&session.state, &window_identity, operation)?
        .cosmic_toplevel
        .clone()
        .ok_or_else(|| CosmicError::CapabilityUnavailable {
            operation: operation.to_owned(),
            capability: "COSMIC toplevel handle".to_owned(),
            detail: format!("window '{window_identity}' did not provide a native handle"),
        })?;
    require_management_protocol_version(
        session.state.toplevel_manager_version,
        ManagementCapability::WindowActivation,
        operation,
    )?;
    require_management_capability(
        session.state.management_capabilities,
        ManagementCapability::WindowActivation,
        operation,
    )?;
    require_read_capability(
        session.state.read.capabilities,
        ReadCapability::AvailableSeats,
        operation,
    )?;
    let seat = session
        .state
        .read
        .seat_state
        .seats()
        .next()
        .ok_or_else(|| CosmicError::CapabilityUnavailable {
            operation: operation.to_owned(),
            capability: "available seats".to_owned(),
            detail: "the compositor did not advertise an input seat".to_owned(),
        })?;
    session
        .state
        .toplevel_manager_state
        .manager
        .activate(&window_handle, &seat);
    flush_mutation_session(&mut session, operation)?;
    Ok(DesktopOperationOutcome::changed(Some(window_identity)))
}

fn connect_mutation_session(operation: &str) -> Result<MutationSession, CosmicError> {
    let connection =
        Connection::connect_to_env().map_err(|source| CosmicError::ConnectionFailed {
            operation: operation.to_owned(),
            detail: source.to_string(),
        })?;
    let (globals, event_queue) =
        registry_queue_init::<CosmicMutationState>(&connection).map_err(|source| {
            CosmicError::InitialSynchronizationFailed {
                operation: operation.to_owned(),
                detail: source.to_string(),
            }
        })?;
    let qh = event_queue.handle();
    let state = CosmicMutationState::new(&globals, &qh, operation)?;
    Ok(MutationSession { event_queue, state })
}

fn synchronize_mutation_session(
    session: &mut MutationSession,
    deadline: Instant,
    timeout_ms: u64,
    operation: &str,
) -> Result<(), CosmicError> {
    {
        let mut event_queue = WaylandEventQueue {
            event_queue: &mut session.event_queue,
        };
        synchronize(
            &mut session.state,
            &mut event_queue,
            |state| {
                state.read.workspaces_done
                    && state.read.toplevels_done
                    && state.management_capabilities_received
            },
            deadline,
            timeout_ms,
            operation,
        )?;
    }
    if let Some(error) = session.state.take_protocol_error() {
        return Err(error);
    }
    Ok(())
}

fn flush_mutation_session(
    session: &mut MutationSession,
    operation: &str,
) -> Result<(), CosmicError> {
    let mut event_queue = WaylandEventQueue {
        event_queue: &mut session.event_queue,
    };
    event_queue.flush(operation)
}

fn commit_workspace_request(
    state: &CosmicMutationState,
    operation: &str,
) -> Result<(), CosmicError> {
    let manager = state
        .read
        .workspace_state
        .workspace_manager()
        .get()
        .map_err(|_| CosmicError::CapabilityUnavailable {
            operation: operation.to_owned(),
            capability: "workspace manager".to_owned(),
            detail: "the native workspace manager proxy is unavailable".to_owned(),
        })?;
    manager.commit();
    Ok(())
}

fn refresh_snapshot<F>(
    session: &mut MutationSession,
    deadline: Instant,
    timeout_ms: u64,
    operation: &str,
    completion: F,
    identity: &str,
) -> Result<DesktopSnapshot, CosmicError>
where
    F: Fn(&CosmicMutationState) -> bool,
{
    let synchronization = {
        let mut event_queue = WaylandEventQueue {
            event_queue: &mut session.event_queue,
        };
        synchronize(
            &mut session.state,
            &mut event_queue,
            completion,
            deadline,
            timeout_ms,
            operation,
        )
    };
    if let Err(error) = synchronization {
        return Err(map_confirmation_error(error, operation, identity));
    }
    if let Some(error) = session.state.take_protocol_error() {
        return Err(error);
    }
    snapshot::from_wayland_state_for_operation(&session.state.read, operation)
}

fn workspace_refresh_complete(state: &CosmicMutationState) -> bool {
    state.read.workspaces_done
}

fn toplevel_refresh_complete(state: &CosmicMutationState) -> bool {
    state.read.toplevels_done
}

fn map_confirmation_error(error: CosmicError, operation: &str, identity: &str) -> CosmicError {
    match error {
        CosmicError::InitialSynchronizationTimedOut { timeout_ms, .. }
        | CosmicError::OperationTimedOut { timeout_ms, .. } => CosmicError::MutationNotConfirmed {
            operation: operation.to_owned(),
            identity: identity.to_owned(),
            detail: format!(
                "the compositor did not publish the requested state within {timeout_ms} ms"
            ),
        },
        error => error,
    }
}

fn confirm_tiling(
    snapshot: &DesktopSnapshot,
    workspace_identity: &str,
    enabled: bool,
    operation: &str,
) -> Result<(), CosmicError> {
    let Some(workspace) = snapshot.workspace(workspace_identity) else {
        return Err(CosmicError::MutationNotConfirmed {
            operation: operation.to_owned(),
            identity: workspace_identity.to_owned(),
            detail: "the fresh COSMIC snapshot no longer contains the workspace".to_owned(),
        });
    };
    if workspace.tiling_enabled == Some(enabled) {
        return Ok(());
    }
    Err(CosmicError::MutationNotConfirmed {
        operation: operation.to_owned(),
        identity: workspace_identity.to_owned(),
        detail: format!(
            "the fresh COSMIC snapshot did not report tiling {}",
            if enabled { "enabled" } else { "disabled" }
        ),
    })
}

fn confirm_window_placement(
    snapshot: &DesktopSnapshot,
    window_identity: &str,
    workspace_identity: &str,
    operation: &str,
) -> Result<(), CosmicError> {
    let Some(window) = snapshot.window(window_identity) else {
        return Err(CosmicError::MutationNotConfirmed {
            operation: operation.to_owned(),
            identity: window_identity.to_owned(),
            detail: "the fresh COSMIC snapshot no longer contains the window".to_owned(),
        });
    };
    if window.workspace_identity.as_deref() == Some(workspace_identity) {
        return Ok(());
    }
    Err(CosmicError::MutationNotConfirmed {
        operation: operation.to_owned(),
        identity: window_identity.to_owned(),
        detail: format!(
            "the fresh COSMIC snapshot did not place the window in workspace '{workspace_identity}'"
        ),
    })
}

fn operation_deadline(
    timeout: Duration,
    operation: &str,
    timeout_ms: u64,
) -> Result<Instant, CosmicError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| CosmicError::OperationTimedOut {
            operation: operation.to_owned(),
            timeout_ms,
        })
}

trait NativeEventQueue<S> {
    fn dispatch_pending(&mut self, state: &mut S, operation: &str) -> Result<usize, CosmicError>;
    fn flush(&mut self, operation: &str) -> Result<(), CosmicError>;
    fn wait_for_events(
        &mut self,
        deadline: Instant,
        operation: &str,
    ) -> Result<WaitOutcome, CosmicError>;
}

struct WaylandEventQueue<'a, S> {
    event_queue: &'a mut EventQueue<S>,
}

impl<S> NativeEventQueue<S> for WaylandEventQueue<'_, S> {
    fn dispatch_pending(&mut self, state: &mut S, operation: &str) -> Result<usize, CosmicError> {
        self.event_queue.dispatch_pending(state).map_err(|source| {
            CosmicError::WaylandDispatchFailed {
                operation: operation.to_owned(),
                detail: source.to_string(),
            }
        })
    }

    fn flush(&mut self, operation: &str) -> Result<(), CosmicError> {
        self.event_queue
            .flush()
            .map_err(|source| CosmicError::WaylandFlushFailed {
                operation: operation.to_owned(),
                detail: source.to_string(),
            })
    }

    fn wait_for_events(
        &mut self,
        deadline: Instant,
        operation: &str,
    ) -> Result<WaitOutcome, CosmicError> {
        let Some(read_guard) = self.event_queue.prepare_read() else {
            return Ok(WaitOutcome::Readable);
        };
        let fd = read_guard.connection_fd();
        let poller = Poller::new().map_err(|source| CosmicError::WaylandDispatchFailed {
            operation: operation.to_owned(),
            detail: format!("could not create a Wayland poller: {source}"),
        })?;
        let mut events = Events::new();

        // The read guard owns the prepared read and keeps this descriptor valid until it is consumed.
        unsafe {
            poller.add(&fd, Event::readable(0)).map_err(|source| {
                CosmicError::WaylandDispatchFailed {
                    operation: operation.to_owned(),
                    detail: format!("could not watch the Wayland descriptor: {source}"),
                }
            })?;
        }
        let wait_result = poller.wait_deadline(&mut events, deadline);
        let delete_result = poller.delete(fd);
        let event_count = match (wait_result, delete_result) {
            (Ok(count), Ok(())) => count,
            (Err(wait_error), Ok(())) => {
                return Err(CosmicError::WaylandDispatchFailed {
                    operation: operation.to_owned(),
                    detail: format!("waiting for Wayland events failed: {wait_error}"),
                });
            }
            (Ok(_), Err(delete_error)) => {
                return Err(CosmicError::WaylandDispatchFailed {
                    operation: operation.to_owned(),
                    detail: format!(
                        "removing the Wayland descriptor from the poller failed: {delete_error}"
                    ),
                });
            }
            (Err(wait_error), Err(delete_error)) => {
                return Err(CosmicError::WaylandDispatchFailed {
                    operation: operation.to_owned(),
                    detail: format!(
                        "waiting for Wayland events failed: {wait_error}; removing the descriptor from the poller also failed: {delete_error}"
                    ),
                });
            }
        };
        if event_count == 0 {
            return Ok(WaitOutcome::NoEvent);
        }
        read_guard
            .read()
            .map_err(|source| CosmicError::WaylandDispatchFailed {
                operation: operation.to_owned(),
                detail: format!("reading Wayland events failed: {source}"),
            })?;
        Ok(WaitOutcome::Readable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Readable,
    NoEvent,
}

fn synchronize<S, Q, F>(
    state: &mut S,
    event_queue: &mut Q,
    completion: F,
    deadline: Instant,
    timeout_ms: u64,
    operation: &str,
) -> Result<(), CosmicError>
where
    Q: NativeEventQueue<S>,
    F: Fn(&S) -> bool,
{
    loop {
        event_queue.dispatch_pending(state, operation)?;
        if completion(state) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(timeout_error(operation, timeout_ms));
        }
        event_queue.flush(operation)?;
        match event_queue.wait_for_events(deadline, operation)? {
            WaitOutcome::Readable => {}
            WaitOutcome::NoEvent if Instant::now() >= deadline => {
                return Err(timeout_error(operation, timeout_ms));
            }
            WaitOutcome::NoEvent => {}
        }
    }
}

fn timeout_error(operation: &str, timeout_ms: u64) -> CosmicError {
    CosmicError::InitialSynchronizationTimedOut {
        operation: operation.to_owned(),
        timeout_ms,
    }
}

#[allow(clippy::manual_unwrap_or, clippy::map_or_identity)]
fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .ok()
        .map_or(u64::MAX, |value| value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct SyncState {
        workspaces_done: bool,
        toplevels_done: bool,
    }

    enum DispatchStep {
        WorkspacesDone,
        ToplevelsDone,
        Noop,
        Error(CosmicError),
    }

    struct FakeEventQueue {
        dispatch_steps: VecDeque<DispatchStep>,
        flush_error: Option<CosmicError>,
        wait_steps: VecDeque<Result<WaitOutcome, CosmicError>>,
    }

    impl NativeEventQueue<SyncState> for FakeEventQueue {
        fn dispatch_pending(
            &mut self,
            state: &mut SyncState,
            _: &str,
        ) -> Result<usize, CosmicError> {
            let step = match self.dispatch_steps.pop_front() {
                Some(step) => step,
                None => DispatchStep::Noop,
            };
            match step {
                DispatchStep::WorkspacesDone => {
                    state.workspaces_done = true;
                    Ok(1)
                }
                DispatchStep::ToplevelsDone => {
                    state.toplevels_done = true;
                    Ok(1)
                }
                DispatchStep::Noop => Ok(0),
                DispatchStep::Error(error) => Err(error),
            }
        }

        fn flush(&mut self, _: &str) -> Result<(), CosmicError> {
            match self.flush_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn wait_for_events(&mut self, _: Instant, _: &str) -> Result<WaitOutcome, CosmicError> {
            match self.wait_steps.pop_front() {
                Some(result) => result,
                None => Ok(WaitOutcome::NoEvent),
            }
        }
    }

    fn synchronization(
        queue: &mut FakeEventQueue,
        state: &mut SyncState,
        deadline: Instant,
    ) -> Result<(), CosmicError> {
        synchronize(
            state,
            queue,
            |state| state.workspaces_done && state.toplevels_done,
            deadline,
            100,
            "test-observe",
        )
    }

    #[test]
    fn initial_synchronization_requires_both_done_markers() {
        let mut state = SyncState::default();
        let mut queue = FakeEventQueue {
            dispatch_steps: VecDeque::from([
                DispatchStep::WorkspacesDone,
                DispatchStep::ToplevelsDone,
            ]),
            flush_error: None,
            wait_steps: VecDeque::from([Ok(WaitOutcome::Readable)]),
        };

        let result = synchronization(
            &mut queue,
            &mut state,
            Instant::now() + Duration::from_secs(1),
        );

        assert!(result.is_ok());
        assert!(state.workspaces_done);
        assert!(state.toplevels_done);
    }

    #[test]
    fn initial_synchronization_times_out_without_completion() {
        let mut state = SyncState::default();
        let mut queue = FakeEventQueue {
            dispatch_steps: VecDeque::from([DispatchStep::Noop]),
            flush_error: None,
            wait_steps: VecDeque::new(),
        };

        let result = synchronization(&mut queue, &mut state, Instant::now());

        assert!(matches!(
            result,
            Err(CosmicError::InitialSynchronizationTimedOut {
                operation,
                timeout_ms: 100
            }) if operation == "test-observe"
        ));
    }

    #[test]
    fn dispatch_failures_are_returned_as_typed_errors() {
        let mut state = SyncState::default();
        let mut queue = FakeEventQueue {
            dispatch_steps: VecDeque::from([DispatchStep::Error(
                CosmicError::WaylandDispatchFailed {
                    operation: "test-observe".to_owned(),
                    detail: "dispatch failed".to_owned(),
                },
            )]),
            flush_error: None,
            wait_steps: VecDeque::new(),
        };

        let result = synchronization(
            &mut queue,
            &mut state,
            Instant::now() + Duration::from_secs(1),
        );

        assert!(matches!(
            result,
            Err(CosmicError::WaylandDispatchFailed { detail, .. })
                if detail == "dispatch failed"
        ));
    }

    #[test]
    fn flush_failures_are_returned_as_typed_errors() {
        let mut state = SyncState::default();
        let mut queue = FakeEventQueue {
            dispatch_steps: VecDeque::from([DispatchStep::Noop]),
            flush_error: Some(CosmicError::WaylandFlushFailed {
                operation: "test-observe".to_owned(),
                detail: "flush failed".to_owned(),
            }),
            wait_steps: VecDeque::new(),
        };

        let result = synchronization(
            &mut queue,
            &mut state,
            Instant::now() + Duration::from_secs(1),
        );

        assert!(matches!(
            result,
            Err(CosmicError::WaylandFlushFailed { detail, .. })
                if detail == "flush failed"
        ));
    }

    #[test]
    fn tiling_postcondition_requires_the_requested_state() {
        let snapshot = DesktopSnapshot {
            workspaces: vec![crate::application::ports::DesktopWorkspaceSnapshot {
                identity: "main".to_owned(),
                name: Some("Main".to_owned()),
                position: Some(0),
                focused: true,
                tiling_enabled: Some(false),
            }],
            windows: Vec::new(),
        };

        let result = confirm_tiling(&snapshot, "main", true, "set-tiling");

        assert!(matches!(
            result,
            Err(CosmicError::MutationNotConfirmed { operation, identity, detail })
                if operation == "set-tiling"
                    && identity == "main"
                    && detail.contains("did not report tiling enabled")
        ));
    }

    #[test]
    fn window_placement_postcondition_rejects_a_different_workspace() {
        let snapshot = DesktopSnapshot {
            workspaces: Vec::new(),
            windows: vec![crate::application::ports::DesktopWindowSnapshot {
                identity: "window-1".to_owned(),
                application: None,
                title: None,
                project_path: None,
                workspace_identity: Some("other".to_owned()),
                focused: false,
            }],
        };

        let result = confirm_window_placement(&snapshot, "window-1", "target", "move-window");

        assert!(matches!(
            result,
            Err(CosmicError::MutationNotConfirmed { operation, identity, detail })
                if operation == "move-window"
                    && identity == "window-1"
                    && detail.contains("workspace 'target'")
        ));
    }

    #[test]
    fn confirmation_timeout_is_not_reported_as_a_changed_operation() {
        let result = map_confirmation_error(
            CosmicError::InitialSynchronizationTimedOut {
                operation: "move-window".to_owned(),
                timeout_ms: 25,
            },
            "move-window",
            "window-1",
        );

        assert!(matches!(
            result,
            CosmicError::MutationNotConfirmed { operation, identity, detail }
                if operation == "move-window"
                    && identity == "window-1"
                    && detail.contains("25 ms")
        ));
    }
}
