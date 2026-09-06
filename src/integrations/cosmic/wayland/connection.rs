use std::time::{Duration, Instant};

use polling::{Event, Events, Poller};
use wayland_client::{Connection, EventQueue, globals::registry_queue_init};

use super::super::errors::CosmicError;
use super::snapshot;
use super::state::CosmicReadState;

pub(crate) struct ReadSession {
    pub(crate) event_queue: EventQueue<CosmicReadState>,
    pub(crate) state: CosmicReadState,
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
}
