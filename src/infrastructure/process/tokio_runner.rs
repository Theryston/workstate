use std::{process::Stdio, sync::Arc};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use crate::{
    application::ports::{
        BackgroundProcess, BoxFuture, ProcessOutput, ProcessOutputChunk, ProcessOutputSink,
        ProcessRequest, ProcessRunner, ProcessStream,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::errors::{
    invalid_working_directory, missing_working_directory, output_read_error, output_sink_error,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct TokioProcessRunner;

impl TokioProcessRunner {
    fn command(request: &ProcessRequest, kill_on_drop: bool) -> Command {
        let mut command = Command::new(&request.program);
        command.args(&request.arguments);
        if let Some(directory) = &request.working_directory {
            command.current_dir(directory);
        }
        command.envs(request.environment.iter().map(|(key, value)| (key, value)));
        command.kill_on_drop(kill_on_drop);
        command
    }

    async fn validate_working_directory(request: &ProcessRequest) -> Result<()> {
        let Some(directory) = request.working_directory.as_deref() else {
            return Ok(());
        };
        let metadata = tokio::fs::metadata(directory)
            .await
            .map_err(|source| missing_working_directory(directory, source))?;
        if !metadata.is_dir() {
            return Err(invalid_working_directory(directory));
        }
        Ok(())
    }
}

impl ProcessRunner for TokioProcessRunner {
    fn run<'a>(&'a self, request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        Box::pin(async move {
            Self::validate_working_directory(&request).await?;
            let output = Self::command(&request, true)
                .stdin(Stdio::null())
                .output()
                .await
                .map_err(|source| {
                    WorkstateError::with_source(
                        ErrorCategory::Process,
                        format!("could not execute '{}'", request.program),
                        source,
                    )
                })?;
            Ok(ProcessOutput {
                status: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        })
    }

    fn run_with_output<'a>(
        &'a self,
        request: ProcessRequest,
        output: Arc<dyn ProcessOutputSink>,
    ) -> BoxFuture<'a, Result<ProcessOutput>> {
        Box::pin(async move {
            Self::validate_working_directory(&request).await?;
            let mut child = Self::command(&request, true)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|source| {
                    WorkstateError::with_source(
                        ErrorCategory::Process,
                        format!("could not execute '{}'", request.program),
                        source,
                    )
                })?;
            let Some(stdout) = child.stdout.take() else {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "the process did not expose stdout",
                ));
            };
            let Some(stderr) = child.stderr.take() else {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "the process did not expose stderr",
                ));
            };

            let stdout_future = collect_stream(stdout, ProcessStream::Stdout, Arc::clone(&output));
            let stderr_future = collect_stream(stderr, ProcessStream::Stderr, output);
            let (stdout, stderr, status) = tokio::join!(stdout_future, stderr_future, child.wait());
            let stdout = stdout?;
            let stderr = stderr?;
            let status = status.map_err(|source| {
                WorkstateError::with_source(
                    ErrorCategory::Process,
                    format!("could not wait for '{}'", request.program),
                    source,
                )
            })?;
            Ok(ProcessOutput {
                status: status.code(),
                stdout,
                stderr,
            })
        })
    }

    fn start_background<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        Box::pin(async move {
            Self::validate_working_directory(&request).await?;
            let child = Self::command(&request, false)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|source| {
                    WorkstateError::with_source(
                        ErrorCategory::Process,
                        format!("could not start '{}'", request.program),
                        source,
                    )
                })?;
            let Some(pid) = child.id() else {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    format!(
                        "process '{}' did not expose an operating system identity",
                        request.program
                    ),
                ));
            };
            drop(child);
            BackgroundProcess::new(format!("pid:{pid}"))
        })
    }

    fn stop_background<'a>(&'a self, process: BackgroundProcess) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some(pid) = process.identity.strip_prefix("pid:") else {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "background process identity is not a local process identity",
                ));
            };
            let output = Command::new("kill")
                .args(["-TERM", pid])
                .output()
                .await
                .map_err(|source| {
                    WorkstateError::with_source(
                        ErrorCategory::Process,
                        "could not request background process termination",
                        source,
                    )
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.to_ascii_lowercase().contains("no such process") {
                    return Ok(());
                }
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "the operating system rejected the background process termination request",
                )
                .with_context("pid", pid));
            }
            Ok(())
        })
    }
}

async fn collect_stream<R>(
    mut reader: R,
    stream: ProcessStream,
    output: Arc<dyn ProcessOutputSink>,
) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|source| output_read_error(stream, source))?;
        if read == 0 {
            break;
        }
        let bytes = buffer[..read].to_vec();
        captured.extend_from_slice(&bytes);
        output
            .emit(ProcessOutputChunk { stream, bytes })
            .await
            .map_err(|error| output_sink_error(stream, error))?;
    }
    Ok(captured)
}
