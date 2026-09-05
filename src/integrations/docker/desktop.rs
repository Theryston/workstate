use std::{
    collections::BTreeSet,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserServiceStatus {
    Active,
    Starting,
    Installed,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemServiceStatus {
    Active,
    Starting,
    Inactive,
}

#[derive(Clone)]
pub struct DockerDesktopController {
    process_runner: Arc<dyn ProcessRunner>,
    executable: Option<PathBuf>,
    linux_user_services: bool,
    state: Arc<Mutex<DockerDesktopState>>,
}

#[derive(Default)]
struct DockerDesktopState {
    started_process: Option<BackgroundProcess>,
}

impl DockerDesktopController {
    pub fn new(process_runner: Arc<dyn ProcessRunner>, executable: Option<PathBuf>) -> Self {
        Self::new_with_platform(process_runner, executable, cfg!(target_os = "linux"))
    }

    pub fn new_for_platform(
        process_runner: Arc<dyn ProcessRunner>,
        linux_user_services: bool,
    ) -> Self {
        Self::new_with_platform(process_runner, None, linux_user_services)
    }

    pub fn new_with_platform(
        process_runner: Arc<dyn ProcessRunner>,
        executable: Option<PathBuf>,
        linux_user_services: bool,
    ) -> Self {
        Self {
            process_runner,
            executable,
            linux_user_services,
            state: Arc::new(Mutex::new(DockerDesktopState::default())),
        }
    }

    pub async fn ensure_started(
        &self,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        if !self.linux_user_services {
            return self.ensure_process_started(context, cancellation).await;
        }
        self.ensure_service_started(
            "docker-desktop",
            ResourceKind::DockerDesktop,
            context,
            cancellation,
        )
        .await
    }

    pub async fn ensure_rootless_started(
        &self,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        let status = self
            .inspect_user_service("docker", cancellation.clone())
            .await?;
        self.ensure_rootless_started_with_status(context, status, cancellation)
            .await
    }

    pub async fn ensure_rootless_started_with_status(
        &self,
        context: &DockerActionContext,
        status: UserServiceStatus,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        self.ensure_service_started_with_status(
            "docker",
            ResourceKind::DockerEngine,
            context,
            status,
            cancellation,
        )
        .await
    }

    pub async fn observe_running(
        &self,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        cancellation.check()?;
        if !self.linux_user_services {
            return self.observe_process(context).await;
        }
        match self
            .inspect_user_service("docker-desktop", cancellation)
            .await?
        {
            UserServiceStatus::Active => resource_for_service(
                context,
                "docker-desktop",
                ResourceKind::DockerDesktop,
                OwnershipStatus::PreExisting,
            )
            .map(Some),
            UserServiceStatus::Starting
            | UserServiceStatus::Installed
            | UserServiceStatus::NotInstalled => Ok(None),
        }
    }

    pub async fn inspect_user_service(
        &self,
        service: &str,
        cancellation: CancellationToken,
    ) -> Result<UserServiceStatus> {
        validate_service_name(service)?;
        cancellation.check()?;
        if !self.linux_user_services {
            return Ok(UserServiceStatus::NotInstalled);
        }

        let load_output = self
            .run_systemctl(
                vec![
                    "--user".to_owned(),
                    "show".to_owned(),
                    service.to_owned(),
                    "--property=LoadState".to_owned(),
                    "--value".to_owned(),
                ],
                cancellation.clone(),
            )
            .await?;
        if !load_output.succeeded() {
            if is_service_not_found(&load_output) {
                return Ok(UserServiceStatus::NotInstalled);
            }
            return Err(service_operation_error(
                "inspect Docker user service",
                service,
                &load_output,
            ));
        }

        let load_state = String::from_utf8_lossy(&load_output.stdout)
            .trim()
            .to_ascii_lowercase();
        if load_state != "loaded" {
            if load_state.is_empty() || load_state == "not-found" {
                return Ok(UserServiceStatus::NotInstalled);
            }
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("Docker user service '{service}' has an unsupported state '{load_state}'"),
            )
            .with_context("service", service)
            .with_context("load_state", load_state));
        }

        let active_output = self
            .run_systemctl(
                vec![
                    "--user".to_owned(),
                    "is-active".to_owned(),
                    service.to_owned(),
                ],
                cancellation,
            )
            .await?;
        if active_output.succeeded()
            && String::from_utf8_lossy(&active_output.stdout)
                .trim()
                .eq_ignore_ascii_case("active")
        {
            return Ok(UserServiceStatus::Active);
        }
        if is_service_starting(&active_output) {
            return Ok(UserServiceStatus::Starting);
        }
        if is_service_not_found(&active_output) {
            return Ok(UserServiceStatus::NotInstalled);
        }
        Ok(UserServiceStatus::Installed)
    }

    pub async fn inspect_system_service(
        &self,
        service: &str,
        cancellation: CancellationToken,
    ) -> Result<SystemServiceStatus> {
        validate_service_name(service)?;
        cancellation.check()?;
        if !self.linux_user_services {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "system service inspection is not supported on this platform",
            ));
        }
        let output = self
            .run_systemctl(
                vec!["is-active".to_owned(), service.to_owned()],
                cancellation,
            )
            .await?;
        if output.succeeded()
            && String::from_utf8_lossy(&output.stdout)
                .trim()
                .eq_ignore_ascii_case("active")
        {
            Ok(SystemServiceStatus::Active)
        } else if is_service_starting(&output) {
            Ok(SystemServiceStatus::Starting)
        } else if is_inactive_service(&output) {
            Ok(SystemServiceStatus::Inactive)
        } else {
            Err(service_operation_error(
                "inspect Docker system service",
                service,
                &output,
            ))
        }
    }

    pub async fn start_user_service(
        &self,
        service: &str,
        cancellation: CancellationToken,
    ) -> Result<()> {
        validate_service_name(service)?;
        cancellation.check()?;
        if !self.linux_user_services {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker user services are not supported on this platform",
            ));
        }
        let output = self
            .run_systemctl(
                vec!["--user".to_owned(), "start".to_owned(), service.to_owned()],
                cancellation,
            )
            .await?;
        if output.succeeded() {
            return Ok(());
        }
        Err(service_operation_error(
            "start Docker user service",
            service,
            &output,
        ))
    }

    async fn ensure_service_started(
        &self,
        service: &str,
        kind: ResourceKind,
        context: &DockerActionContext,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        cancellation.check()?;
        if !self.linux_user_services {
            return Ok(None);
        }
        let status = self
            .inspect_user_service(service, cancellation.clone())
            .await?;
        self.ensure_service_started_with_status(service, kind, context, status, cancellation)
            .await
    }

    async fn ensure_service_started_with_status(
        &self,
        service: &str,
        kind: ResourceKind,
        context: &DockerActionContext,
        status: UserServiceStatus,
        cancellation: CancellationToken,
    ) -> Result<Option<ResourceRecord>> {
        cancellation.check()?;
        if !self.linux_user_services {
            return Ok(None);
        }
        match status {
            UserServiceStatus::Active | UserServiceStatus::Starting => {
                resource_for_service(context, service, kind, OwnershipStatus::PreExisting).map(Some)
            }
            UserServiceStatus::NotInstalled => Ok(None),
            UserServiceStatus::Installed => {
                self.start_user_service(service, cancellation).await?;
                resource_for_service(context, service, kind, OwnershipStatus::CreatedByCurrentRun)
                    .map(Some)
            }
        }
    }

    async fn ensure_process_started(
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

    async fn observe_process(
        &self,
        context: &DockerActionContext,
    ) -> Result<Option<ResourceRecord>> {
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

    pub async fn stop_owned(
        &self,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<Vec<String>> {
        let mut outputs = Vec::new();
        let mut seen = BTreeSet::new();
        for resource in resources.iter().filter(|resource| {
            resource.is_cleanup_candidate()
                && matches!(
                    resource.resource.kind,
                    ResourceKind::DockerDesktop | ResourceKind::DockerEngine
                )
        }) {
            if !seen.insert(resource.resource.clone()) {
                continue;
            }
            cancellation.check()?;
            if let Some(service) = resource.integration_metadata.get("service_name") {
                self.stop_user_service(service, cancellation.clone())
                    .await?;
                outputs.push(format!(
                    "stopped Docker service '{service}' started by Workstate"
                ));
                continue;
            }
            if let Some(identity) = resource.integration_metadata.get("process_identity") {
                let process = BackgroundProcess::new(identity.clone())?;
                self.process_runner.stop_background(process).await?;
                let mut state = self.state.lock().await;
                if state
                    .started_process
                    .as_ref()
                    .is_some_and(|started| started.identity == *identity)
                {
                    state.started_process = None;
                }
                outputs.push("stopped Docker Desktop started by Workstate".to_owned());
                continue;
            }
            outputs.push(format!(
                "preserved Docker resource '{}' because its service identity is unavailable",
                resource.resource.stable_identity
            ));
        }
        Ok(outputs)
    }

    async fn stop_user_service(
        &self,
        service: &str,
        cancellation: CancellationToken,
    ) -> Result<()> {
        validate_service_name(service)?;
        if !self.linux_user_services {
            return Ok(());
        }
        let output = self
            .run_systemctl(
                vec!["--user".to_owned(), "stop".to_owned(), service.to_owned()],
                cancellation,
            )
            .await?;
        if output.succeeded() || is_service_not_found(&output) {
            return Ok(());
        }
        Err(service_operation_error(
            "stop Docker user service",
            service,
            &output,
        ))
    }

    async fn run_systemctl(
        &self,
        arguments: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<crate::application::ports::ProcessOutput> {
        cancellation.check()?;
        self.process_runner
            .run(ProcessRequest {
                program: "systemctl".to_owned(),
                arguments,
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|error| {
                error
                    .with_context("operation", "execute systemctl")
                    .with_context("service_scope", "user")
            })
    }
}

fn validate_service_name(service: &str) -> Result<()> {
    if matches!(service, "docker" | "docker-desktop") {
        return Ok(());
    }
    Err(WorkstateError::new(
        ErrorCategory::Integration,
        "unsupported Docker service name",
    )
    .with_context("service", service))
}

fn is_service_not_found(output: &crate::application::ports::ProcessOutput) -> bool {
    let diagnostic = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    [
        "not-found",
        "not found",
        "could not be found",
        "no such file",
        "unknown unit",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

fn is_inactive_service(output: &crate::application::ports::ProcessOutput) -> bool {
    let status = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    matches!(
        status.as_str(),
        "inactive" | "failed" | "deactivating" | "dead" | "unknown" | "not-found"
    ) || is_service_not_found(output)
}

fn is_service_starting(output: &crate::application::ports::ProcessOutput) -> bool {
    let status = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    matches!(status.as_str(), "activating" | "reloading")
}

fn service_operation_error(
    operation: &str,
    service: &str,
    output: &crate::application::ports::ProcessOutput,
) -> WorkstateError {
    let detail = super::errors::sanitized_output(&output.stderr)
        .or_else(|| super::errors::sanitized_output(&output.stdout))
        .unwrap_or_else(|| "systemctl returned no diagnostic output".to_owned());
    WorkstateError::new(
        ErrorCategory::Integration,
        format!("{operation} '{service}' failed"),
    )
    .with_context("service", service)
    .with_context("detail", detail)
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
    record.observed_before = ownership == OwnershipStatus::PreExisting;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("process_identity".to_owned(), process.identity.clone());
    record
        .integration_metadata
        .insert("process_name".to_owned(), executable_name.to_owned());
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
    record
        .integration_metadata
        .insert("environment".to_owned(), context.environment.to_string());
    Ok(record)
}

fn resource_for_service(
    context: &DockerActionContext,
    service: &str,
    kind: ResourceKind,
    ownership: OwnershipStatus,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(kind, format!("systemd-user:{service}"))
        .map_err(WorkstateError::from)?;
    let mut record =
        ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
    record.observed_before = ownership == OwnershipStatus::PreExisting;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("service_name".to_owned(), service.to_owned());
    record
        .integration_metadata
        .insert("service_scope".to_owned(), "user".to_owned());
    record
        .integration_metadata
        .insert("service_manager".to_owned(), "systemd".to_owned());
    record
        .integration_metadata
        .insert("environment".to_owned(), context.environment.to_string());
    Ok(record)
}
