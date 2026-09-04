use thiserror::Error;

use crate::error::{ErrorCategory, WorkstateError};

#[derive(Debug, Error)]
pub enum ZedError {
    #[error("Zed project path is invalid: {detail}")]
    InvalidProjectPath { detail: String },
    #[error("Zed operation '{operation}' failed: {detail}")]
    OperationFailed { operation: String, detail: String },
    #[error("Zed project '{project}' is ambiguous because {matches} matching windows were found")]
    AmbiguousProject { project: String, matches: usize },
    #[error("Zed project '{project}' did not become observable before the timeout")]
    WindowTimeout { project: String },
}

impl ZedError {
    pub fn into_workstate(self) -> WorkstateError {
        let message = self.to_string();
        WorkstateError::with_source(ErrorCategory::Integration, message, self)
    }
}
