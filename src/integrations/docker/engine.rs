use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::sync::Mutex;

use crate::{
    application::{
        planner::CancellationToken,
        ports::{
            DockerEngineRequest, DockerEngineSnapshot, DockerEnsureOutcome, DockerOperationStatus,
            ProcessOutput, ProcessRequest, ProcessRunner,
        },
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::desktop::DockerDesktopController;

#[derive(Clone)]
pub struct DockerEngineController {
    process_runner: Arc<dyn ProcessRunner>,
    docker_program: PathBuf,
    desktop: Arc<DockerDesktopController>,
    lock: Arc<Mutex<()>>,
}

impl DockerEngineController {
    pub fn new(
        process_runner: Arc<dyn ProcessRunner>,
        docker_program: PathBuf,
        desktop: Arc<DockerDesktopController>,
    ) -> Result<Self> {
        if docker_program.as_os_str().is_empty() {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker executable must be non-empty",
            ));
        }
        Ok(Self {
            process_runner,
            docker_program,
            desktop,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn docker_program(&self) -> &Path {
        &self.docker_program
    }

    pub async fn inspect(&self, cancellation: CancellationToken) -> Result<DockerEngineSnapshot> {
        cancellation.check()?;
        let output = self
            .run(
                vec![
                    "info".to_owned(),
                    "--format".to_owned(),
                    "{{.ServerVersion}}".to_owned(),
                ],
                None,
            )
            .await?;
        if output.succeeded() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            return Ok(DockerEngineSnapshot::ready(if version.is_empty() {
                "unknown".to_owned()
            } else {
                version
            }));
        }
        Ok(DockerEngineSnapshot::unavailable(diagnostic_from_output(
            &output,
        )))
    }

    pub async fn ensure_ready(
        &self,
        request: DockerEngineRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        validate_wait_bounds(request.timeout, request.poll_interval)?;
        let _guard = self.lock.lock().await;
        let initial = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    "operation was cancelled while inspecting Docker Engine",
                ).with_context("cancelled", "true"));
            }
            result = tokio::time::timeout(request.timeout, self.inspect(cancellation.clone())) => {
                match result {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(engine_timeout_error(
                            &DockerEngineSnapshot::unavailable(
                                "Docker Engine inspection timed out",
                            ),
                            request.timeout,
                        ));
                    }
                }
            }
        };
        if initial.ready {
            let desktop_resource = self
                .desktop
                .observe_running(&request.action, cancellation.clone())
                .await?;
            let resources = desktop_resource.into_iter().collect::<Vec<_>>();
            return Ok(DockerEnsureOutcome::new(DockerOperationStatus::Reused)
                .with_resources(resources)
                .with_output(engine_output(&initial)));
        }
        if !request.launch_desktop_when_needed {
            return Err(engine_unavailable_error(&initial, false));
        }

        let desktop_resource = self
            .desktop
            .ensure_started(&request.action, cancellation.clone())
            .await?;
        if desktop_resource.is_none() {
            return Err(engine_unavailable_error(&initial, false));
        }
        let deadline = tokio::time::Instant::now() + request.timeout;
        let mut last = initial;
        loop {
            if let Err(error) = cancellation.check() {
                return Err(self
                    .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                    .await);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                let error = engine_timeout_error(&last, request.timeout);
                return Err(self
                    .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                    .await);
            }
            let remaining = deadline.saturating_duration_since(now);
            let inspected = tokio::select! {
                _ = cancellation.cancelled() => {
                    let error = WorkstateError::new(
                        ErrorCategory::Runtime,
                        "operation was cancelled while waiting for Docker Engine",
                    ).with_context("cancelled", "true");
                    return Err(self
                        .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                        .await);
                }
                result = tokio::time::timeout(remaining, self.inspect(cancellation.clone())) => result,
            };
            last = match inspected {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) => {
                    return Err(self
                        .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                        .await);
                }
                Err(_) => {
                    let error = engine_timeout_error(&last, request.timeout);
                    return Err(self
                        .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                        .await);
                }
            };
            if last.ready {
                let resources = desktop_resource.into_iter().collect::<Vec<_>>();
                let status = if resources.is_empty() {
                    DockerOperationStatus::Repaired
                } else {
                    DockerOperationStatus::Created
                };
                return Ok(DockerEnsureOutcome::new(status)
                    .with_resources(resources)
                    .with_output(engine_output(&last)));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                let error = engine_timeout_error(&last, request.timeout);
                return Err(self
                    .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                    .await);
            }
            let remaining = deadline.saturating_duration_since(now);
            tokio::select! {
                _ = cancellation.cancelled() => {
                    let error = WorkstateError::new(
                        ErrorCategory::Runtime,
                        "operation was cancelled while waiting for Docker Engine",
                    ).with_context("cancelled", "true");
                    return Err(self
                        .cleanup_desktop_after_failure(desktop_resource.as_ref(), error)
                        .await);
                }
                _ = tokio::time::sleep(request.poll_interval.min(remaining)) => {}
            }
        }
    }

    async fn cleanup_desktop_after_failure(
        &self,
        resource: Option<&crate::domain::ResourceRecord>,
        error: WorkstateError,
    ) -> WorkstateError {
        let Some(resource) = resource else {
            return error;
        };
        if !resource.is_cleanup_candidate() {
            return error;
        }
        match self
            .desktop
            .stop_owned(std::slice::from_ref(resource), CancellationToken::new())
            .await
        {
            Ok(_) => error.with_context("desktop_cleanup", "completed"),
            Err(cleanup_error) => {
                error.with_context("desktop_cleanup_error", cleanup_error.render())
            }
        }
    }

    pub async fn run(
        &self,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
    ) -> Result<ProcessOutput> {
        self.run_process(ProcessRequest {
            program: self.docker_program.to_string_lossy().into_owned(),
            arguments,
            working_directory,
            environment: Vec::new(),
        })
        .await
    }

    pub async fn run_process(&self, request: ProcessRequest) -> Result<ProcessOutput> {
        self.process_runner.run(request).await.map_err(|error| {
            error
                .with_context("operation", "execute Docker command")
                .with_context("program", self.docker_program.display().to_string())
        })
    }

    pub async fn stop_desktop(
        &self,
        resources: &[crate::domain::ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<Vec<String>> {
        self.desktop.stop_owned(resources, cancellation).await
    }
}

fn validate_wait_bounds(timeout: Duration, poll_interval: Duration) -> Result<()> {
    if timeout.is_zero() {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker Engine readiness timeout must be greater than zero",
        ));
    }
    if poll_interval.is_zero() {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker Engine readiness polling interval must be greater than zero",
        ));
    }
    Ok(())
}

fn diagnostic_from_output(output: &ProcessOutput) -> String {
    super::errors::sanitized_output(&output.stderr)
        .or_else(|| super::errors::sanitized_output(&output.stdout))
        .unwrap_or_else(|| "Docker Engine is not responding".to_owned())
}

fn engine_output(snapshot: &DockerEngineSnapshot) -> String {
    match &snapshot.version {
        Some(version) => format!("Docker Engine is ready ({version})"),
        None => "Docker Engine is ready".to_owned(),
    }
}

fn engine_unavailable_error(snapshot: &DockerEngineSnapshot, timed_out: bool) -> WorkstateError {
    let message = if timed_out {
        "Docker Engine did not become ready before the timeout"
    } else {
        "Docker Engine is unavailable and Docker Desktop launch is disabled"
    };
    WorkstateError::new(ErrorCategory::Integration, message)
        .with_context(
            "detail",
            snapshot
                .detail
                .clone()
                .unwrap_or_else(|| "no diagnostic was returned".to_owned()),
        )
        .with_context("next_action", "start Docker Engine and run Workstate again")
}

fn engine_timeout_error(snapshot: &DockerEngineSnapshot, timeout: Duration) -> WorkstateError {
    engine_unavailable_error(snapshot, true)
        .with_context("timeout_milliseconds", timeout.as_millis().to_string())
}
