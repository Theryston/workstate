use crate::{
    application::ports::ProcessOutput,
    error::{ErrorCategory, WorkstateError},
};

pub(crate) fn malformed_data(detail: impl Into<String>) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        format!("tmux returned malformed data: {}", detail.into()),
    )
}

pub(crate) fn invalid_utf8(source: std::string::FromUtf8Error) -> WorkstateError {
    WorkstateError::with_source(
        ErrorCategory::Integration,
        "tmux returned output that is not valid UTF-8",
        source,
    )
}

pub(crate) fn command_failed(operation: &str, output: &ProcessOutput) -> WorkstateError {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let mut error = WorkstateError::new(
        ErrorCategory::Integration,
        format!("tmux operation '{operation}' failed"),
    )
    .with_context(
        "exit_status",
        output
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
    );
    if !detail.is_empty() {
        error = error.with_context("detail", detail);
    }
    error
}

pub(crate) fn operation_error(operation: &str, source: WorkstateError) -> WorkstateError {
    source
        .with_context("integration", "tmux")
        .with_context("operation", operation)
}

pub(crate) fn missing_target(output: &ProcessOutput) -> bool {
    let detail = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    [
        "no server running",
        "failed to connect to server",
        "can't find session",
        "can't find window",
        "no such session",
        "no such window",
    ]
    .iter()
    .any(|marker| detail.contains(marker))
}

pub(crate) fn no_server(output: &ProcessOutput) -> bool {
    let detail = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    detail.contains("no server running") || detail.contains("failed to connect to server")
}

pub(crate) fn readiness_timeout(session_name: &str, window_name: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "tmux did not expose the requested window before the readiness timeout",
    )
    .with_context("session_name", session_name.to_owned())
    .with_context("window_name", window_name.to_owned())
}
