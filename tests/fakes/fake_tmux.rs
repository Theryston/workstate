use std::sync::{Arc, Mutex};

use workstate::{
    application::ports::{
        BoxFuture, TerminalBackend, TmuxBackend, TmuxSessionSnapshot, TmuxWindowRequest,
        TmuxWindowSnapshot,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxCall {
    Observe,
    CreateSession {
        session_name: String,
        window_name: String,
    },
    CreateWindow {
        session_name: String,
        window_name: String,
    },
    KillWindow {
        session_name: String,
        window_identity: String,
    },
    KillSession {
        session_name: String,
    },
}

#[derive(Clone, Default)]
pub struct FakeTmux {
    state: Arc<Mutex<FakeTmuxState>>,
}

#[derive(Default)]
struct FakeTmuxState {
    sessions: Vec<TmuxSessionSnapshot>,
    calls: Vec<TmuxCall>,
    next_session: usize,
    next_window: usize,
}

impl FakeTmux {
    pub fn insert_session(&self, session: TmuxSessionSnapshot) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .map(|mut state| state.sessions.push(session))
    }

    pub fn sessions(&self) -> Result<Vec<TmuxSessionSnapshot>> {
        self.state
            .lock()
            .map(|state| state.sessions.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
    }

    pub fn calls(&self) -> Result<Vec<TmuxCall>> {
        self.state
            .lock()
            .map(|state| state.calls.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
    }

    fn create_window_snapshot(
        state: &mut FakeTmuxState,
        window: TmuxWindowRequest,
    ) -> TmuxWindowSnapshot {
        let identity = format!("@{}", state.next_window);
        state.next_window = state.next_window.saturating_add(1);
        TmuxWindowSnapshot {
            identity,
            name: window.name,
            command: Some(
                window
                    .process
                    .program
                    .rsplit('/')
                    .next()
                    .unwrap_or(&window.process.program)
                    .to_owned(),
            ),
            start_command: Some(window.process.program),
            working_directory: window.process.working_directory,
            process_id: Some(1_000_u32.saturating_add(state.next_window as u32)),
            is_dead: false,
        }
    }
}

impl TerminalBackend for FakeTmux {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }
}

impl TmuxBackend for FakeTmux {
    fn observe<'a>(&'a self) -> BoxFuture<'a, Result<Vec<TmuxSessionSnapshot>>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .map(|mut state| {
                state.calls.push(TmuxCall::Observe);
                state.sessions.clone()
            });
        Box::pin(async move { result })
    }

    fn create_session<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .and_then(|mut state| {
                state.calls.push(TmuxCall::CreateSession {
                    session_name: session_name.to_owned(),
                    window_name: window.name.clone(),
                });
                if state
                    .sessions
                    .iter()
                    .any(|session| session.name == session_name)
                {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "fake tmux session already exists",
                    ));
                }
                let identity = format!("session-{}", state.next_session);
                state.next_session = state.next_session.saturating_add(1);
                let window = Self::create_window_snapshot(&mut state, window);
                let session = TmuxSessionSnapshot {
                    identity,
                    name: session_name.to_owned(),
                    windows: vec![window],
                };
                state.sessions.push(session.clone());
                Ok(session)
            });
        Box::pin(async move { result })
    }

    fn create_window<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .and_then(|mut state| {
                state.calls.push(TmuxCall::CreateWindow {
                    session_name: session_name.to_owned(),
                    window_name: window.name.clone(),
                });
                let Some(index) = state
                    .sessions
                    .iter()
                    .position(|session| session.name == session_name)
                else {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "fake tmux session was not found",
                    ));
                };
                let window = Self::create_window_snapshot(&mut state, window);
                state.sessions[index].windows.push(window);
                Ok(state.sessions[index].clone())
            });
        Box::pin(async move { result })
    }

    fn kill_window<'a>(
        &'a self,
        session_name: &'a str,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .map(|mut state| {
                state.calls.push(TmuxCall::KillWindow {
                    session_name: session_name.to_owned(),
                    window_identity: window_identity.to_owned(),
                });
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.name == session_name)
                {
                    session
                        .windows
                        .retain(|window| window.identity != window_identity);
                }
            });
        Box::pin(async move { result })
    }

    fn kill_session<'a>(&'a self, session_name: &'a str) -> BoxFuture<'a, Result<()>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake tmux lock failed"))
            .map(|mut state| {
                state.calls.push(TmuxCall::KillSession {
                    session_name: session_name.to_owned(),
                });
                state
                    .sessions
                    .retain(|session| session.name != session_name);
            });
        Box::pin(async move { result })
    }
}
