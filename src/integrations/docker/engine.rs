use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    application::{
        planner::CancellationToken,
        ports::{
            DockerEngineRequest, DockerEngineSnapshot, DockerEnsureOutcome, DockerOperationStatus,
            ProcessOutput, ProcessRequest, ProcessRunner,
        },
    },
    domain::OwnershipStatus,
    error::{ErrorCategory, Result, WorkstateError},
};

use super::desktop::{DockerDesktopController, SystemServiceStatus};

const DOCKER_ENVIRONMENT_KEYS: [&str; 7] = [
    "DOCKER_API_VERSION",
    "DOCKER_CERT_PATH",
    "DOCKER_CONFIG",
    "DOCKER_CONTEXT",
    "DOCKER_HOST",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CUSTOM_HEADERS",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DockerCliEnvironment {
    variables: Vec<(String, String)>,
}

impl DockerCliEnvironment {
    pub fn from_process() -> Self {
        let variables = DOCKER_ENVIRONMENT_KEYS
            .into_iter()
            .filter_map(|key| {
                std::env::var_os(key)
                    .map(|value| (key.to_owned(), value.to_string_lossy().into_owned()))
            })
            .filter(|(_, value)| !value.is_empty())
            .collect();
        Self::from_variables(variables)
    }

    pub fn from_variables(variables: Vec<(String, String)>) -> Self {
        let mut normalized = BTreeMap::new();
        for (key, value) in variables {
            if DOCKER_ENVIRONMENT_KEYS.contains(&key.as_str()) && !value.is_empty() {
                normalized.insert(key, value);
            }
        }
        Self {
            variables: normalized.into_iter().collect(),
        }
    }

    pub fn variables(&self) -> Vec<(String, String)> {
        self.variables.clone()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.variables
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn merge(&self, overrides: &[(String, String)]) -> Vec<(String, String)> {
        let mut values = self.variables.iter().cloned().collect::<BTreeMap<_, _>>();
        for (key, value) in overrides {
            values.insert(key.clone(), value.clone());
        }
        values.into_iter().collect()
    }
}

#[derive(Clone)]
pub struct DockerEngineController {
    process_runner: Arc<dyn ProcessRunner>,
    docker_program: PathBuf,
    desktop: Arc<DockerDesktopController>,
    environment: DockerCliEnvironment,
    linux_user_services: bool,
    lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockerEndpointKind {
    Desktop,
    Rootless,
    Global,
    Remote,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DockerEndpoint {
    kind: DockerEndpointKind,
    context: Option<String>,
    endpoint: Option<String>,
}

struct DockerReadinessWait {
    endpoint: DockerEndpoint,
    initial: EngineProbe,
    environment: Vec<(String, String)>,
    timeout: Duration,
    poll_interval: Duration,
    deadline: tokio::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EngineProbe {
    Ready { version: String },
    Unavailable { detail: String },
}

impl EngineProbe {
    fn detail(&self) -> Option<&str> {
        match self {
            Self::Ready { .. } => None,
            Self::Unavailable { detail } => Some(detail),
        }
    }
}

impl DockerEngineController {
    pub fn new(
        process_runner: Arc<dyn ProcessRunner>,
        docker_program: PathBuf,
        desktop: Arc<DockerDesktopController>,
    ) -> Result<Self> {
        Self::new_for_platform(
            process_runner,
            docker_program,
            desktop,
            cfg!(target_os = "linux"),
        )
    }

    pub fn new_for_platform(
        process_runner: Arc<dyn ProcessRunner>,
        docker_program: PathBuf,
        desktop: Arc<DockerDesktopController>,
        linux_user_services: bool,
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
            environment: DockerCliEnvironment::from_process(),
            linux_user_services,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn with_environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = DockerCliEnvironment::from_variables(environment);
        self
    }

    pub fn docker_program(&self) -> &Path {
        &self.docker_program
    }

    pub fn environment(&self) -> Vec<(String, String)> {
        self.environment.variables()
    }

    pub fn complete_process_request(&self, mut request: ProcessRequest) -> ProcessRequest {
        request.environment = self.environment.merge(&request.environment);
        request
    }

    pub async fn inspect(&self, cancellation: CancellationToken) -> Result<DockerEngineSnapshot> {
        self.inspect_with_environment(cancellation, self.environment.variables())
            .await
    }

    pub async fn inspect_with_environment(
        &self,
        cancellation: CancellationToken,
        environment: Vec<(String, String)>,
    ) -> Result<DockerEngineSnapshot> {
        let environment = self.environment.merge(&environment);
        match self.probe(environment, cancellation).await? {
            EngineProbe::Ready { version } => Ok(DockerEngineSnapshot::ready(version)),
            EngineProbe::Unavailable { detail } => Ok(DockerEngineSnapshot::unavailable(detail)),
        }
    }

    pub async fn ensure_ready(
        &self,
        request: DockerEngineRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        validate_wait_bounds(request.timeout, request.poll_interval)?;
        let _guard = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    "operation was cancelled while waiting for Docker readiness",
                ).with_context("cancelled", "true"));
            }
            guard = self.lock.lock() => guard,
        };
        let environment = self.environment.merge(&request.environment);
        let deadline = tokio::time::Instant::now() + request.timeout;
        let initial = match self
            .probe_with_timeout(&environment, request.timeout, cancellation.clone())
            .await
        {
            Ok(probe) => probe,
            Err(error) => return Err(error),
        };
        if let EngineProbe::Ready { version } = &initial {
            return Ok(DockerEnsureOutcome::new(DockerOperationStatus::Reused)
                .with_output(engine_output(version)));
        }
        if !request.launch_desktop_when_needed {
            return Err(engine_unavailable_error(
                initial.detail(),
                "Docker Engine launch was disabled for this operation",
            ));
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(engine_timeout_error(
                "Docker endpoint detection did not finish before the timeout",
                request.timeout,
            ));
        }
        let endpoint = tokio::time::timeout(
            remaining,
            self.detect_endpoint(&environment, cancellation.clone()),
        )
        .await
        .map_err(|_| {
            engine_timeout_error(
                "Docker endpoint detection did not finish before the timeout",
                request.timeout,
            )
        })??;
        let wait = DockerReadinessWait {
            endpoint: endpoint.clone(),
            initial: initial.clone(),
            environment: environment.clone(),
            timeout: request.timeout,
            poll_interval: request.poll_interval,
            deadline,
        };
        match endpoint.kind {
            DockerEndpointKind::Remote => {
                return Err(remote_engine_error(&endpoint, initial.detail()));
            }
            DockerEndpointKind::Unknown => {
                return Err(unknown_engine_error(&endpoint, initial.detail()));
            }
            DockerEndpointKind::Global => {
                return self.handle_global_engine(wait, cancellation).await;
            }
            DockerEndpointKind::Desktop | DockerEndpointKind::Rootless => {}
        }

        let resource = match endpoint.kind {
            DockerEndpointKind::Desktop => {
                self.desktop
                    .ensure_started(&request.action, cancellation.clone())
                    .await?
            }
            DockerEndpointKind::Rootless => {
                self.desktop
                    .ensure_rootless_started(&request.action, cancellation.clone())
                    .await?
            }
            DockerEndpointKind::Global
            | DockerEndpointKind::Remote
            | DockerEndpointKind::Unknown => None,
        };
        let Some(resource) = resource else {
            return Err(local_service_unavailable_error(&endpoint, initial.detail()));
        };
        self.wait_for_ready(wait, Some(resource), cancellation)
            .await
    }

    async fn probe_with_timeout(
        &self,
        environment: &[(String, String)],
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<EngineProbe> {
        tokio::select! {
            _ = cancellation.cancelled() => Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "operation was cancelled while inspecting Docker Engine",
            ).with_context("cancelled", "true")),
            result = tokio::time::timeout(
                timeout,
                self.probe(environment.to_vec(), cancellation.clone()),
            ) => match result {
                Ok(result) => result,
                Err(_) => Err(engine_timeout_error(
                    "Docker Engine inspection timed out",
                    timeout,
                )),
            },
        }
    }

    async fn probe(
        &self,
        environment: Vec<(String, String)>,
        cancellation: CancellationToken,
    ) -> Result<EngineProbe> {
        cancellation.check()?;
        let output = self
            .run_unchecked(
                vec![
                    "info".to_owned(),
                    "--format".to_owned(),
                    "{{.ServerVersion}}".to_owned(),
                ],
                None,
                environment,
            )
            .await;
        match output {
            Ok(output) if output.succeeded() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                Ok(EngineProbe::Ready {
                    version: if version.is_empty() {
                        "unknown".to_owned()
                    } else {
                        version
                    },
                })
            }
            Ok(output) => Ok(EngineProbe::Unavailable {
                detail: diagnostic_from_output(&output),
            }),
            Err(error) if is_missing_executable(&error) => Err(docker_cli_missing_error()),
            Err(error) => Err(error
                .with_context("operation", "inspect Docker Engine")
                .with_context("docker_command", "docker info")),
        }
    }

    async fn detect_endpoint(
        &self,
        environment: &[(String, String)],
        cancellation: CancellationToken,
    ) -> Result<DockerEndpoint> {
        cancellation.check()?;
        let environment_view = DockerCliEnvironment::from_variables(environment.to_vec());
        if environment_view.get("DOCKER_CONTEXT").is_none()
            && let Some(host) = environment_view.get("DOCKER_HOST")
        {
            return classify_endpoint(None, Some(host.to_owned()), self.linux_user_services);
        }

        let context_output = self
            .run_unchecked(
                vec!["context".to_owned(), "show".to_owned()],
                None,
                environment.to_vec(),
            )
            .await
            .map_err(|error| {
                if is_missing_executable(&error) {
                    docker_cli_missing_error()
                } else {
                    error
                        .with_context("operation", "inspect Docker context")
                        .with_context("docker_command", "docker context show")
                }
            })?;
        if !context_output.succeeded() {
            return Err(context_command_error(
                "docker context show",
                &context_output,
            ));
        }
        let context = String::from_utf8_lossy(&context_output.stdout)
            .trim()
            .to_owned();
        if context.is_empty() || context.chars().any(char::is_control) {
            return Err(invalid_context_error(
                &context,
                "Docker context discovery returned an empty or invalid name",
            ));
        }

        let inspect_output = self
            .run_unchecked(
                vec![
                    "context".to_owned(),
                    "inspect".to_owned(),
                    context.clone(),
                    "--format".to_owned(),
                    "{{json .}}".to_owned(),
                ],
                None,
                environment.to_vec(),
            )
            .await
            .map_err(|error| {
                if is_missing_executable(&error) {
                    docker_cli_missing_error()
                } else {
                    error
                        .with_context("operation", "inspect Docker context")
                        .with_context("context", context.clone())
                }
            })?;
        if !inspect_output.succeeded() {
            return Err(
                context_command_error("docker context inspect", &inspect_output)
                    .with_context("context", context),
            );
        }
        let endpoint = parse_context_endpoint(&inspect_output.stdout)?;
        classify_endpoint(Some(context), endpoint, self.linux_user_services)
    }

    async fn handle_global_engine(
        &self,
        wait: DockerReadinessWait,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        let service_status = self
            .desktop
            .inspect_system_service("docker", cancellation.clone())
            .await;
        if is_permission_denied(wait.initial.detail()) {
            return Err(global_permission_error(
                &wait.endpoint,
                wait.initial.detail(),
            ));
        }
        match service_status {
            Ok(SystemServiceStatus::Active | SystemServiceStatus::Starting) => {
                self.wait_for_ready(wait, None, cancellation).await
            }
            Ok(SystemServiceStatus::Inactive) => {
                Err(global_stopped_error(&wait.endpoint, wait.initial.detail()))
            }
            Err(error) => Err(global_inspection_error(
                &wait.endpoint,
                wait.initial.detail(),
                error,
            )),
        }
    }

    async fn wait_for_ready(
        &self,
        wait: DockerReadinessWait,
        resource: Option<crate::domain::ResourceRecord>,
        cancellation: CancellationToken,
    ) -> Result<DockerEnsureOutcome> {
        let mut last = wait.initial;
        loop {
            if let Err(error) = cancellation.check() {
                return Err(self
                    .cleanup_service_after_failure(resource.as_ref(), error)
                    .await);
            }
            let now = tokio::time::Instant::now();
            if now >= wait.deadline {
                let error = engine_timeout_error(
                    last.detail()
                        .unwrap_or("Docker Engine did not become ready"),
                    wait.timeout,
                )
                .with_context("endpoint_kind", endpoint_kind_name(wait.endpoint.kind));
                return Err(self
                    .cleanup_service_after_failure(resource.as_ref(), error)
                    .await);
            }
            let remaining = wait.deadline.saturating_duration_since(now);
            let inspected = tokio::time::timeout(
                remaining,
                self.probe(wait.environment.clone(), cancellation.clone()),
            )
            .await;
            last = match inspected {
                Ok(Ok(probe)) => probe,
                Ok(Err(error)) => {
                    return Err(self
                        .cleanup_service_after_failure(resource.as_ref(), error)
                        .await);
                }
                Err(_) => {
                    let error = engine_timeout_error(
                        last.detail()
                            .unwrap_or("Docker Engine inspection timed out"),
                        wait.timeout,
                    )
                    .with_context("endpoint_kind", endpoint_kind_name(wait.endpoint.kind));
                    return Err(self
                        .cleanup_service_after_failure(resource.as_ref(), error)
                        .await);
                }
            };
            if let EngineProbe::Ready { version } = &last {
                let status = match resource.as_ref() {
                    Some(resource)
                        if resource.ownership == OwnershipStatus::CreatedByCurrentRun =>
                    {
                        DockerOperationStatus::Created
                    }
                    Some(_) | None => DockerOperationStatus::Reused,
                };
                let resources = resource.into_iter().collect();
                return Ok(DockerEnsureOutcome::new(status)
                    .with_resources(resources)
                    .with_output(engine_output(version)));
            }
            let remaining = wait
                .deadline
                .saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                continue;
            }
            tokio::select! {
                _ = cancellation.cancelled() => {
                    let error = WorkstateError::new(
                        ErrorCategory::Runtime,
                        "operation was cancelled while waiting for Docker Engine",
                    ).with_context("cancelled", "true");
                    return Err(self
                        .cleanup_service_after_failure(resource.as_ref(), error)
                        .await);
                }
                _ = tokio::time::sleep(wait.poll_interval.min(remaining)) => {}
            }
        }
    }

    async fn cleanup_service_after_failure(
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
            Ok(_) => error.with_context("service_cleanup", "completed"),
            Err(cleanup_error) => {
                error.with_context("service_cleanup_error", cleanup_error.render())
            }
        }
    }

    pub async fn run(
        &self,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
    ) -> Result<ProcessOutput> {
        self.run_unchecked(arguments, working_directory, self.environment.variables())
            .await
    }

    pub async fn run_with_environment(
        &self,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
        environment: Vec<(String, String)>,
    ) -> Result<ProcessOutput> {
        self.run_unchecked(
            arguments,
            working_directory,
            self.environment.merge(&environment),
        )
        .await
    }

    async fn run_unchecked(
        &self,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
        environment: Vec<(String, String)>,
    ) -> Result<ProcessOutput> {
        self.run_process(ProcessRequest {
            program: self.docker_program.to_string_lossy().into_owned(),
            arguments,
            working_directory,
            environment,
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

fn engine_output(version: &str) -> String {
    format!("Docker Engine is ready ({version})")
}

fn engine_unavailable_error(detail: Option<&str>, reason: &str) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Integration, "Docker Engine is unavailable")
        .with_context("reason", reason)
        .with_context(
            "detail",
            detail
                .unwrap_or("Docker Engine did not return a diagnostic")
                .to_owned(),
        )
}

fn engine_timeout_error(detail: &str, timeout: Duration) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker Engine did not become ready before the timeout",
    )
    .with_context("detail", detail)
    .with_context("timeout_milliseconds", timeout.as_millis().to_string())
}

fn docker_cli_missing_error() -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker CLI is not installed or could not be executed",
    )
    .with_context("next_action", "Install Docker CLI and run Workstate again")
}

fn is_missing_executable(error: &WorkstateError) -> bool {
    error
        .source
        .as_deref()
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|source| source.kind() == ErrorKind::NotFound)
}

fn classify_endpoint(
    context: Option<String>,
    endpoint: Option<String>,
    linux_user_services: bool,
) -> Result<DockerEndpoint> {
    let context_name = context.as_deref().unwrap_or_default();
    let context_lower = context_name.to_ascii_lowercase();
    let endpoint_lower = endpoint.as_deref().unwrap_or_default().to_ascii_lowercase();
    let kind = if endpoint_lower.starts_with("tcp://")
        || endpoint_lower.starts_with("ssh://")
        || endpoint_lower.starts_with("http://")
        || endpoint_lower.starts_with("https://")
    {
        DockerEndpointKind::Remote
    } else if endpoint_lower.starts_with("unix://") {
        let path = endpoint_lower.trim_start_matches("unix://");
        if path.contains("/run/user/") || path.contains("rootless") {
            DockerEndpointKind::Rootless
        } else if path.contains("/.docker/desktop/") || path.contains("docker/desktop") {
            DockerEndpointKind::Desktop
        } else if path.is_empty() || path == "/var/run/docker.sock" || path == "/run/docker.sock" {
            DockerEndpointKind::Global
        } else {
            DockerEndpointKind::Unknown
        }
    } else if endpoint_lower.starts_with("fd://") {
        DockerEndpointKind::Global
    } else if endpoint_lower.starts_with("npipe://") {
        if linux_user_services {
            DockerEndpointKind::Remote
        } else {
            DockerEndpointKind::Desktop
        }
    } else if endpoint.is_some()
        && (context_lower == "desktop-linux" || context_lower.contains("docker-desktop"))
    {
        DockerEndpointKind::Desktop
    } else if endpoint.is_some() && context_lower.contains("rootless") {
        DockerEndpointKind::Rootless
    } else if endpoint.is_some() {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker host configuration is invalid",
        )
        .with_context(
            "endpoint",
            redact_endpoint(endpoint.as_deref().unwrap_or_default()),
        ));
    } else if context_lower == "desktop-linux" || context_lower.contains("docker-desktop") {
        DockerEndpointKind::Desktop
    } else if context_lower.contains("rootless") {
        DockerEndpointKind::Rootless
    } else if context_lower.is_empty() || context_lower == "default" {
        DockerEndpointKind::Global
    } else {
        DockerEndpointKind::Unknown
    };
    Ok(DockerEndpoint {
        kind,
        context,
        endpoint: endpoint.map(|value| redact_endpoint(&value)),
    })
}

fn parse_context_endpoint(bytes: &[u8]) -> Result<Option<String>> {
    let value = serde_json::from_slice::<Value>(bytes).map_err(|source| {
        WorkstateError::with_source(
            ErrorCategory::Integration,
            "Docker context inspection returned malformed data",
            source,
        )
    })?;
    let context = match &value {
        Value::Array(values) => values.first().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "Docker context inspection returned no context data",
            )
        })?,
        Value::Object(_) => &value,
        _ => {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Docker context inspection returned an unexpected value",
            ));
        }
    };
    let endpoint = context
        .get("Endpoints")
        .or_else(|| context.get("endpoints"))
        .and_then(|value| value.get("docker"))
        .and_then(|value| value.get("Host").or_else(|| value.get("host")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if endpoint
        .as_deref()
        .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "Docker context inspection returned an invalid endpoint",
        ));
    }
    Ok(endpoint)
}

fn context_command_error(operation: &str, output: &ProcessOutput) -> WorkstateError {
    let detail = diagnostic_from_output(output);
    let lower = detail.to_ascii_lowercase();
    if lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("invalid context")
    {
        return invalid_context_error("unknown", &detail);
    }
    WorkstateError::new(
        ErrorCategory::Integration,
        format!("Docker context operation '{operation}' failed"),
    )
    .with_context("detail", detail)
}

fn invalid_context_error(context: &str, detail: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "The selected Docker context is invalid or does not exist",
    )
    .with_context("context", context)
    .with_context("detail", detail)
}

fn remote_engine_error(endpoint: &DockerEndpoint, detail: Option<&str>) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "The selected Docker context points to a remote Engine that is unavailable; Workstate will not start local Docker services",
    )
    .with_context(
        "endpoint",
        endpoint
            .endpoint
            .clone()
            .unwrap_or_else(|| "remote".to_owned()),
    )
    .with_context(
        "detail",
        detail
            .unwrap_or("the remote Docker host did not respond")
            .to_owned(),
    )
    .with_context(
        "next_action",
        "Check the selected Docker context or remote host and run Workstate again",
    )
}

fn unknown_engine_error(endpoint: &DockerEndpoint, detail: Option<&str>) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker Engine availability could not be determined safely",
    )
    .with_context(
        "context",
        endpoint
            .context
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
    )
    .with_context(
        "endpoint",
        endpoint
            .endpoint
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
    )
    .with_context(
        "detail",
        detail.unwrap_or("Docker returned no diagnostic").to_owned(),
    )
    .with_context(
        "next_action",
        "Inspect Docker context configuration manually",
    )
}

fn local_service_unavailable_error(
    endpoint: &DockerEndpoint,
    detail: Option<&str>,
) -> WorkstateError {
    let service = match endpoint.kind {
        DockerEndpointKind::Desktop => "Docker Desktop",
        DockerEndpointKind::Rootless => "the rootless Docker user service",
        DockerEndpointKind::Global | DockerEndpointKind::Remote | DockerEndpointKind::Unknown => {
            "the selected Docker service"
        }
    };
    WorkstateError::new(
        ErrorCategory::Integration,
        format!("{service} is not installed or cannot be started safely"),
    )
    .with_context(
        "detail",
        detail
            .unwrap_or("no compatible local user service was found")
            .to_owned(),
    )
    .with_context(
        "next_action",
        "Start the selected Docker service manually and run Workstate again",
    )
}

fn global_permission_error(endpoint: &DockerEndpoint, detail: Option<&str>) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker Engine is running, but the current user cannot access the selected socket",
    )
    .with_context(
        "endpoint",
        endpoint
            .endpoint
            .clone()
            .unwrap_or_else(|| "global socket".to_owned()),
    )
    .with_context(
        "detail",
        detail
            .unwrap_or("permission was denied by Docker")
            .to_owned(),
    )
    .with_context(
        "next_action",
        "Configure Docker access for your user, then sign in again: sudo usermod -aG docker $USER",
    )
}

fn global_stopped_error(endpoint: &DockerEndpoint, detail: Option<&str>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Integration, "Docker Engine is not running")
        .with_context(
            "endpoint",
            endpoint
                .endpoint
                .clone()
                .unwrap_or_else(|| "global socket".to_owned()),
        )
        .with_context(
            "detail",
            detail
                .unwrap_or("the global Docker service is inactive")
                .to_owned(),
        )
        .with_context(
            "next_action",
            "Start it manually with: sudo systemctl start docker",
        )
}

fn global_inspection_error(
    endpoint: &DockerEndpoint,
    detail: Option<&str>,
    inspection_error: WorkstateError,
) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Integration,
        "Docker Engine availability could not be verified safely",
    )
    .with_context(
        "endpoint",
        endpoint
            .endpoint
            .clone()
            .unwrap_or_else(|| "global socket".to_owned()),
    )
    .with_context("detail", detail.unwrap_or("docker info failed").to_owned())
    .with_context("service_inspection_error", inspection_error.render())
    .with_context(
        "next_action",
        "Start or inspect Docker manually, then run Workstate again",
    )
}

fn is_permission_denied(detail: Option<&str>) -> bool {
    detail.is_some_and(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("permission denied")
            || lower.contains("got permission denied")
            || lower.contains("access denied")
    })
}

fn redact_endpoint(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return super::errors::sanitized_output(value.as_bytes())
            .unwrap_or_else(|| "unknown".to_owned());
    };
    if let Some((_, host)) = rest.rsplit_once('@') {
        return format!("{scheme}://[redacted]@{host}");
    }
    super::errors::sanitized_output(value.as_bytes()).unwrap_or_else(|| "unknown".to_owned())
}

fn endpoint_kind_name(kind: DockerEndpointKind) -> &'static str {
    match kind {
        DockerEndpointKind::Desktop => "docker_desktop",
        DockerEndpointKind::Rootless => "rootless",
        DockerEndpointKind::Global => "global",
        DockerEndpointKind::Remote => "remote",
        DockerEndpointKind::Unknown => "unknown",
    }
}
