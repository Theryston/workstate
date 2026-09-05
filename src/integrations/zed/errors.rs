use thiserror::Error;

use crate::error::{ErrorCategory, WorkstateError};

#[derive(Debug, Error)]
pub enum ZedError {
    #[error("{editor} project path is invalid: {detail}")]
    InvalidProjectPath { editor: String, detail: String },
    #[error("{editor} operation '{operation}' failed: {detail}")]
    OperationFailed {
        editor: String,
        operation: String,
        detail: String,
    },
    #[error(
        "{editor} project '{project}' is ambiguous because {matches} matching windows were found"
    )]
    AmbiguousProject {
        editor: String,
        project: String,
        matches: usize,
    },
    #[error("{editor} project '{project}' did not become observable before the timeout")]
    WindowTimeout { editor: String, project: String },
    #[error("{editor} window '{window}' did not close before the timeout")]
    WindowCloseTimeout { editor: String, window: String },
}

impl ZedError {
    pub fn into_workstate(self) -> WorkstateError {
        let message = self.to_string();
        WorkstateError::with_source(ErrorCategory::Integration, message, self)
    }
}
