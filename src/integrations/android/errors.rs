use thiserror::Error;

use crate::{
    application::ports::ProcessOutput,
    error::{ErrorCategory, WorkstateError},
};

#[derive(Debug, Error)]
pub enum AndroidError {
    #[error("Android tool '{tool}' is unavailable")]
    ToolUnavailable { tool: String },
    #[error("Android operation '{operation}' failed: {detail}")]
    CommandFailed { operation: String, detail: String },
    #[error("Android operation '{operation}' returned malformed data: {detail}")]
    MalformedOutput { operation: String, detail: String },
    #[error("Android Virtual Device '{avd}' is not available")]
    MissingAvd { avd: String },
    #[error("Android Virtual Device '{avd}' matched multiple emulator devices: {serials:?}")]
    AmbiguousAvd { avd: String, serials: Vec<String> },
    #[error("Android Virtual Device '{avd}' did not become ready before the timeout")]
    DeviceTimeout {
        avd: String,
        serial: Option<String>,
        last_state: String,
        boot_completed: bool,
    },
    #[error("Android Emulator window for '{avd}' did not become observable before the timeout")]
    WindowTimeout { avd: String, serial: String },
    #[error("Android Emulator window for '{avd}' is ambiguous")]
    AmbiguousWindow {
        avd: String,
        serial: String,
        matches: usize,
    },
    #[error("Android Emulator cleanup could not prove ownership")]
    OwnershipUnavailable { serial: String },
}

impl AndroidError {
    pub fn into_workstate(self) -> WorkstateError {
        WorkstateError::with_source(ErrorCategory::Integration, self.to_string(), self)
    }
}

pub fn command_failed(operation: &str, output: &ProcessOutput) -> WorkstateError {
    AndroidError::CommandFailed {
        operation: operation.to_owned(),
        detail: process_failure_detail(output),
    }
    .into_workstate()
}

pub fn malformed(operation: &str, detail: impl Into<String>) -> WorkstateError {
    AndroidError::MalformedOutput {
        operation: operation.to_owned(),
        detail: detail.into(),
    }
    .into_workstate()
}

pub fn process_failure_detail(output: &ProcessOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        return stdout;
    }
    output
        .exit_code()
        .map(|code| format!("process exited with status {code}"))
        .unwrap_or_else(|| "process terminated without an exit status".to_owned())
}

pub fn unavailable(tool: &str) -> WorkstateError {
    AndroidError::ToolUnavailable {
        tool: tool.to_owned(),
    }
    .into_workstate()
}
