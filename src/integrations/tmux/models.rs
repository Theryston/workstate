use crate::{
    domain::{ActionId, EnvironmentSlug},
    error::{ErrorCategory, Result, WorkstateError},
};

pub const SESSION_PREFIX: &str = "workstate-";
pub const WINDOW_PREFIX: &str = "workstate-";

pub fn session_name(environment: &EnvironmentSlug) -> String {
    format!("{SESSION_PREFIX}{environment}")
}

pub fn window_name(action_id: &ActionId) -> String {
    format!("{WINDOW_PREFIX}{action_id}")
}

pub fn validate_name(kind: &'static str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            format!("tmux {kind} name must be non-empty and contain no control characters"),
        )
        .with_context(kind, value.to_owned()));
    }
    Ok(())
}

pub fn validate_identity(kind: &'static str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains(':')
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            format!("tmux {kind} identity must be non-empty and contain no control characters"),
        )
        .with_context(kind, value.to_owned()));
    }
    Ok(())
}

pub fn validate_process(process: &crate::application::ports::ProcessRequest) -> Result<()> {
    if process.program.is_empty() || process.program.chars().any(char::is_control) {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "tmux process executable must be non-empty and contain no control characters",
        ));
    }
    if process
        .arguments
        .iter()
        .any(|argument| argument.contains('\0') || argument.chars().any(char::is_control))
    {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "tmux process arguments must not contain NUL characters",
        ));
    }
    if process.environment.iter().any(|(key, value)| {
        key.is_empty()
            || key.contains('=')
            || key.chars().any(char::is_control)
            || value.chars().any(char::is_control)
            || value.contains('\0')
    }) {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "tmux process environment entries are invalid",
        ));
    }
    if process
        .working_directory
        .as_ref()
        .is_some_and(|directory| !directory.is_absolute())
    {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "tmux process working directory must be absolute",
        ));
    }
    Ok(())
}
