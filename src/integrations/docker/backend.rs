use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, ActionOutputSink, CancellationToken, CompensationResult,
            ReadinessCheckResult, ReadinessReport,
        },
        ports::{
            BoxFuture, DockerActionContext, DockerBackend, DockerCheckReport, DockerCleanupRequest,
            DockerComposeObservation, DockerComposeRequest, DockerComposeServiceSnapshot,
            DockerComposeSnapshot, DockerContainerObservation, DockerContainerRequest,
            DockerContainerSnapshot, DockerContainerState, DockerEngineRequest,
            DockerEnsureOutcome, DockerHealthState, DockerOperationStatus, FileSystem,
            ProcessOutput, ProcessRunner,
        },
        timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    },
    domain::{
        ActionKind, ActionSpec, CommandSpec, ContainerSpec, OwnershipStatus, ReadinessCheck,
        ResourceKind, ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::filesystem::PathResolver,
    platform::CapabilityId,
};

use super::{
    checks,
    compose::DockerComposeController,
    desktop::DockerDesktopController,
    engine::DockerEngineController,
    errors::{self, compose_configuration_error, conflict, docker_error, missing_image},
    models,
};

#[derive(Clone)]
pub struct DockerProcessBackend {
    engine: Arc<DockerEngineController>,
    compose: Arc<DockerComposeController>,
    process_runner: Arc<dyn ProcessRunner>,
    file_system: Arc<dyn FileSystem>,
    poll_interval: Duration,
    resource_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
}

#[derive(Default)]
struct PartialDockerState {
    engine_resources: Vec<ResourceRecord>,
    container: Option<PartialContainerState>,
    compose_before: Option<DockerComposeSnapshot>,
    compose_started: bool,
}

struct PartialContainerState {
    identity: String,
    cleanup_operation: &'static str,
}

enum PartialCleanupOutcome {
    Cleaned,
    AlreadyAbsent,
    Preserved(String),
}

impl DockerProcessBackend {
    pub fn new(
        process_runner: Arc<dyn ProcessRunner>,
        file_system: Arc<dyn FileSystem>,
        docker_program: PathBuf,
        desktop_program: Option<PathBuf>,
        compose_program: Option<PathBuf>,
    ) -> Result<Self> {
        Self::new_for_platform(
            Arc::clone(&process_runner),
            file_system,
            docker_program,
            desktop_program,
            compose_program,
            cfg!(target_os = "linux"),
        )
    }

    pub fn new_for_platform(
        process_runner: Arc<dyn ProcessRunner>,
        file_system: Arc<dyn FileSystem>,
        docker_program: PathBuf,
        desktop_program: Option<PathBuf>,
        compose_program: Option<PathBuf>,
        linux_user_services: bool,
    ) -> Result<Self> {
        let desktop = Arc::new(DockerDesktopController::new_with_platform(
            Arc::clone(&process_runner),
            desktop_program,
            linux_user_services,
        ));
        let engine = Arc::new(DockerEngineController::new_for_platform(
            Arc::clone(&process_runner),
            docker_program,
            Arc::clone(&desktop),
            linux_user_services,
        )?);
        let compose = Arc::new(DockerComposeController::new(
            engine.clone(),
            compose_program,
        ));
        Ok(Self {
            engine,
            compose,
            process_runner,
            file_system,
            poll_interval: checks::DEFAULT_POLL_INTERVAL,
            resource_locks: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = checks::poll_interval(poll_interval);
        self
    }

    pub fn engine(&self) -> Arc<DockerEngineController> {
        Arc::clone(&self.engine)
    }

    async fn lock_resource(&self, key: String) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.resource_locks.lock().await;
            locks
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    async fn observe_container_inner(
        &self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerContainerObservation> {
        cancellation.check()?;
        let engine = self.engine.inspect(cancellation.clone()).await?;
        if !engine.ready {
            return Ok(DockerContainerObservation::Unavailable(engine));
        }
        let output = self
            .engine
            .run(
                vec![
                    "inspect".to_owned(),
                    "--format".to_owned(),
                    "{{json .}}".to_owned(),
                    request.specification.name.clone(),
                ],
                request.working_directory,
            )
            .await?;
        if !output.succeeded() {
            if is_missing_container(&output) {
                return Ok(DockerContainerObservation::Missing);
            }
            return Err(docker_error("inspect container", &output));
        }
        Ok(DockerContainerObservation::Present(Box::new(
            parse_container_snapshot(&output.stdout)?,
        )))
    }

    async fn ensure_container_inner(
        &self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        let mut partial = PartialDockerState::default();
        match self
            .ensure_container_uncompensated(request.clone(), cancellation.clone(), &mut partial)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(self
                .cleanup_partial_container(&request, partial, error)
                .await),
        }
    }

    async fn ensure_container_uncompensated(
        &self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
        partial: &mut PartialDockerState,
    ) -> Result<DockerEnsureOutcome> {
        cancellation.check()?;
        let engine = self
            .engine
            .ensure_ready(
                DockerEngineRequest {
                    launch_desktop_when_needed: true,
                    timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
                    poll_interval: self.poll_interval,
                    action: request.context.clone(),
                    environment: self.engine.environment(),
                },
                cancellation.clone(),
            )
            .await?;
        partial.engine_resources = engine.resources.clone();
        let mut resources = engine.resources;
        let mut outputs = engine.outputs;
        let before = self
            .observe_container_inner(request.clone(), cancellation.clone())
            .await?;
        let (snapshot, ownership, status, changed, cleanup_operation, observed_before) =
            match before {
                DockerContainerObservation::Missing => {
                    let image = request.specification.image.as_deref().ok_or_else(|| {
                        missing_image(
                            &request.context.action_id.to_string(),
                            &request.specification.name,
                        )
                    })?;
                    let create_output = self.create_container(&request, image).await?;
                    let created_identity = docker_identity_from_output(&create_output.stdout);
                    partial.container = created_identity.map(|identity| PartialContainerState {
                        identity,
                        cleanup_operation: models::CONTAINER_CLEANUP_REMOVE,
                    });
                    let created = self
                        .observe_container_inner(request.clone(), cancellation.clone())
                        .await?;
                    let snapshot = match created {
                        DockerContainerObservation::Present(snapshot) => snapshot,
                        DockerContainerObservation::Missing => {
                            return Err(WorkstateError::new(
                                ErrorCategory::Integration,
                                "Docker did not expose the container after creation",
                            )
                            .with_context("container_name", &request.specification.name));
                        }
                        DockerContainerObservation::Unavailable(engine) => {
                            return Err(engine_unavailable_after_mutation(&engine));
                        }
                    };
                    let id = snapshot.id.clone();
                    partial.container = Some(PartialContainerState {
                        identity: id.clone(),
                        cleanup_operation: models::CONTAINER_CLEANUP_REMOVE,
                    });
                    outputs.push(format!(
                        "created Docker container '{}'",
                        request.specification.name
                    ));
                    if !create_output.stdout.is_empty() {
                        outputs.push(format!("Docker assigned container identity '{id}'"));
                    }
                    self.start_container(&request, &snapshot, cancellation.clone())
                        .await?;
                    let started = self
                        .observe_container_inner(request.clone(), cancellation.clone())
                        .await?;
                    let snapshot = match started {
                        DockerContainerObservation::Present(snapshot) => snapshot,
                        DockerContainerObservation::Missing => {
                            return Err(WorkstateError::new(
                                ErrorCategory::Integration,
                                "Docker container disappeared after it was started",
                            )
                            .with_context("container_name", &request.specification.name));
                        }
                        DockerContainerObservation::Unavailable(engine) => {
                            return Err(engine_unavailable_after_mutation(&engine));
                        }
                    };
                    if !models::container_matches(&request, &snapshot) {
                        return Err(conflict(
                            &request.specification.name,
                            &models::container_configuration_key(&request),
                            &models::snapshot_configuration_key(&snapshot),
                        ));
                    }
                    (
                        snapshot,
                        OwnershipStatus::CreatedByCurrentRun,
                        DockerOperationStatus::Created,
                        true,
                        Some(models::CONTAINER_CLEANUP_REMOVE),
                        false,
                    )
                }
                DockerContainerObservation::Present(snapshot) => {
                    if !models::container_matches(&request, &snapshot) {
                        return Err(conflict(
                            &request.specification.name,
                            &models::container_configuration_key(&request),
                            &models::snapshot_configuration_key(&snapshot),
                        ));
                    }
                    if snapshot.state.is_running() {
                        if !has_observable_readiness_checks(&request.readiness_checks)
                            && !snapshot.health.satisfies_readiness()
                        {
                            return Err(self
                                .container_not_ready_error(&request.specification.name, &snapshot)
                                .await);
                        }
                        outputs.push(format!(
                            "reused running Docker container '{}'",
                            request.specification.name
                        ));
                        (
                            snapshot,
                            OwnershipStatus::ReusedExisting,
                            DockerOperationStatus::Reused,
                            false,
                            None,
                            true,
                        )
                    } else {
                        partial.container = Some(PartialContainerState {
                            identity: snapshot.id.clone(),
                            cleanup_operation: models::CONTAINER_CLEANUP_STOP,
                        });
                        self.start_container(&request, &snapshot, cancellation.clone())
                            .await?;
                        let started = self
                            .observe_container_inner(request.clone(), cancellation.clone())
                            .await?;
                        let snapshot = match started {
                            DockerContainerObservation::Present(snapshot) => snapshot,
                            DockerContainerObservation::Missing => {
                                return Err(WorkstateError::new(
                                    ErrorCategory::Integration,
                                    "Docker container disappeared after it was started",
                                )
                                .with_context("container_name", &request.specification.name));
                            }
                            DockerContainerObservation::Unavailable(engine) => {
                                return Err(engine_unavailable_after_mutation(&engine));
                            }
                        };
                        partial.container = Some(PartialContainerState {
                            identity: snapshot.id.clone(),
                            cleanup_operation: models::CONTAINER_CLEANUP_STOP,
                        });
                        outputs.push(format!(
                            "started existing Docker container '{}'",
                            request.specification.name
                        ));
                        (
                            snapshot,
                            OwnershipStatus::CreatedByCurrentRun,
                            DockerOperationStatus::Repaired,
                            true,
                            Some(models::CONTAINER_CLEANUP_STOP),
                            true,
                        )
                    }
                }
                DockerContainerObservation::Unavailable(engine) => {
                    return Err(engine_unavailable_after_mutation(&engine));
                }
            };
        let record = models::container_record_with_cleanup(
            &request.context,
            &request,
            &snapshot,
            ownership,
            observed_before,
            cleanup_operation,
        )?;
        resources.push(record);
        let outcome = DockerEnsureOutcome {
            status,
            resources,
            detail: None,
            outputs,
        }
        .with_output(if changed {
            format!(
                "Docker container '{}' is running",
                request.specification.name
            )
        } else {
            format!(
                "Docker container '{}' was already healthy",
                request.specification.name
            )
        });
        if has_observable_readiness_checks(&request.readiness_checks) {
            self.check_container_readiness(request, cancellation)
                .await?;
        }
        Ok(outcome)
    }

    async fn cleanup_partial_container(
        &self,
        request: &DockerContainerRequest,
        partial: PartialDockerState,
        error: WorkstateError,
    ) -> WorkstateError {
        let mut diagnostics = Vec::new();
        if let Some(container) = partial.container {
            match self
                .cleanup_exact_container(
                    &container.identity,
                    container.cleanup_operation,
                    Some(&request.specification.name),
                )
                .await
            {
                Ok(PartialCleanupOutcome::Cleaned | PartialCleanupOutcome::AlreadyAbsent) => {}
                Ok(PartialCleanupOutcome::Preserved(reason)) => diagnostics.push(reason),
                Err(cleanup_error) => diagnostics.push(cleanup_error.render()),
            }
        }
        self.append_engine_cleanup(&partial.engine_resources, &mut diagnostics)
            .await;
        if diagnostics.is_empty() {
            error.with_context("partial_cleanup", "completed")
        } else {
            error.with_context("partial_cleanup", diagnostics.join("; "))
        }
    }

    async fn cleanup_exact_container(
        &self,
        identity: &str,
        cleanup_operation: &str,
        expected_name: Option<&str>,
    ) -> Result<PartialCleanupOutcome> {
        let Some(snapshot) = self.inspect_container_identity(identity).await? else {
            return Ok(PartialCleanupOutcome::AlreadyAbsent);
        };
        if snapshot.id != identity
            || expected_name.is_some_and(|expected| snapshot.name != expected)
        {
            return Ok(PartialCleanupOutcome::Preserved(format!(
                "preserved Docker container identity '{identity}' because it no longer matches the original resource"
            )));
        }
        let should_stop = cleanup_operation == models::CONTAINER_CLEANUP_STOP;
        if should_stop && !snapshot.state.is_running() {
            return Ok(PartialCleanupOutcome::AlreadyAbsent);
        }
        let arguments = if should_stop {
            vec!["stop".to_owned(), identity.to_owned()]
        } else {
            vec!["rm".to_owned(), "--force".to_owned(), identity.to_owned()]
        };
        let output = self.engine.run(arguments, None).await?;
        if !output.succeeded()
            && !is_missing_container(&output)
            && !(should_stop && is_already_stopped(&output))
        {
            return Err(if should_stop {
                docker_error("stop partially started container", &output)
            } else {
                docker_error("remove partially created container", &output)
            });
        }
        Ok(PartialCleanupOutcome::Cleaned)
    }

    async fn inspect_container_identity(
        &self,
        identity: &str,
    ) -> Result<Option<DockerContainerSnapshot>> {
        let output = self
            .engine
            .run(
                vec![
                    "inspect".to_owned(),
                    "--format".to_owned(),
                    "{{json .}}".to_owned(),
                    identity.to_owned(),
                ],
                None,
            )
            .await?;
        if !output.succeeded() {
            if is_missing_container(&output) {
                return Ok(None);
            }
            return Err(docker_error("inspect partially created container", &output));
        }
        parse_container_snapshot(&output.stdout).map(Some)
    }

    async fn append_engine_cleanup(
        &self,
        resources: &[ResourceRecord],
        diagnostics: &mut Vec<String>,
    ) {
        if resources.is_empty() {
            return;
        }
        if let Err(error) = self
            .engine
            .stop_desktop(resources, CancellationToken::new())
            .await
        {
            diagnostics.push(error.render());
        }
    }

    async fn create_container(
        &self,
        request: &DockerContainerRequest,
        image: &str,
    ) -> Result<ProcessOutput> {
        let mut arguments = vec![
            "create".to_owned(),
            "--name".to_owned(),
            request.specification.name.clone(),
        ];
        for (key, value) in &request.specification.environment {
            arguments.extend(["--env".to_owned(), format!("{key}={value}")]);
        }
        for mount in &request.specification.mounts {
            let source = self.resolve_mount_source(mount.source.as_str())?;
            let mut value = format!(
                "type=bind,source={},destination={}",
                source.display(),
                mount.target
            );
            if mount.read_only {
                value.push_str(",readonly");
            }
            arguments.extend(["--mount".to_owned(), value]);
        }
        for port in &request.specification.ports {
            arguments.extend([
                "--publish".to_owned(),
                format!(
                    "{}:{}{}",
                    port.host,
                    port.container,
                    protocol_suffix(&port.protocol)
                ),
            ]);
        }
        arguments.push(image.to_owned());
        if let Some(command) = &request.specification.command {
            if command.shell {
                arguments.extend([
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    command.program.clone(),
                ]);
            } else {
                arguments.push(command.program.clone());
                arguments.extend(command.arguments.clone());
            }
        }
        let output = self
            .engine
            .run(arguments, request.working_directory.clone())
            .await?;
        if !output.succeeded() {
            return Err(docker_error("create container", &output));
        }
        Ok(output)
    }

    async fn start_container(
        &self,
        request: &DockerContainerRequest,
        snapshot: &DockerContainerSnapshot,
        cancellation: CancellationToken,
    ) -> Result<()> {
        cancellation.check()?;
        let output = self
            .engine
            .run(
                vec!["start".to_owned(), snapshot.id.clone()],
                request.working_directory.clone(),
            )
            .await?;
        if output.succeeded() {
            return Ok(());
        }
        let logs = self.container_logs(&snapshot.id).await;
        let mut error = docker_error("start container", &output)
            .with_context("container_name", &request.specification.name);
        match logs {
            Ok(Some(logs)) => {
                error = error.with_context("log_tail", logs);
            }
            Ok(None) => {}
            Err(log_error) => {
                error = error.with_context("log_tail_error", log_error.render());
            }
        }
        Err(error)
    }

    async fn container_logs(&self, identity: &str) -> Result<Option<String>> {
        let output = self
            .engine
            .run(
                vec![
                    "logs".to_owned(),
                    "--tail".to_owned(),
                    "20".to_owned(),
                    identity.to_owned(),
                ],
                None,
            )
            .await?;
        if !output.succeeded() {
            return Err(docker_error("read container logs", &output));
        }
        Ok(errors::sanitized_output(&output.stdout)
            .or_else(|| errors::sanitized_output(&output.stderr)))
    }

    async fn container_not_ready_error(
        &self,
        container_name: &str,
        snapshot: &DockerContainerSnapshot,
    ) -> WorkstateError {
        WorkstateError::new(
            ErrorCategory::Integration,
            format!(
                "Docker container '{container_name}' is running but not ready: {}",
                self.container_failure_detail(snapshot).await
            ),
        )
        .with_context("container_name", container_name)
        .with_context("container_id", snapshot.id.clone())
    }

    async fn container_failure_detail(&self, snapshot: &DockerContainerSnapshot) -> String {
        let state = container_state_detail(snapshot);
        match self.container_logs(&snapshot.id).await {
            Ok(Some(logs)) => format!("{state}; log tail: {logs}"),
            Ok(None) | Err(_) => state,
        }
    }

    fn resolve_mount_source(&self, raw: &str) -> Result<PathBuf> {
        let home = self.file_system.home_directory()?;
        let resolver = PathResolver::new(home, self.file_system.as_ref())?;
        resolver.canonicalize_for_execution(raw)
    }

    async fn observe_compose_inner(
        &self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerComposeObservation> {
        self.compose.observe(request, cancellation).await
    }

    async fn ensure_compose_inner(
        &self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        let mut partial = PartialDockerState::default();
        match self
            .ensure_compose_uncompensated(request.clone(), cancellation.clone(), &mut partial)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(self.cleanup_partial_compose(&request, partial, error).await),
        }
    }

    async fn ensure_compose_uncompensated(
        &self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
        partial: &mut PartialDockerState,
    ) -> Result<DockerEnsureOutcome> {
        cancellation.check()?;
        let engine = self
            .engine
            .ensure_ready(
                DockerEngineRequest {
                    launch_desktop_when_needed: true,
                    timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
                    poll_interval: self.poll_interval,
                    action: request.context.clone(),
                    environment: request.environment.clone(),
                },
                cancellation.clone(),
            )
            .await?;
        partial.engine_resources = engine.resources.clone();
        let mut resources = engine.resources;
        let mut outputs = engine.outputs;
        let before = self
            .observe_compose_inner(request.clone(), cancellation.clone())
            .await?;
        let (before_snapshot, project_ownership) = match before {
            DockerComposeObservation::Missing => (None, OwnershipStatus::CreatedByCurrentRun),
            DockerComposeObservation::Present(snapshot) => {
                (Some(snapshot), OwnershipStatus::ReusedExisting)
            }
            DockerComposeObservation::Unavailable(engine) => {
                return Err(engine_unavailable_after_mutation(&engine));
            }
        };
        partial.compose_before = before_snapshot.clone();

        partial.compose_started = true;
        let output = self.compose.up(&request, cancellation.clone()).await?;
        if !output.succeeded() {
            return Err(docker_error("start Compose project", &output));
        }
        outputs.push(format!(
            "reconciled Docker Compose project in '{}'",
            request.working_directory.display()
        ));
        let after = match self
            .observe_compose_inner(request.clone(), cancellation.clone())
            .await?
        {
            DockerComposeObservation::Present(snapshot) => snapshot,
            DockerComposeObservation::Missing => {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "Docker Compose did not expose the project after startup",
                )
                .with_context(
                    "working_directory",
                    request.working_directory.display().to_string(),
                ));
            }
            DockerComposeObservation::Unavailable(engine) => {
                return Err(engine_unavailable_after_mutation(&engine));
            }
        };
        resources.push(models::compose_record(
            &request.context,
            &after,
            project_ownership,
        )?);
        let outcome = DockerEnsureOutcome {
            status: if before_snapshot.is_some() {
                DockerOperationStatus::Repaired
            } else {
                DockerOperationStatus::Created
            },
            resources,
            detail: None,
            outputs,
        };
        if has_observable_readiness_checks(&request.readiness_checks) {
            self.check_compose_readiness(request, cancellation).await?;
        }
        Ok(outcome)
    }

    async fn cleanup_partial_compose(
        &self,
        request: &DockerComposeRequest,
        partial: PartialDockerState,
        error: WorkstateError,
    ) -> WorkstateError {
        let mut diagnostics = Vec::new();
        if partial.compose_started {
            match self
                .cleanup_partial_compose_resources(request, partial.compose_before.as_ref())
                .await
            {
                Ok(details) => diagnostics.extend(details),
                Err(cleanup_error) => diagnostics.push(cleanup_error.render()),
            }
        }
        self.append_engine_cleanup(&partial.engine_resources, &mut diagnostics)
            .await;
        if diagnostics.is_empty() {
            error.with_context("partial_cleanup", "completed")
        } else {
            error.with_context("partial_cleanup", diagnostics.join("; "))
        }
    }

    async fn cleanup_partial_compose_resources(
        &self,
        request: &DockerComposeRequest,
        before: Option<&DockerComposeSnapshot>,
    ) -> Result<Vec<String>> {
        let observation = self
            .compose
            .observe(request.clone(), CancellationToken::new())
            .await?;
        let after = match observation {
            DockerComposeObservation::Missing => return Ok(Vec::new()),
            DockerComposeObservation::Present(after) => after,
            DockerComposeObservation::Unavailable(engine) => {
                return Err(engine_unavailable_after_mutation(&engine));
            }
        };
        let before_states = before
            .map(|snapshot| {
                snapshot
                    .services
                    .iter()
                    .filter_map(|service| {
                        service
                            .container_id
                            .as_ref()
                            .map(|id| (id.clone(), service.state.is_running()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut outputs = Vec::new();
        for service in after.services {
            let Some(identity) = service.container_id.as_deref() else {
                continue;
            };
            let cleanup_operation = match before_states.get(identity) {
                None => models::CONTAINER_CLEANUP_REMOVE,
                Some(true) => continue,
                Some(false) => models::CONTAINER_CLEANUP_STOP,
            };
            match self
                .cleanup_exact_container(identity, cleanup_operation, None)
                .await?
            {
                PartialCleanupOutcome::Cleaned => outputs.push(format!(
                    "cleaned partially started Compose service '{}'",
                    service.name
                )),
                PartialCleanupOutcome::AlreadyAbsent => outputs.push(format!(
                    "partially started Compose service '{}' was already absent",
                    service.name
                )),
                PartialCleanupOutcome::Preserved(reason) => outputs.push(reason),
            }
        }
        Ok(outputs)
    }

    async fn check_container_readiness(
        &self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerCheckReport> {
        let checks = request.readiness_checks.clone();
        self.run_checks(checks, cancellation, |check, token| {
            let request = request.clone();
            async move { self.check_container_once(&request, &check, token).await }
        })
        .await
    }

    async fn check_compose_readiness(
        &self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerCheckReport> {
        let checks = request.readiness_checks.clone();
        self.run_checks(checks, cancellation, |check, token| {
            let request = request.clone();
            async move { self.check_compose_once(&request, &check, token).await }
        })
        .await
    }

    async fn run_checks<F, Fut>(
        &self,
        checks_to_run: Vec<ReadinessCheck>,
        cancellation: CancellationToken,
        mut check: F,
    ) -> Result<DockerCheckReport>
    where
        F: FnMut(ReadinessCheck, CancellationToken) -> Fut,
        Fut: std::future::Future<Output = Result<ReadinessCheckResult>>,
    {
        let mut checks_run = 0usize;
        let mut last_detail = None;
        for readiness_check in checks_to_run {
            cancellation.check()?;
            if let ReadinessCheck::None = readiness_check {
                checks_run = checks_run.saturating_add(1);
                continue;
            }
            let timeout = readiness_timeout(&readiness_check);
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                cancellation.check()?;
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(readiness_timeout_error(
                        &readiness_check,
                        timeout,
                        last_detail.as_deref(),
                    ));
                }
                let remaining = deadline.saturating_duration_since(now);
                let result = tokio::select! {
                    _ = cancellation.cancelled() => {
                        return Err(WorkstateError::new(
                            ErrorCategory::Runtime,
                            "operation was cancelled during Docker readiness checks",
                        ).with_context("cancelled", "true"));
                    }
                    result = tokio::time::timeout(
                        remaining,
                        check(readiness_check.clone(), cancellation.clone()),
                    ) => {
                        match result {
                            Ok(result) => result?,
                            Err(_) => {
                                return Err(readiness_timeout_error(
                                    &readiness_check,
                                    timeout,
                                    last_detail.as_deref(),
                                ));
                            }
                        }
                    }
                };
                checks_run = checks_run.saturating_add(1);
                if result.passed {
                    last_detail = result.detail;
                    break;
                }
                last_detail = result.detail;
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(readiness_timeout_error(
                        &readiness_check,
                        timeout,
                        last_detail.as_deref(),
                    ));
                }
                let remaining = deadline.saturating_duration_since(now);
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        return Err(WorkstateError::new(
                            ErrorCategory::Runtime,
                            "operation was cancelled during Docker readiness checks",
                        ).with_context("cancelled", "true"));
                    }
                    _ = tokio::time::sleep(self.poll_interval.min(remaining)) => {}
                }
            }
        }
        Ok(DockerCheckReport {
            checks_run,
            last_detail,
        })
    }

    async fn check_container_once(
        &self,
        request: &DockerContainerRequest,
        check: &ReadinessCheck,
        cancellation: CancellationToken,
    ) -> Result<ReadinessCheckResult> {
        match check {
            ReadinessCheck::Container { name, .. } => {
                let mut request = request.clone();
                request.specification.name = name.clone();
                match self.observe_container_inner(request, cancellation).await? {
                    DockerContainerObservation::Present(snapshot) => {
                        if models::healthy_container(&snapshot) {
                            Ok(ReadinessCheckResult::passed())
                        } else {
                            Ok(ReadinessCheckResult::failed(
                                self.container_failure_detail(&snapshot).await,
                            ))
                        }
                    }
                    DockerContainerObservation::Missing => Ok(ReadinessCheckResult::failed(
                        "the expected Docker container is missing",
                    )),
                    DockerContainerObservation::Unavailable(engine) => {
                        Ok(ReadinessCheckResult::failed(engine.detail.unwrap_or_else(
                            || "Docker Engine is unavailable".to_owned(),
                        )))
                    }
                }
            }
            ReadinessCheck::Tcp {
                host,
                port,
                timeout,
            } => {
                checks::tcp_check(
                    host.clone(),
                    *port,
                    Duration::from_millis(timeout.milliseconds),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Http {
                url,
                expected_status,
                timeout,
            } => {
                checks::http_check(
                    self.process_runner.as_ref(),
                    url.clone(),
                    *expected_status,
                    Duration::from_millis(timeout.milliseconds),
                    request.working_directory.clone(),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Command { command, timeout } => {
                checks::command_check(
                    self.process_runner.as_ref(),
                    &request.context.action_id,
                    command,
                    Duration::from_millis(timeout.milliseconds),
                    request.working_directory.clone(),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Delay { milliseconds } => {
                checks::delay(*milliseconds, cancellation).await
            }
            ReadinessCheck::None | ReadinessCheck::Compose { .. } => {
                Ok(ReadinessCheckResult::failed(
                    "the readiness check does not target a direct Docker container",
                ))
            }
        }
    }

    async fn check_compose_once(
        &self,
        request: &DockerComposeRequest,
        check: &ReadinessCheck,
        cancellation: CancellationToken,
    ) -> Result<ReadinessCheckResult> {
        match check {
            ReadinessCheck::Compose { services, .. } => {
                match self
                    .observe_compose_inner(request.clone(), cancellation)
                    .await?
                {
                    DockerComposeObservation::Present(snapshot) => {
                        if snapshot.is_healthy(services) {
                            Ok(ReadinessCheckResult::passed())
                        } else {
                            Ok(ReadinessCheckResult::failed(compose_state_detail(
                                &snapshot, services,
                            )))
                        }
                    }
                    DockerComposeObservation::Missing => Ok(ReadinessCheckResult::failed(
                        "the expected Docker Compose project is missing",
                    )),
                    DockerComposeObservation::Unavailable(engine) => {
                        Ok(ReadinessCheckResult::failed(engine.detail.unwrap_or_else(
                            || "Docker Engine is unavailable".to_owned(),
                        )))
                    }
                }
            }
            ReadinessCheck::Container { name, .. } => {
                let request = DockerContainerRequest {
                    context: request.context.clone(),
                    specification: ContainerSpec {
                        name: name.clone(),
                        image: None,
                        command: None,
                        environment: BTreeMap::new(),
                        mounts: Vec::new(),
                        ports: Vec::new(),
                    },
                    working_directory: Some(request.working_directory.clone()),
                    readiness_checks: Vec::new(),
                };
                self.check_container_once(&request, check, cancellation)
                    .await
            }
            ReadinessCheck::Tcp {
                host,
                port,
                timeout,
            } => {
                checks::tcp_check(
                    host.clone(),
                    *port,
                    Duration::from_millis(timeout.milliseconds),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Http {
                url,
                expected_status,
                timeout,
            } => {
                checks::http_check(
                    self.process_runner.as_ref(),
                    url.clone(),
                    *expected_status,
                    Duration::from_millis(timeout.milliseconds),
                    Some(request.working_directory.clone()),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Command { command, timeout } => {
                checks::command_check(
                    self.process_runner.as_ref(),
                    &request.context.action_id,
                    command,
                    Duration::from_millis(timeout.milliseconds),
                    Some(request.working_directory.clone()),
                    cancellation,
                )
                .await
            }
            ReadinessCheck::Delay { milliseconds } => {
                checks::delay(*milliseconds, cancellation).await
            }
            ReadinessCheck::None => Ok(ReadinessCheckResult::passed()),
        }
    }

    async fn stop_owned_inner(
        &self,
        request: DockerCleanupRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        cancellation.check()?;
        let mut outputs = Vec::new();
        let mut conflict_detected = false;
        let engine_resources = if request.compose.is_some() || request.specification.is_some() {
            let environment = request
                .compose
                .as_ref()
                .map(|compose| compose.environment.clone())
                .unwrap_or_else(|| self.engine.environment());
            self.engine
                .ensure_ready(
                    DockerEngineRequest {
                        launch_desktop_when_needed: true,
                        timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
                        poll_interval: self.poll_interval,
                        action: request.context.clone(),
                        environment,
                    },
                    cancellation.clone(),
                )
                .await?
                .resources
        } else {
            Vec::new()
        };
        if let Some(compose_request) = &request.compose {
            let (compose_outputs, compose_conflict) = self
                .stop_compose_owned(compose_request, &request.resources, cancellation.clone())
                .await?;
            outputs.extend(compose_outputs);
            conflict_detected |= compose_conflict;
        }
        if let Some(specification) = &request.specification {
            let container_request = DockerContainerRequest {
                context: request.context.clone(),
                specification: specification.clone(),
                working_directory: None,
                readiness_checks: Vec::new(),
            };
            for resource in request.resources.iter().filter(|resource| {
                resource.resource.kind == ResourceKind::DockerContainer
                    && resource.is_cleanup_candidate()
            }) {
                cancellation.check()?;
                let observed = self
                    .observe_container_inner(container_request.clone(), cancellation.clone())
                    .await?;
                let snapshot = match observed {
                    DockerContainerObservation::Present(snapshot) => snapshot,
                    DockerContainerObservation::Missing => {
                        outputs.push(format!(
                            "Docker container '{}' was already absent",
                            specification.name
                        ));
                        continue;
                    }
                    DockerContainerObservation::Unavailable(engine) => {
                        return Err(engine_unavailable_after_mutation(&engine));
                    }
                };
                if snapshot.id != resource.resource.stable_identity
                    || !models::record_matches_snapshot(resource, &container_request, &snapshot)
                {
                    outputs.push(format!(
                        "preserved Docker container '{}' because its configuration changed externally",
                        specification.name
                    ));
                    conflict_detected = true;
                    continue;
                }
                let cleanup_operation = resource
                    .integration_metadata
                    .get(models::CONTAINER_CLEANUP_OPERATION)
                    .map(String::as_str)
                    .unwrap_or(models::CONTAINER_CLEANUP_REMOVE);
                if cleanup_operation == models::CONTAINER_CLEANUP_STOP
                    && !snapshot.state.is_running()
                {
                    outputs.push(format!(
                        "Docker container '{}' was already stopped",
                        specification.name
                    ));
                    continue;
                }
                let should_stop = cleanup_operation == models::CONTAINER_CLEANUP_STOP;
                let arguments = if should_stop {
                    vec!["stop".to_owned(), snapshot.id.clone()]
                } else {
                    vec!["rm".to_owned(), "--force".to_owned(), snapshot.id.clone()]
                };
                let output = self.engine.run(arguments, None).await?;
                let already_absent = is_missing_container(&output);
                let already_stopped = should_stop && is_already_stopped(&output);
                if !output.succeeded() && !already_absent && !already_stopped {
                    return Err(if should_stop {
                        docker_error("stop owned container", &output)
                    } else {
                        docker_error("remove owned container", &output)
                    });
                }
                if should_stop {
                    outputs.push(format!(
                        "stopped Docker container '{}' started by Workstate",
                        specification.name
                    ));
                } else {
                    outputs.push(format!(
                        "removed Docker container '{}' created by Workstate",
                        specification.name
                    ));
                }
            }
        }
        let mut service_resources = request.resources.clone();
        service_resources.extend(engine_resources);
        outputs.extend(
            self.engine
                .stop_desktop(&service_resources, cancellation.clone())
                .await?,
        );
        Ok(DockerEnsureOutcome {
            status: if conflict_detected {
                DockerOperationStatus::Conflict
            } else {
                DockerOperationStatus::Repaired
            },
            resources: Vec::new(),
            detail: conflict_detected.then_some(
                "one or more Docker resources were preserved because ownership could not be proven"
                    .to_owned(),
            ),
            outputs,
        })
    }

    async fn stop_compose_owned(
        &self,
        request: &DockerComposeRequest,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<(Vec<String>, bool)> {
        let Some(project) = resources.iter().find(|resource| {
            resource.resource.kind == ResourceKind::DockerCompose && resource.is_cleanup_candidate()
        }) else {
            return Ok((Vec::new(), false));
        };
        let project_name = project
            .integration_metadata
            .get("project_name")
            .cloned()
            .unwrap_or_else(|| {
                request
                    .working_directory
                    .file_name()
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.is_empty())
                    .unwrap_or("compose-project")
                    .to_owned()
            });
        let output = self.compose.down(request, cancellation).await?;
        if output.succeeded() || is_missing_project(&output) {
            return Ok((
                vec![format!(
                    "stopped owned Docker Compose project '{project_name}'"
                )],
                false,
            ));
        }
        Err(docker_error("stop Compose project", &output))
    }
}

impl DockerBackend for DockerProcessBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn inspect_engine<'a>(
        &'a self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<crate::application::ports::DockerEngineSnapshot>> {
        Box::pin(async move { self.engine.inspect(cancellation).await })
    }

    fn ensure_engine_ready<'a>(
        &'a self,
        request: DockerEngineRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async move { self.engine.ensure_ready(request, cancellation).await })
    }

    fn observe_container<'a>(
        &'a self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerContainerObservation>> {
        Box::pin(async move { self.observe_container_inner(request, cancellation).await })
    }

    fn ensure_container<'a>(
        &'a self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async move {
            let _guard = self
                .lock_resource(format!("container:{}", request.specification.name))
                .await;
            self.ensure_container_inner(request, cancellation).await
        })
    }

    fn observe_compose<'a>(
        &'a self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerComposeObservation>> {
        Box::pin(async move { self.observe_compose_inner(request, cancellation).await })
    }

    fn ensure_compose<'a>(
        &'a self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async move {
            let _guard = self.lock_resource(compose_lock_key(&request)).await;
            self.ensure_compose_inner(request, cancellation).await
        })
    }

    fn check_readiness<'a>(
        &'a self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async move { self.check_container_readiness(request, cancellation).await })
    }

    fn check_compose_readiness<'a>(
        &'a self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async move { self.check_compose_readiness(request, cancellation).await })
    }

    fn stop_owned<'a>(
        &'a self,
        request: DockerCleanupRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        Box::pin(async move {
            let _guard = self.lock_resource(cleanup_lock_key(&request)).await;
            self.stop_owned_inner(request, cancellation).await
        })
    }
}

#[derive(Clone)]
pub struct DockerActionHandler {
    key: &'static str,
    docker: Arc<dyn DockerBackend>,
    file_system: Arc<dyn FileSystem>,
}

impl DockerActionHandler {
    pub fn new(
        key: &'static str,
        docker: Arc<dyn DockerBackend>,
        file_system: Arc<dyn FileSystem>,
    ) -> Result<Self> {
        if !matches!(key, "start_container" | "start_compose") {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("unsupported Docker action handler key '{key}'"),
            ));
        }
        Ok(Self {
            key,
            docker,
            file_system,
        })
    }

    fn action_matches(&self, action: &ActionSpec) -> bool {
        matches!(
            (&action.kind, self.key),
            (ActionKind::StartContainer, "start_container")
                | (ActionKind::StartCompose, "start_compose")
        )
    }

    fn context(&self, action: &ActionSpec) -> Result<DockerActionContext> {
        let environment = action.resolved_environment.clone().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Runtime,
                format!(
                    "Docker action '{}' was executed without an environment context",
                    action.id
                ),
            )
        })?;
        Ok(DockerActionContext {
            action_id: action.id.clone(),
            environment,
            cleanup_policy: action.cleanup_policy,
        })
    }

    fn resolve_directory(&self, raw: Option<&str>, required: bool) -> Result<Option<PathBuf>> {
        let Some(raw) = raw else {
            if required {
                return Err(compose_configuration_error(
                    "Docker Compose actions require an explicit working directory",
                ));
            }
            return Ok(None);
        };
        let home = self.file_system.home_directory()?;
        let resolver = PathResolver::new(home, self.file_system.as_ref())?;
        resolver.canonicalize_for_execution(raw).map(Some)
    }

    fn container_request(&self, action: &ActionSpec) -> Result<DockerContainerRequest> {
        let mut specification = action.parameters.container.clone().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!(
                    "Docker container action '{}' is missing its configuration",
                    action.id
                ),
            )
        })?;
        self.resolve_container_mounts(&mut specification)?;
        Ok(DockerContainerRequest {
            context: self.context(action)?,
            specification,
            working_directory: self
                .resolve_directory(action.working_directory.as_deref(), false)?,
            readiness_checks: action.readiness_checks.clone(),
        })
    }

    fn compose_request(&self, action: &ActionSpec) -> Result<DockerComposeRequest> {
        let mut specification = action.parameters.compose.clone().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!(
                    "Docker Compose action '{}' is missing its configuration",
                    action.id
                ),
            )
        })?;
        let working_directory = self
            .resolve_directory(action.working_directory.as_deref(), true)?
            .ok_or_else(|| compose_configuration_error("Compose working directory is missing"))?;
        if specification.compose_file.is_none() && specification.up_command.is_none() {
            return Err(compose_configuration_error(
                "Docker Compose actions require a compose file or an explicit up command",
            ));
        }
        if let Some(file) = &specification.compose_file {
            specification.compose_file = Some(self.resolve_compose_file(&working_directory, file)?);
        }
        let environment = compose_environment(&specification);
        Ok(DockerComposeRequest {
            context: self.context(action)?,
            specification,
            working_directory,
            readiness_checks: action.readiness_checks.clone(),
            environment,
        })
    }

    fn resolve_container_mounts(&self, specification: &mut ContainerSpec) -> Result<()> {
        if specification.mounts.is_empty() {
            return Ok(());
        }
        let home = self.file_system.home_directory()?;
        let resolver = PathResolver::new(home, self.file_system.as_ref())?;
        for mount in &mut specification.mounts {
            mount.source = resolver
                .canonicalize_for_execution(&mount.source)?
                .display()
                .to_string();
        }
        Ok(())
    }

    fn resolve_compose_file(
        &self,
        working_directory: &std::path::Path,
        raw: &str,
    ) -> Result<String> {
        if raw.is_empty() || raw.chars().any(char::is_control) {
            return Err(compose_configuration_error(
                "Compose file paths must be non-empty and contain no control characters",
            ));
        }
        let home = self.file_system.home_directory()?;
        let resolver = PathResolver::new(home, self.file_system.as_ref())?;
        let candidate =
            if raw == "~" || raw.starts_with("~/") || raw == "$HOME" || raw.starts_with("$HOME/") {
                resolver.expand(raw)?
            } else {
                let path = std::path::Path::new(raw);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    working_directory.join(path)
                }
            };
        if candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(compose_configuration_error(
                "Compose file paths must not contain parent-directory traversal",
            ));
        }
        if !self.file_system.exists(&candidate)? {
            return Err(
                compose_configuration_error("configured Compose file does not exist")
                    .with_context("path", candidate.display().to_string()),
            );
        }
        if self.file_system.is_directory(&candidate)? {
            return Err(compose_configuration_error(
                "configured Compose file must not be a directory",
            )
            .with_context("path", candidate.display().to_string()));
        }
        Ok(self
            .file_system
            .canonicalize(&candidate)?
            .display()
            .to_string())
    }

    fn result(&self, outcome: DockerEnsureOutcome, default_changed: bool) -> ActionExecutionResult {
        ActionExecutionResult {
            changed: default_changed
                && !matches!(
                    outcome.status,
                    DockerOperationStatus::Reused
                        | DockerOperationStatus::Healthy
                        | DockerOperationStatus::Unchanged
                ),
            resources: outcome.resources,
            mutations: Vec::new(),
            outputs: outcome.outputs.into_iter().map(ActionOutput::log).collect(),
        }
    }

    fn cleanup_request(
        &self,
        action: &ActionSpec,
        resources: &[ResourceRecord],
    ) -> Result<DockerCleanupRequest> {
        let context = self.context(action)?;
        match &action.kind {
            ActionKind::StartContainer => Ok(DockerCleanupRequest {
                context,
                specification: Some(self.container_request(action)?.specification),
                compose: None,
                resources: resources.to_vec(),
            }),
            ActionKind::StartCompose => Ok(DockerCleanupRequest {
                context: context.clone(),
                specification: None,
                compose: Some(self.compose_request(action)?),
                resources: resources.to_vec(),
            }),
            _ => Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker cleanup received an incompatible action",
            )),
        }
    }

    async fn apply_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        match action.kind {
            ActionKind::StartContainer => {
                let request = self.container_request(action)?;
                let outcome = self.docker.ensure_container(request, cancellation).await?;
                Ok(self.result(outcome, true))
            }
            ActionKind::StartCompose => {
                let request = self.compose_request(action)?;
                let outcome = self.docker.ensure_compose(request, cancellation).await?;
                Ok(self.result(outcome, true))
            }
            _ => Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "Docker handler '{}' received an incompatible action",
                    action.id
                ),
            )),
        }
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        match action.kind {
            ActionKind::StartContainer => {
                let request = self.container_request(action)?;
                match self
                    .docker
                    .observe_container(request.clone(), cancellation)
                    .await?
                {
                    DockerContainerObservation::Missing => Ok(ActionObservation::requires_change()
                        .with_detail("the Docker container is missing")),
                    DockerContainerObservation::Unavailable(engine) => {
                        Ok(ActionObservation::requires_change().with_detail(
                            engine
                                .detail
                                .unwrap_or_else(|| "Docker Engine is unavailable".to_owned()),
                        ))
                    }
                    DockerContainerObservation::Present(snapshot) => {
                        if !models::container_matches(&request, &snapshot) {
                            return Ok(ActionObservation::requires_change().with_detail(
                                "the Docker container exists with incompatible configuration",
                            ));
                        }
                        let record = models::container_record(
                            &request.context,
                            &request,
                            &snapshot,
                            OwnershipStatus::ReusedExisting,
                            true,
                        )?;
                        if snapshot.state.is_running()
                            && snapshot.health.satisfies_readiness()
                            && !has_observable_readiness(action)
                        {
                            Ok(ActionObservation::already_correct().with_resources(vec![record]))
                        } else {
                            Ok(ActionObservation::requires_change().with_resources(vec![record]))
                        }
                    }
                }
            }
            ActionKind::StartCompose => {
                let _ = self.compose_request(action)?;
                Ok(ActionObservation::requires_change()
                    .with_detail("Docker Compose will reconcile the project"))
            }
            _ => Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker handler received an incompatible action",
            )),
        }
    }
}

impl ActionHandler for DockerActionHandler {
    fn action_key(&self) -> &str {
        self.key
    }

    fn required_capabilities(&self) -> std::collections::BTreeSet<CapabilityId> {
        match self.key {
            "start_container" => [CapabilityId::DockerEngine].into_iter().collect(),
            "start_compose" => [CapabilityId::DockerEngine, CapabilityId::DockerCompose]
                .into_iter()
                .collect(),
            _ => std::collections::BTreeSet::new(),
        }
    }

    fn execution_timeout(
        &self,
        action: &ActionSpec,
        _default_timeout: Duration,
    ) -> Option<Duration> {
        action
            .timeout
            .as_ref()
            .map(|timeout| Duration::from_millis(timeout.milliseconds))
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if !self.action_matches(action) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("handler '{}' received an incompatible action", self.key),
            ));
        }
        action.validate().map_err(WorkstateError::from)?;
        if action.resolved_environment.is_none() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "Docker actions require an environment context",
            ));
        }
        if self.key == "start_compose" {
            let _ = self.compose_request(action)?;
        } else {
            let _ = self.container_request(action)?;
        }
        Ok(())
    }

    fn requires_workspace_target_for_observation(&self, _action: &ActionSpec) -> bool {
        false
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_inner(action, cancellation).await })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move { self.apply_inner(action, cancellation).await })
    }

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.apply(action, cancellation)
    }

    fn run_once_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move {
            let result = self.apply_inner(action, cancellation).await?;
            for item in &result.outputs {
                output.emit(item.clone()).await?;
            }
            Ok(ActionExecutionResult {
                outputs: Vec::new(),
                ..result
            })
        })
    }

    fn wait_for_readiness<'a>(
        &'a self,
        _action: &'a ActionSpec,
        _runner: &'a dyn crate::application::planner::ReadinessCheckRunner,
        _default_timeout: Duration,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ReadinessReport>> {
        Box::pin(async move {
            Ok(ReadinessReport {
                checks_run: 0,
                detail: None,
            })
        })
    }

    fn observe_for_cleanup<'a>(
        &'a self,
        _action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            cancellation.check()?;
            if resources.is_empty() {
                return Ok(ActionObservation::already_correct());
            }
            Ok(ActionObservation::already_correct().with_resources(resources.to_vec()))
        })
    }

    fn compensate<'a>(
        &'a self,
        action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move {
            let request = self.cleanup_request(action, &result.resources)?;
            let outcome = self.docker.stop_owned(request, cancellation).await?;
            docker_compensation_result(outcome)
        })
    }

    fn stop<'a>(
        &'a self,
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move {
            let request = self.cleanup_request(action, resources)?;
            let outcome = self.docker.stop_owned(request, cancellation).await?;
            docker_compensation_result(outcome)
        })
    }
}

pub fn register_handlers(
    registry: &mut ActionHandlerRegistry,
    docker: Arc<dyn DockerBackend>,
    file_system: Arc<dyn FileSystem>,
) -> Result<()> {
    registry.register(DockerActionHandler::new(
        "start_container",
        Arc::clone(&docker),
        Arc::clone(&file_system),
    )?)?;
    registry.register(DockerActionHandler::new(
        "start_compose",
        docker,
        file_system,
    )?)?;
    Ok(())
}

fn docker_compensation_result(outcome: DockerEnsureOutcome) -> Result<CompensationResult> {
    if outcome.status == DockerOperationStatus::Conflict {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker cleanup preserved resources because their ownership could not be proven",
        )
        .with_context(
            "detail",
            outcome
                .detail
                .unwrap_or_else(|| "no additional cleanup detail was returned".to_owned()),
        ));
    }
    Ok(CompensationResult {
        outputs: outcome.outputs.into_iter().map(ActionOutput::log).collect(),
    })
}

fn compose_environment(specification: &crate::domain::ComposeSpec) -> Vec<(String, String)> {
    let mut values = BTreeMap::new();
    for command in [
        specification.up_command.as_ref(),
        specification.down_command.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for (key, value) in &command.environment {
            if key.starts_with("DOCKER_") {
                values.insert(key.clone(), value.clone());
            }
        }
    }
    values.into_iter().collect()
}

fn docker_identity_from_output(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(str::to_owned)
}

fn parse_container_snapshot(bytes: &[u8]) -> Result<DockerContainerSnapshot> {
    let value = serde_json::from_slice::<Value>(bytes).map_err(|source| {
        WorkstateError::with_source(
            ErrorCategory::Integration,
            "Docker returned malformed container data",
            source,
        )
    })?;
    let value = match &value {
        Value::Array(items) => items.first().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "Docker returned an empty container observation",
            )
        })?,
        _ => &value,
    };
    let id = value_string(value, &["Id", "ID", "id"]).ok_or_else(|| {
        WorkstateError::new(
            ErrorCategory::Integration,
            "Docker container data omitted its identity",
        )
    })?;
    let name = value_string(value, &["Name", "name"])
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_owned();
    if name.is_empty() {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker container data omitted its name",
        ));
    }
    let config = value.get("Config").or_else(|| value.get("config"));
    let state_value = value.get("State").or_else(|| value.get("state"));
    let image = config.and_then(|config| value_string(config, &["Image", "image"]));
    let working_directory =
        config.and_then(|config| value_string(config, &["WorkingDir", "working_dir"]));
    let command = config.and_then(parse_command);
    let environment = config.map(parse_environment).unwrap_or_default();
    let mounts = value
        .get("Mounts")
        .or_else(|| value.get("mounts"))
        .map(parse_mounts)
        .unwrap_or_default();
    let ports = value
        .get("NetworkSettings")
        .and_then(|settings| settings.get("Ports"))
        .or_else(|| value.get("ports"))
        .map(parse_ports)
        .unwrap_or_default();
    let state =
        parse_state(state_value.and_then(|state| value_string(state, &["Status", "status"])));
    let health = parse_health(
        state_value
            .and_then(|state| state.get("Health").or_else(|| state.get("health")))
            .and_then(|health| value_string(health, &["Status", "status"])),
    );
    let exit_code = state_value
        .and_then(|state| state.get("ExitCode").or_else(|| state.get("exit_code")))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    Ok(DockerContainerSnapshot {
        id,
        name,
        image,
        command,
        working_directory,
        environment,
        mounts,
        ports,
        state,
        health,
        exit_code,
        status: state_value.and_then(|state| value_string(state, &["Status", "status"])),
    })
}

fn value_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_owned))
}

fn parse_command(config: &Value) -> Option<CommandSpec> {
    let command = config
        .get("Cmd")
        .or_else(|| config.get("cmd"))?
        .as_array()?;
    let program = command.first()?.as_str()?.to_owned();
    let mut specification = CommandSpec::new(program);
    specification.arguments = command
        .iter()
        .skip(1)
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Some(specification)
}

fn parse_environment(config: &Value) -> BTreeMap<String, String> {
    config
        .get("Env")
        .or_else(|| config.get("env"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|value| {
                    value
                        .split_once('=')
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_mounts(value: &Value) -> Vec<crate::application::ports::DockerMountSnapshot> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mount| {
            let source = value_string(mount, &["Source", "source"])?;
            let target = value_string(mount, &["Destination", "destination", "Target", "target"])?;
            let read_only = mount
                .get("RW")
                .or_else(|| mount.get("rw"))
                .and_then(Value::as_bool)
                .is_some_and(|value| !value);
            Some(crate::application::ports::DockerMountSnapshot {
                source,
                target,
                read_only,
            })
        })
        .collect()
}

fn parse_ports(value: &Value) -> Vec<crate::application::ports::DockerPortSnapshot> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let (container, protocol) = key.split_once('/')?;
            let container = container.split('/').next()?.parse::<u16>().ok()?;
            let bindings = value.as_array()?;
            let binding = bindings.first()?.as_object()?;
            let host = binding
                .get("HostPort")
                .or_else(|| binding.get("host_port"))
                .and_then(value_u16)?;
            Some(crate::application::ports::DockerPortSnapshot {
                host,
                container,
                protocol: protocol.to_owned(),
            })
        })
        .collect()
}

fn value_u16(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|value| value.parse::<u16>().ok()))
}

fn parse_state(value: Option<String>) -> DockerContainerState {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "created" => DockerContainerState::Created,
        "running" => DockerContainerState::Running,
        "paused" => DockerContainerState::Paused,
        "restarting" => DockerContainerState::Restarting,
        "exited" => DockerContainerState::Exited,
        "dead" => DockerContainerState::Dead,
        value => DockerContainerState::Unknown(value.to_owned()),
    }
}

fn parse_health(value: Option<String>) -> DockerHealthState {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "healthy" => DockerHealthState::Healthy,
        "unhealthy" => DockerHealthState::Unhealthy,
        "starting" => DockerHealthState::Starting,
        "" => DockerHealthState::None,
        value => DockerHealthState::Unknown(value.to_owned()),
    }
}

fn protocol_suffix(protocol: &str) -> String {
    if protocol == "tcp" {
        String::new()
    } else {
        format!("/{protocol}")
    }
}

fn compose_lock_key(request: &DockerComposeRequest) -> String {
    format!("compose:{}", compose_lock_identity(request))
}

fn compose_lock_identity(request: &DockerComposeRequest) -> String {
    let project_name = request
        .working_directory
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("compose-project");
    models::compose_project_identity(project_name, &request.working_directory)
}

fn cleanup_lock_key(request: &DockerCleanupRequest) -> String {
    if let Some(compose) = &request.compose {
        return compose_lock_key(compose);
    }
    if let Some(specification) = &request.specification {
        return format!("container:{}", specification.name);
    }
    format!("cleanup:{}", request.context.action_id)
}

fn is_missing_container(output: &ProcessOutput) -> bool {
    let text = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    text.contains("no such object")
        || text.contains("no such container")
        || text.contains("not found")
        || text.contains("does not exist")
}

fn is_already_stopped(output: &ProcessOutput) -> bool {
    let text = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    text.contains("is not running")
        || text.contains("not running")
        || text.contains("already stopped")
}

fn has_observable_readiness(action: &ActionSpec) -> bool {
    has_observable_readiness_checks(&action.readiness_checks)
}

fn has_observable_readiness_checks(checks: &[ReadinessCheck]) -> bool {
    checks
        .iter()
        .any(|check| !matches!(check, ReadinessCheck::None))
}

fn is_missing_project(output: &ProcessOutput) -> bool {
    let text = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    text.contains("no such service")
        || text.contains("no container")
        || text.contains("not found")
        || text.contains("does not exist")
}

fn engine_unavailable_after_mutation(
    engine: &crate::application::ports::DockerEngineSnapshot,
) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker Engine became unavailable while applying a Docker action",
    )
    .with_context(
        "detail",
        engine
            .detail
            .clone()
            .unwrap_or_else(|| "no diagnostic was returned".to_owned()),
    )
}

fn container_state_detail(snapshot: &DockerContainerSnapshot) -> String {
    let state = snapshot
        .status
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    match snapshot.exit_code {
        Some(exit_code) => format!("container state is {state} with exit status {exit_code}"),
        None => format!("container state is {state}"),
    }
}

fn compose_state_detail(snapshot: &DockerComposeSnapshot, services: &[String]) -> String {
    let details = snapshot
        .services
        .iter()
        .filter(|service| services.is_empty() || services.contains(&service.name))
        .map(|service| format!("{}:{}", service.name, service_state(service)))
        .collect::<Vec<_>>();
    if details.is_empty() {
        "no requested Compose services were observed".to_owned()
    } else {
        details.join(", ")
    }
}

fn service_state(service: &DockerComposeServiceSnapshot) -> &'static str {
    if !service.state.is_running() {
        return "not running";
    }
    if !service.health.satisfies_readiness() {
        return "not healthy";
    }
    "ready"
}

fn readiness_timeout(check: &ReadinessCheck) -> Duration {
    match check {
        ReadinessCheck::Tcp { timeout, .. }
        | ReadinessCheck::Http { timeout, .. }
        | ReadinessCheck::Command { timeout, .. }
        | ReadinessCheck::Container { timeout, .. }
        | ReadinessCheck::Compose { timeout, .. } => Duration::from_millis(timeout.milliseconds),
        ReadinessCheck::Delay { milliseconds } => Duration::from_millis(*milliseconds),
        ReadinessCheck::None => Duration::from_secs(1),
    }
}

fn readiness_label(check: &ReadinessCheck) -> &'static str {
    match check {
        ReadinessCheck::None => "none",
        ReadinessCheck::Tcp { .. } => "tcp",
        ReadinessCheck::Http { .. } => "http",
        ReadinessCheck::Command { .. } => "command",
        ReadinessCheck::Delay { .. } => "delay",
        ReadinessCheck::Container { .. } => "container",
        ReadinessCheck::Compose { .. } => "compose",
    }
}

fn readiness_timeout_error(
    check: &ReadinessCheck,
    timeout: Duration,
    last_observed: Option<&str>,
) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Runtime,
        format!(
            "Docker readiness check '{}' timed out",
            readiness_label(check)
        ),
    )
    .with_context(
        "last_observed",
        last_observed.unwrap_or("no value was observed").to_owned(),
    )
    .with_context("timeout_milliseconds", timeout.as_millis().to_string())
}
