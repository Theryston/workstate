use std::process::Stdio;

use tokio::process::Command;

use crate::{
    application::ports::{
        BackgroundProcess, BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalProcessRunner;

impl LocalProcessRunner {
    fn command(request: &ProcessRequest) -> Command {
        let mut command = Command::new(&request.program);
        command.args(&request.arguments);
        if let Some(directory) = &request.working_directory {
            command.current_dir(directory);
        }
        command.envs(request.environment.iter().map(|(key, value)| (key, value)));
        command
    }
}

impl ProcessRunner for LocalProcessRunner {
    fn run<'a>(&'a self, request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        Box::pin(async move {
            let output = Self::command(&request)
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

    fn start_background<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        Box::pin(async move {
            let child = Self::command(&request)
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
