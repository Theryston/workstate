#![allow(dead_code)]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use workstate::{
    application::ports::{BoxFuture, DesktopBackend, DesktopOperationOutcome, DesktopSnapshot},
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone)]
pub struct FakeDesktop {
    state: Arc<Mutex<DesktopSnapshot>>,
    calls: Arc<Mutex<Vec<String>>>,
    fail_next_moves: Arc<AtomicUsize>,
}

impl FakeDesktop {
    pub fn new(state: DesktopSnapshot) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            calls: Arc::new(Mutex::new(Vec::new())),
            fail_next_moves: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn state(&self) -> Result<DesktopSnapshot> {
        self.state.lock().map(|state| state.clone()).map_err(|_| {
            WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
        })
    }

    pub fn calls(&self) -> Result<Vec<String>> {
        self.calls.lock().map(|calls| calls.clone()).map_err(|_| {
            WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
        })
    }

    pub fn fail_next_move(&self) {
        self.fail_next_moves.fetch_add(1, Ordering::AcqRel);
    }

    pub fn add_window(
        &self,
        window: workstate::application::ports::DesktopWindowSnapshot,
    ) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| state.windows.push(window))
            .map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
            })
    }
}

impl DesktopBackend for FakeDesktop {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            state.lock().map(|state| state.clone()).map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
            })
        })
    }

    fn create_workspace<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let state = Arc::clone(&self.state);
        let calls = Arc::clone(&self.calls);
        let name = name.to_owned();
        Box::pin(async move {
            let identity = format!("fake-{}", name.to_ascii_lowercase().replace(' ', "-"));
            state
                .lock()
                .map(|mut state| {
                    let position = state.workspaces.len() as u32;
                    state.workspaces.push(
                        workstate::application::ports::DesktopWorkspaceSnapshot {
                            identity: identity.clone(),
                            name: Some(name.clone()),
                            position: Some(position),
                            focused: false,
                            tiling_enabled: Some(false),
                        },
                    );
                })
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
                })?;
            calls
                .lock()
                .map(|mut calls| calls.push(format!("create-workspace:{name}")))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
                })?;
            Ok(DesktopOperationOutcome::created(Some(identity)))
        })
    }

    fn delete_workspace<'a>(
        &'a self,
        workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let state = Arc::clone(&self.state);
        let calls = Arc::clone(&self.calls);
        let identity = workspace_identity.to_owned();
        Box::pin(async move {
            state
                .lock()
                .map(|mut state| {
                    state
                        .workspaces
                        .retain(|workspace| workspace.identity != identity)
                })
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
                })?;
            calls
                .lock()
                .map(|mut calls| calls.push(format!("delete-workspace:{identity}")))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
                })?;
            Ok(DesktopOperationOutcome::changed(Some(identity)))
        })
    }

    fn move_window<'a>(
        &'a self,
        window_identity: &'a str,
        workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let state = Arc::clone(&self.state);
        let calls = Arc::clone(&self.calls);
        let failures = Arc::clone(&self.fail_next_moves);
        let window = window_identity.to_owned();
        let workspace = workspace_identity.to_owned();
        Box::pin(async move {
            calls
                .lock()
                .map(|mut calls| calls.push(format!("move-window:{window}:{workspace}")))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
                })?;
            if failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    if value > 0 { Some(value - 1) } else { None }
                })
                .is_ok()
            {
                return Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "fake move failed",
                ));
            }
            let mut state = state.lock().map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
            })?;
            let Some(window_state) = state
                .windows
                .iter_mut()
                .find(|item| item.identity == window)
            else {
                return Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "fake window was not found",
                ));
            };
            window_state.workspace_identity = Some(workspace.clone());
            Ok(DesktopOperationOutcome::changed(Some(window)))
        })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let state = Arc::clone(&self.state);
        let calls = Arc::clone(&self.calls);
        let identity = window_identity.to_owned();
        Box::pin(async move {
            state
                .lock()
                .map(|mut state| state.windows.retain(|window| window.identity != identity))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
                })?;
            calls
                .lock()
                .map(|mut calls| calls.push(format!("close-window:{identity}")))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
                })?;
            Ok(DesktopOperationOutcome::changed(Some(identity)))
        })
    }

    fn set_tiling<'a>(
        &'a self,
        workspace_identity: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let state = Arc::clone(&self.state);
        let calls = Arc::clone(&self.calls);
        let identity = workspace_identity.to_owned();
        Box::pin(async move {
            let mut state = state.lock().map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake desktop state lock failed")
            })?;
            let Some(workspace) = state
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.identity == identity)
            else {
                return Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "fake workspace was not found",
                ));
            };
            workspace.tiling_enabled = Some(enabled);
            calls
                .lock()
                .map(|mut calls| calls.push(format!("set-tiling:{identity}:{enabled}")))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake desktop call lock failed")
                })?;
            Ok(DesktopOperationOutcome::changed(Some(identity)))
        })
    }
}
