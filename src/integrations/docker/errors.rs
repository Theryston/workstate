use crate::{
    application::ports::ProcessOutput,
    error::{ErrorCategory, WorkstateError},
};

pub fn docker_error(operation: &str, output: &ProcessOutput) -> WorkstateError {
    let diagnostic = sanitized_output(&output.stderr)
        .or_else(|| sanitized_output(&output.stdout))
        .unwrap_or_else(|| "Docker returned no diagnostic output".to_owned());
    WorkstateError::new(
        ErrorCategory::Integration,
        format!("Docker operation '{operation}' failed: {diagnostic}"),
    )
    .with_context("operation", operation)
    .with_context(
        "exit_status",
        output
            .status
            .map_or_else(|| "unknown".to_owned(), |status| status.to_string()),
    )
}

pub fn sanitized_output(bytes: &[u8]) -> Option<String> {
    let output = String::from_utf8_lossy(bytes);
    let lines = output.lines().filter_map(sanitize_line).collect::<Vec<_>>();
    (!lines.is_empty()).then(|| lines.join(" "))
}

fn sanitize_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let lower = line.to_ascii_lowercase();
    if [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Some("[sensitive Docker diagnostic redacted]".to_owned());
    }
    Some(
        line.chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect(),
    )
}

pub fn missing_image(action_id: &str, container_name: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        format!(
            "container '{container_name}' is missing and action '{action_id}' does not define an image"
        ),
    )
    .with_context("action_id", action_id)
    .with_context("container_name", container_name)
}

pub fn conflict(container_name: &str, expected_key: &str, actual_key: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        format!(
            "Docker container '{container_name}' exists with incompatible configuration; it was preserved"
        ),
    )
    .with_context("container_name", container_name)
    .with_context("expected_configuration", expected_key)
    .with_context("actual_configuration", actual_key)
}

pub fn compose_configuration_error(message: impl Into<String>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Integration, message)
}

pub fn compose_failure(operation: &str, output: &ProcessOutput) -> WorkstateError {
    docker_error(operation, output)
}
