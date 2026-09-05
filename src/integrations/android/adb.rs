use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::{
        planner::CancellationToken,
        ports::{AndroidDeviceSnapshot, ProcessOutput, ProcessRequest, ProcessRunner},
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{errors, models};

#[derive(Clone)]
pub struct AdbClient {
    runner: Arc<dyn ProcessRunner>,
    executable: PathBuf,
}

impl AdbClient {
    pub fn new(runner: Arc<dyn ProcessRunner>, executable: PathBuf) -> Result<Self> {
        validate_executable(&executable, "adb")?;
        Ok(Self { runner, executable })
    }

    pub fn executable(&self) -> &PathBuf {
        &self.executable
    }

    pub async fn list_devices(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AndroidDeviceSnapshot>> {
        let output = self
            .run(
                "list-devices",
                vec!["devices".to_owned(), "-l".to_owned()],
                cancellation,
            )
            .await?;
        models::parse_devices(&output.stdout)
    }

    pub async fn avd_name(
        &self,
        serial: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>> {
        validate_serial(serial)?;
        let output = self
            .run_allow_transient_failure(
                "get-avd-name",
                vec![
                    "-s".to_owned(),
                    serial.to_owned(),
                    "emu".to_owned(),
                    "avd".to_owned(),
                    "name".to_owned(),
                ],
                cancellation,
            )
            .await?;
        if let Some(output) = output {
            return models::parse_avd_name(&output.stdout);
        }
        Ok(None)
    }

    pub async fn boot_completed(
        &self,
        serial: &str,
        cancellation: &CancellationToken,
    ) -> Result<bool> {
        validate_serial(serial)?;
        let output = self
            .run_allow_transient_failure(
                "check-boot-readiness",
                vec![
                    "-s".to_owned(),
                    serial.to_owned(),
                    "shell".to_owned(),
                    "getprop".to_owned(),
                    "sys.boot_completed".to_owned(),
                ],
                cancellation,
            )
            .await?;
        Ok(output.is_some_and(|value| models::parse_boot_property(&value.stdout)))
    }

    pub async fn observe_emulators(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AndroidDeviceSnapshot>> {
        let devices = self.list_devices(cancellation).await?;
        let mut observed = Vec::with_capacity(devices.len());
        for mut device in devices {
            cancellation.check()?;
            device.avd = self.avd_name(&device.serial, cancellation).await?;
            device.boot_completed = device.state.is_connected()
                && self.boot_completed(&device.serial, cancellation).await?;
            observed.push(device);
        }
        Ok(observed)
    }

    async fn run(
        &self,
        operation: &str,
        arguments: Vec<String>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput> {
        cancellation.check()?;
        let output = self
            .runner
            .run(self.request(arguments))
            .await
            .map_err(|source| source.with_context("operation", operation))?;
        if output.succeeded() {
            return Ok(output);
        }
        Err(errors::command_failed(operation, &output))
    }

    async fn run_allow_transient_failure(
        &self,
        operation: &str,
        arguments: Vec<String>,
        cancellation: &CancellationToken,
    ) -> Result<Option<ProcessOutput>> {
        cancellation.check()?;
        let output = self
            .runner
            .run(self.request(arguments))
            .await
            .map_err(|source| source.with_context("operation", operation))?;
        if output.succeeded() {
            return Ok(Some(output));
        }
        if is_transient_failure(&output) {
            return Ok(None);
        }
        Err(errors::command_failed(operation, &output))
    }

    fn request(&self, arguments: Vec<String>) -> ProcessRequest {
        ProcessRequest {
            program: self.executable.to_string_lossy().into_owned(),
            arguments,
            working_directory: None,
            environment: Vec::new(),
        }
    }
}

fn validate_executable(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() || path.to_string_lossy().chars().any(char::is_control) {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            format!("{label} executable path must be non-empty and contain no control characters"),
        ));
    }
    Ok(())
}

fn validate_serial(serial: &str) -> Result<()> {
    if serial.is_empty() || serial.chars().any(char::is_control) {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Android emulator serial must be non-empty and contain no control characters",
        ));
    }
    Ok(())
}

fn is_transient_failure(output: &ProcessOutput) -> bool {
    let detail = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    [
        "offline",
        "unauthorized",
        "no permissions",
        "device not found",
        "device offline",
        "unknown device",
        "more than one device",
        "connection refused",
        "could not connect",
        "failed to connect",
        "error: closed",
    ]
    .iter()
    .any(|marker| detail.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::is_transient_failure;
    use crate::application::ports::ProcessOutput;

    #[test]
    fn connection_refused_during_shutdown_is_transient() {
        let output = ProcessOutput {
            status: Some(1),
            stdout: Vec::new(),
            stderr: b"error: could not connect to TCP port 5554: Connection refused".to_vec(),
        };

        assert!(is_transient_failure(&output));
    }
}
