use thiserror::Error;

use crate::error::{ErrorCategory, WorkstateError};

#[derive(Debug, Error)]
pub enum CosmicError {
    #[error("COSMIC operation '{operation}' failed: {detail}")]
    CommandFailed { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' returned malformed data: {detail}")]
    MalformedOutput { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' returned incomplete data: {detail}")]
    IncompleteOutput { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' is unavailable: {detail}")]
    Unavailable { operation: String, detail: String },
}

impl CosmicError {
    pub fn into_workstate(self) -> WorkstateError {
        let message = self.to_string();
        WorkstateError::with_source(ErrorCategory::Integration, message, self)
    }
}
