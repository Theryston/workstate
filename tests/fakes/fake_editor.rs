#![allow(dead_code)]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use workstate::{
    application::planner::CancellationToken,
    application::ports::{
        BoxFuture, DesktopOperationOutcome, EditorBackend, EditorOpenOutcome,
        EditorOperationStatus, EditorWindowSnapshot,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone, Default)]
pub struct FakeEditor {
    windows: Arc<Mutex<Vec<EditorWindowSnapshot>>>,
    closed: Arc<Mutex<Vec<String>>>,
}

impl FakeEditor {
    pub fn with_windows(windows: Vec<EditorWindowSnapshot>) -> Self {
        Self {
            windows: Arc::new(Mutex::new(windows)),
            closed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn closed(&self) -> Result<Vec<String>> {
        self.closed
            .lock()
            .map(|closed| closed.clone())
            .map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake editor close lock failed")
            })
    }
}

impl EditorBackend for FakeEditor {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn observe_projects<'a>(&'a self) -> BoxFuture<'a, Result<Vec<EditorWindowSnapshot>>> {
        let windows = Arc::clone(&self.windows);
        Box::pin(async move {
            windows.lock().map(|windows| windows.clone()).map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake editor state lock failed")
            })
        })
    }

    fn open_project<'a>(
        &'a self,
        project_path: PathBuf,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<EditorOpenOutcome>> {
        let windows = Arc::clone(&self.windows);
        Box::pin(async move {
            cancellation.check()?;
            let mut windows = windows.lock().map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake editor state lock failed")
            })?;
            if let Some(window) = windows
                .iter()
                .find(|window| window.project_path.as_ref() == Some(&project_path))
                .cloned()
            {
                return Ok(EditorOpenOutcome {
                    status: EditorOperationStatus::Reused,
                    window,
                    owned: false,
                    process_identity: None,
                });
            }
            let window = EditorWindowSnapshot {
                identity: format!("fake-zed-{}", windows.len()),
                application: "dev.zed.Zed".to_owned(),
                title: Some(project_path.display().to_string()),
                project_path: Some(project_path),
                workspace_identity: None,
            };
            windows.push(window.clone());
            Ok(EditorOpenOutcome {
                status: EditorOperationStatus::Launched,
                window,
                owned: true,
                process_identity: Some("fake-process".to_owned()),
            })
        })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        let closed = Arc::clone(&self.closed);
        let identity = window_identity.to_owned();
        Box::pin(async move {
            closed
                .lock()
                .map(|mut closed| closed.push(identity.clone()))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake editor close lock failed")
                })?;
            Ok(DesktopOperationOutcome::changed(Some(identity)))
        })
    }
}

pub fn unavailable_editor_error() -> WorkstateError {
    WorkstateError::new(ErrorCategory::Integration, "fake editor is unavailable")
}
