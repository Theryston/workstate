use std::path::PathBuf;

use crate::{
    application::{
        planner::CancellationToken,
        ports::{BoxFuture, desktop::DesktopOperationOutcome},
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorWindowSnapshot {
    pub identity: String,
    pub application: String,
    pub title: Option<String>,
    pub project_path: Option<PathBuf>,
    pub workspace_identity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOperationStatus {
    Reused,
    Launched,
    Changed,
    Unchanged,
    Unavailable,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorOpenOutcome {
    pub status: EditorOperationStatus,
    pub window: EditorWindowSnapshot,
    pub owned: bool,
    pub process_identity: Option<String>,
}

pub trait EditorBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;

    fn observe_projects<'a>(&'a self) -> BoxFuture<'a, Result<Vec<EditorWindowSnapshot>>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "editor project observation is not configured",
            ))
        })
    }

    fn open_project<'a>(
        &'a self,
        _project_path: PathBuf,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<EditorOpenOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "editor project opening is not configured",
            ))
        })
    }

    fn close_window<'a>(
        &'a self,
        _window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "editor window closing is not configured",
            ))
        })
    }
}
