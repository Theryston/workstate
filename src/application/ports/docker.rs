use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use crate::{
    application::{planner::CancellationToken, timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT},
    domain::{
        ActionId, CleanupPolicy, CommandSpec, ComposeSpec, ContainerSpec, EnvironmentSlug,
        ReadinessCheck, ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerOperationStatus {
    Healthy,
    Created,
    Repaired,
    Reused,
    Unchanged,
    Missing,
    Conflict,
    Failed,
    Unavailable,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerEngineSnapshot {
    pub ready: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
}

impl DockerEngineSnapshot {
    pub fn ready(version: impl Into<String>) -> Self {
        Self {
            ready: true,
            version: Some(version.into()),
            detail: None,
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            ready: false,
            version: None,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerActionContext {
    pub action_id: ActionId,
    pub environment: EnvironmentSlug,
    pub cleanup_policy: CleanupPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerEngineRequest {
    pub launch_desktop_when_needed: bool,
    pub timeout: Duration,
    pub poll_interval: Duration,
    pub action: DockerActionContext,
    pub environment: Vec<(String, String)>,
}

impl DockerEngineRequest {
    pub fn for_action(action: DockerActionContext) -> Self {
        Self {
            launch_desktop_when_needed: true,
            timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
            poll_interval: Duration::from_millis(100),
            action,
            environment: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerContainerRequest {
    pub context: DockerActionContext,
    pub specification: ContainerSpec,
    pub working_directory: Option<PathBuf>,
    pub readiness_checks: Vec<ReadinessCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerComposeRequest {
    pub context: DockerActionContext,
    pub specification: ComposeSpec,
    pub working_directory: PathBuf,
    pub readiness_checks: Vec<ReadinessCheck>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerContainerSnapshot {
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub command: Option<CommandSpec>,
    pub working_directory: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub mounts: Vec<DockerMountSnapshot>,
    pub ports: Vec<DockerPortSnapshot>,
    pub state: DockerContainerState,
    pub health: DockerHealthState,
    pub exit_code: Option<i32>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerMountSnapshot {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerPortSnapshot {
    pub host: u16,
    pub container: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Exited,
    Dead,
    Unknown(String),
}

impl DockerContainerState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerHealthState {
    Healthy,
    Unhealthy,
    Starting,
    None,
    Unknown(String),
}

impl DockerHealthState {
    pub fn satisfies_readiness(&self) -> bool {
        matches!(self, Self::Healthy | Self::None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerContainerObservation {
    Missing,
    Present(Box<DockerContainerSnapshot>),
    Unavailable(DockerEngineSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerComposeServiceSnapshot {
    pub name: String,
    pub container_id: Option<String>,
    pub state: DockerContainerState,
    pub health: DockerHealthState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerComposeSnapshot {
    pub project_name: String,
    pub working_directory: PathBuf,
    pub services: Vec<DockerComposeServiceSnapshot>,
}

impl DockerComposeSnapshot {
    pub fn is_healthy(&self, requested_services: &[String]) -> bool {
        if requested_services.is_empty() {
            return !self.services.is_empty()
                && self.services.iter().all(|service| {
                    service.state.is_running() && service.health.satisfies_readiness()
                });
        }

        requested_services.iter().all(|requested| {
            self.services.iter().any(|service| {
                service.name == *requested
                    && service.state.is_running()
                    && service.health.satisfies_readiness()
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerComposeObservation {
    Missing,
    Present(DockerComposeSnapshot),
    Unavailable(DockerEngineSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerEnsureOutcome {
    pub status: DockerOperationStatus,
    pub resources: Vec<ResourceRecord>,
    pub detail: Option<String>,
    pub outputs: Vec<String>,
}

impl DockerEnsureOutcome {
    pub fn new(status: DockerOperationStatus) -> Self {
        Self {
            status,
            resources: Vec::new(),
            detail: None,
            outputs: Vec::new(),
        }
    }

    pub fn with_resources(mut self, resources: Vec<ResourceRecord>) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.outputs.push(output.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerCleanupRequest {
    pub context: DockerActionContext,
    pub specification: Option<ContainerSpec>,
    pub compose: Option<DockerComposeRequest>,
    pub resources: Vec<ResourceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerCheckReport {
    pub checks_run: usize,
    pub last_detail: Option<String>,
}

pub trait DockerBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;

    fn inspect_engine<'a>(
        &'a self,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerEngineSnapshot>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Engine inspection is not configured",
            ))
        })
    }

    fn ensure_engine_ready<'a>(
        &'a self,
        _request: DockerEngineRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Engine readiness is not configured",
            ))
        })
    }

    fn observe_container<'a>(
        &'a self,
        _request: DockerContainerRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerContainerObservation>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker container observation is not configured",
            ))
        })
    }

    fn ensure_container<'a>(
        &'a self,
        _request: DockerContainerRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker container lifecycle is not configured",
            ))
        })
    }

    fn observe_compose<'a>(
        &'a self,
        _request: DockerComposeRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerComposeObservation>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Compose observation is not configured",
            ))
        })
    }

    fn ensure_compose<'a>(
        &'a self,
        _request: DockerComposeRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Compose lifecycle is not configured",
            ))
        })
    }

    fn check_readiness<'a>(
        &'a self,
        _request: DockerContainerRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker readiness checks are not configured",
            ))
        })
    }

    fn check_compose_readiness<'a>(
        &'a self,
        _request: DockerComposeRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker Compose readiness checks are not configured",
            ))
        })
    }

    fn stop_owned<'a>(
        &'a self,
        _request: DockerCleanupRequest,
        _cancellation: CancellationToken,
    ) -> crate::application::ports::BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker cleanup is not configured",
            ))
        })
    }
}
