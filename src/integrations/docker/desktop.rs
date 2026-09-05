use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::Mutex;

use crate::{
    application::{
        planner::CancellationToken,
        ports::{BackgroundProcess, DockerActionContext, ProcessRequest, ProcessRunner},
    },
    domain::{OwnershipStatus, ResourceIdentity, ResourceKind, ResourceRecord},
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone)]
pub struct DockerDesktopController {
    process_runner: Arc<dyn ProcessRunner>,
    executable: Option<PathBuf>,
    state: Arc<Mutex<DockerDesktopState>>,
}

#[derive(Default)]
struct DockerDesktopState {
    started_process: Option<BackgroundProcess>,
}

impl DockerDesktopController {
    pub fn new(process_runner: Arc<dyn ProcessRunner>, executable: Option<PathBuf>) -> Self {
        Self {
            process_runner,
            executable,
            state: Arc::new(Mutex::new(DockerDesktopState::default())),
        }
    }

    pub async fn ensure_started(
        &self,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        cancellation.check()?;
        let Some(executable) = &self.executable else {
            return Ok(None);
        };
        let executable_name = executable_name(executable)?;
        let mut state = self.state.lock().await;
        if let Some(process) = &state.started_process {
            return resource_for_process(
                context,
                process,
                &executable_name,
                OwnershipStatus::CreatedByCurrentRun,
            )
            .map(Some);
        }

        if self.is_running(&executable_name).await? {
            return resource_for_existing(context, &executable_name).map(Some);
        }

        cancellation.check()?;
        let process = self
            .process_runner
            .start_background(ProcessRequest {
                program: executable.to_string_lossy().into_owned(),
                arguments: Vec::new(),
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|error| {
                error
                    .with_context("operation", "start Docker Desktop")
                    .with_context("executable", executable.display().to_string())
            })?;
        state.started_process = Some(process.clone());
        resource_for_process(
            context,
            &process,
            &executable_name,
            OwnershipStatus::CreatedByCurrentRun,
        )
        .map(Some)
    }

    pub async fn observe_running(
        &self,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        cancellation.check()?;
        let Some(executable) = &self.executable else {
            return Ok(None);
        };
        let executable_name = executable_name(executable)?;
        let state = self.state.lock().await;
        if let Some(process) = &state.started_process {
            return resource_for_process(
                context,
                process,
                &executable_name,
                OwnershipStatus::CreatedByCurrentRun,
            )
            .map(Some);
        }
        drop(state);
        if self.is_running(&executable_name).await? {
            return resource_for_existing(context, &executable_name).map(Some);
        }
        Ok(None)
    }

    pub async fn stop_owned(
        &self,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<Vec<String>> {
        let mut outputs = Vec::new();
        let mut state = self.state.lock().await;
        for resource in resources.iter().filter(|resource| {
            resource.resource.kind == ResourceKind::DockerDesktop && resource.is_cleanup_candidate()
        }) {
            cancellation.check()?;
            let Some(identity) = resource.integration_metadata.get("process_identity") else {
                outputs.push(format!(
                    "preserved Docker Desktop resource '{}' because its process identity is unavailable",
                    resource.resource.stable_identity
                ));
                continue;
            };
            let process = BackgroundProcess::new(identity.clone())?;
            self.process_runner.stop_background(process.clone()).await?;
            if state
                .started_process
                .as_ref()
                .is_some_and(|started| started.identity == process.identity)
            {
                state.started_process = None;
            }
            outputs.push("stopped Docker Desktop started by Workstate".to_owned());
        }
        Ok(outputs)
    }

    async fn is_running(&self, executable_name: &str) -> Result<bool> {
        let output = self
            .process_runner
            .run(ProcessRequest {
                program: "pgrep".to_owned(),
                arguments: vec!["-x".to_owned(), executable_name.to_owned()],
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|error| {
                WorkstateError::new(
                    ErrorCategory::Integration,
                    "could not determine whether Docker Desktop is already running",
                )
                .with_context("operation", "inspect Docker Desktop process")
                .with_context("detail", error.render())
            })?;
        Ok(output.succeeded())
    }
}

fn executable_name(executable: &Path) -> Result<String> {
    executable
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Desktop executable must have a valid file name",
            )
        })
}

fn resource_for_process(
    context: &DockerActionContext,
    process: &BackgroundProcess,
    executable_name: &str,
    ownership: OwnershipStatus,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(
        ResourceKind::DockerDesktop,
        format!("desktop:{executable_name}"),
    )
    .map_err(WorkstateError::from)?;
    let mut record =
        ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("process_identity".to_owned(), process.identity.clone());
    record
        .integration_metadata
        .insert("environment".to_owned(), context.environment.to_string());
    Ok(record)
}

fn resource_for_existing(
    context: &DockerActionContext,
    executable_name: &str,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(
        ResourceKind::DockerDesktop,
        format!("desktop:{executable_name}"),
    )
    .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, OwnershipStatus::PreExisting)
        .with_action(context.action_id.clone());
    record.observed_before = true;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("process_name".to_owned(), executable_name.to_owned());
    Ok(record)
}
