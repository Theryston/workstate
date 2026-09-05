#[path = "fakes/fake_docker.rs"]
mod fake_docker;
#[path = "fakes/fake_process.rs"]
mod fake_process;

use std::{
    collections::BTreeMap,
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use fake_docker::{DockerCall, FakeDocker};
use fake_process::FakeProcessRunner;
use workstate::{
    application::{
        planner::CancellationToken,
        ports::{
            DockerActionContext, DockerBackend, DockerCleanupRequest, DockerComposeRequest,
            DockerComposeSnapshot, DockerContainerRequest, DockerEngineRequest,
            DockerEngineSnapshot, DockerHealthState, DockerOperationStatus, FileSystem,
            ProcessOutput,
        },
    },
    domain::{CleanupPolicy, ComposeSpec, ContainerSpec, EnvironmentSlug, OwnershipStatus},
    error::ErrorCategory,
    infrastructure::filesystem::local::LocalFileSystem,
    integrations::docker::{
        DockerProcessBackend, checks, desktop::DockerDesktopController,
        engine::DockerEngineController, models,
    },
};

type TestResult = std::result::Result<(), Box<dyn Error>>;
type ValueResult<T> = std::result::Result<T, Box<dyn Error>>;

fn context(action_id: &str) -> ValueResult<DockerActionContext> {
    Ok(DockerActionContext {
        action_id: workstate::domain::ActionId::new(action_id)?,
        environment: EnvironmentSlug::new("docker-tests")?,
        cleanup_policy: CleanupPolicy::OwnedOnly,
    })
}

fn container_request(
    action_id: &str,
    name: &str,
    image: Option<&str>,
) -> ValueResult<DockerContainerRequest> {
    Ok(DockerContainerRequest {
        context: context(action_id)?,
        specification: ContainerSpec {
            name: name.to_owned(),
            image: image.map(str::to_owned),
            command: None,
            environment: BTreeMap::new(),
            mounts: Vec::new(),
            ports: Vec::new(),
        },
        working_directory: None,
        readiness_checks: Vec::new(),
    })
}

fn compose_request(
    action_id: &str,
    project_name: &str,
    working_directory: &str,
) -> ValueResult<DockerComposeRequest> {
    Ok(DockerComposeRequest {
        context: context(action_id)?,
        specification: ComposeSpec {
            project_name: Some(project_name.to_owned()),
            files: vec!["compose.yaml".to_owned()],
            services: Vec::new(),
            up_command: None,
            down_command: None,
        },
        working_directory: PathBuf::from(working_directory),
        readiness_checks: Vec::new(),
    })
}

#[tokio::test]
async fn ready_engine_is_reused_without_claiming_ownership() -> TestResult {
    let docker = FakeDocker::default();
    let request = DockerEngineRequest::for_action(context("engine")?);

    let outcome = docker
        .ensure_engine_ready(request, CancellationToken::new())
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert!(outcome.resources.is_empty());
    assert!(docker.calls()?.contains(&DockerCall::EnsureEngine));
    Ok(())
}

#[tokio::test]
async fn matching_healthy_container_is_reused() -> TestResult {
    let docker = FakeDocker::default();
    docker.insert_container(fake_docker::container_snapshot("web", "nginx:latest", true))?;

    let outcome = docker
        .ensure_container(
            container_request("web", "web", Some("nginx:latest"))?,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    let Some(record) = outcome.resources.first() else {
        return Err("a reused container must produce a resource record".into());
    };
    assert_eq!(record.ownership, OwnershipStatus::ReusedExisting);
    assert!(!record.is_cleanup_candidate());
    Ok(())
}

#[tokio::test]
async fn container_configuration_conflict_is_preserved() -> TestResult {
    let docker = FakeDocker::default();
    docker.insert_container(fake_docker::container_snapshot("web", "redis:latest", true))?;

    let result = docker
        .ensure_container(
            container_request("web", "web", Some("nginx:latest"))?,
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(container) = docker.container("web")? else {
        return Err("the conflicting container must be preserved".into());
    };
    assert_eq!(container.image.as_deref(), Some("redis:latest"));
    Ok(())
}

#[tokio::test]
async fn created_container_is_owned_and_removed_by_cleanup() -> TestResult {
    let docker = FakeDocker::default();
    let request = container_request("web", "web", Some("nginx:latest"))?;
    let outcome = docker
        .ensure_container(request.clone(), CancellationToken::new())
        .await?;
    let Some(record) = outcome.resources.first().cloned() else {
        return Err("a created container must produce a resource record".into());
    };
    assert_eq!(record.ownership, OwnershipStatus::CreatedByCurrentRun);
    assert!(record.is_cleanup_candidate());

    docker
        .stop_owned(
            DockerCleanupRequest {
                context: request.context.clone(),
                specification: Some(request.specification),
                compose: None,
                resources: vec![record],
            },
            CancellationToken::new(),
        )
        .await?;

    assert!(docker.container("web")?.is_none());
    Ok(())
}

#[tokio::test]
async fn pre_existing_container_survives_cleanup() -> TestResult {
    let docker = FakeDocker::default();
    docker.insert_container(fake_docker::container_snapshot("web", "nginx:latest", true))?;
    let request = container_request("web", "web", Some("nginx:latest"))?;
    let outcome = docker
        .ensure_container(request.clone(), CancellationToken::new())
        .await?;

    docker
        .stop_owned(
            DockerCleanupRequest {
                context: request.context,
                specification: Some(request.specification),
                compose: None,
                resources: outcome.resources,
            },
            CancellationToken::new(),
        )
        .await?;

    assert!(docker.container("web")?.is_some());
    Ok(())
}

#[tokio::test]
async fn process_backend_reuses_a_healthy_container_from_docker_inspect() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/container-inspect.json").to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;

    let outcome = backend
        .ensure_container(
            container_request("web", "workstate-web", Some("nginx:latest"))?,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    Ok(())
}

#[tokio::test]
async fn process_backend_creates_and_starts_a_missing_container() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(1),
            stdout: Vec::new(),
            stderr: b"Error: No such object: workstate-web".to_vec(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"created-id\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/container-inspect.json").to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/container-inspect.json").to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;

    let outcome = backend
        .ensure_container(
            container_request("web", "workstate-web", Some("nginx:latest"))?,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Created);
    let requests = runner_view.requests()?;
    assert!(
        requests
            .iter()
            .any(|request| { request.arguments.first().map(String::as_str) == Some("create") })
    );
    assert!(
        requests
            .iter()
            .any(|request| { request.arguments.first().map(String::as_str) == Some("start") })
    );
    Ok(())
}

#[tokio::test]
async fn process_backend_reuses_a_healthy_compose_project() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/compose-ps.json").to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;

    let outcome = backend
        .ensure_compose(
            compose_request("stack", "blog", "/tmp/blog")?,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert_eq!(outcome.resources.len(), 3);
    Ok(())
}

#[tokio::test]
async fn process_backend_removes_an_owned_container_during_cleanup() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/container-inspect.json").to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;
    let request = container_request("web", "workstate-web", Some("nginx:latest"))?;
    let mut snapshot = fake_docker::container_snapshot("workstate-web", "nginx:latest", true);
    snapshot.id = "7f1d6c3a".to_owned();
    let record = models::container_record(
        &request.context,
        &request,
        &snapshot,
        OwnershipStatus::CreatedByCurrentRun,
        false,
    )?;
    let cleanup = DockerCleanupRequest {
        context: request.context.clone(),
        specification: Some(request.specification.clone()),
        compose: None,
        resources: vec![record],
    };

    let outcome = backend
        .stop_owned(cleanup, CancellationToken::new())
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Repaired);
    assert!(
        runner_view
            .requests()?
            .iter()
            .any(|request| { request.arguments.first().map(String::as_str) == Some("rm") })
    );
    Ok(())
}

#[tokio::test]
async fn process_backend_preserves_a_container_with_external_configuration_changes() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/container-inspect.json").to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;
    let request = container_request("web", "workstate-web", Some("nginx:latest"))?;
    let mut snapshot = fake_docker::container_snapshot("workstate-web", "nginx:latest", true);
    snapshot.id = "7f1d6c3a".to_owned();
    let mut record = models::container_record(
        &request.context,
        &request,
        &snapshot,
        OwnershipStatus::CreatedByCurrentRun,
        false,
    )?;
    record
        .integration_metadata
        .insert("configuration_key".to_owned(), "external-change".to_owned());

    let outcome = backend
        .stop_owned(
            DockerCleanupRequest {
                context: request.context.clone(),
                specification: Some(request.specification.clone()),
                compose: None,
                resources: vec![record],
            },
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Conflict);
    assert!(
        !runner_view
            .requests()?
            .iter()
            .any(|request| { request.arguments.first().map(String::as_str) == Some("rm") })
    );
    Ok(())
}

#[tokio::test]
async fn process_backend_uses_compose_down_only_for_fully_owned_projects() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: include_bytes!("fixtures/docker/compose-ps.json").to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let backend = DockerProcessBackend::new(
        Arc::clone(&runner),
        file_system,
        PathBuf::from("docker"),
        None,
        None,
    )?;
    let request = compose_request("stack", "blog", "/tmp/blog")?;
    let snapshot = DockerComposeSnapshot {
        project_name: "blog".to_owned(),
        working_directory: request.working_directory.clone(),
        services: vec![
            workstate::application::ports::DockerComposeServiceSnapshot {
                name: "api".to_owned(),
                container_id: Some("api-container-id".to_owned()),
                state: workstate::application::ports::DockerContainerState::Running,
                health: DockerHealthState::Healthy,
            },
            workstate::application::ports::DockerComposeServiceSnapshot {
                name: "db".to_owned(),
                container_id: Some("db-container-id".to_owned()),
                state: workstate::application::ports::DockerContainerState::Running,
                health: DockerHealthState::Healthy,
            },
        ],
    };
    let mut resources = vec![models::compose_record(
        &request.context,
        &snapshot,
        OwnershipStatus::CreatedByCurrentRun,
    )?];
    resources.extend(models::compose_service_records(
        &request.context,
        &snapshot,
        OwnershipStatus::CreatedByCurrentRun,
    )?);

    let outcome = backend
        .stop_owned(
            DockerCleanupRequest {
                context: request.context.clone(),
                specification: None,
                compose: Some(request),
                resources,
            },
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Repaired);
    assert!(
        runner_view
            .requests()?
            .iter()
            .any(|request| { request.arguments.iter().any(|argument| argument == "down") })
    );
    Ok(())
}

#[tokio::test]
async fn healthy_compose_project_is_reused() -> TestResult {
    let docker = FakeDocker::default();
    let request = compose_request("stack", "blog", "/tmp/blog")?;
    docker.insert_compose(fake_docker::compose_snapshot(
        "blog",
        request.working_directory.clone(),
        &[("api", true), ("db", true)],
    ))?;

    let outcome = docker
        .ensure_compose(request, CancellationToken::new())
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert!(
        outcome
            .resources
            .iter()
            .all(|resource| resource.ownership == OwnershipStatus::ReusedExisting)
    );
    Ok(())
}

#[tokio::test]
async fn compose_project_identity_separates_same_named_projects() -> TestResult {
    let docker = FakeDocker::default();
    let first = compose_request("first", "blog", "/tmp/first")?;
    let second = compose_request("second", "blog", "/tmp/second")?;
    docker.insert_compose(fake_docker::compose_snapshot(
        "blog",
        first.working_directory.clone(),
        &[("api", true)],
    ))?;

    let outcome = docker
        .ensure_compose(second, CancellationToken::new())
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Created);
    assert!(
        docker
            .compose_project("blog", &first.working_directory)?
            .is_some()
    );
    assert!(
        docker
            .compose_project("blog", &PathBuf::from("/tmp/second"))?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn unavailable_engine_is_reported_by_the_fake_port() -> TestResult {
    let docker = FakeDocker::default().with_engine(DockerEngineSnapshot::unavailable(
        "daemon is not responding",
    ));
    let result = docker
        .ensure_container(
            container_request("web", "web", Some("nginx:latest"))?,
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let error = result.err().ok_or("missing Docker engine error")?;
    assert!(error.render().contains("Docker Engine is unavailable"));
    Ok(())
}

#[tokio::test]
async fn engine_launches_desktop_once_when_daemon_becomes_ready() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(1),
            stdout: Vec::new(),
            stderr: b"daemon is not running".to_vec(),
        },
        ProcessOutput {
            status: Some(1),
            stdout: Vec::new(),
            stderr: b"not running".to_vec(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"27.5.1\n".to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let desktop = Arc::new(DockerDesktopController::new(
        Arc::clone(&runner),
        Some(PathBuf::from("/usr/bin/docker-desktop")),
    ));
    let engine =
        DockerEngineController::new(Arc::clone(&runner), PathBuf::from("docker"), desktop)?;

    let outcome = engine
        .ensure_ready(
            DockerEngineRequest {
                launch_desktop_when_needed: true,
                timeout: Duration::from_millis(100),
                poll_interval: Duration::from_millis(1),
                action: context("engine")?,
            },
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Created);
    assert_eq!(runner_view.background_requests()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn readiness_delay_cancels_without_waiting_for_the_full_delay() -> TestResult {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let started = Instant::now();

    let result = checks::delay(10_000, cancellation).await;

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_millis(200));
    Ok(())
}

#[tokio::test]
async fn http_readiness_uses_bounded_curl_arguments_and_redacts_query_values() -> TestResult {
    let runner = FakeProcessRunner::with_responses([ProcessOutput {
        status: Some(0),
        stdout: b"204".to_vec(),
        stderr: Vec::new(),
    }]);
    let runner_view = runner.clone();
    let result = checks::http_check(
        &runner,
        "http://localhost:8080/health?token=secret&mode=ready".to_owned(),
        Some(204),
        Duration::from_millis(200),
        None,
        CancellationToken::new(),
    )
    .await?;

    assert!(result.passed);
    let requests = runner_view.requests()?;
    let Some(request) = requests.first() else {
        return Err("HTTP readiness must execute curl".into());
    };
    assert!(
        request
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "--connect-timeout" && pair[1] == "0.200")
    );
    assert!(
        request
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "--max-time" && pair[1] == "0.200")
    );
    assert!(!request.arguments.iter().any(|value| value == "secret"));
    Ok(())
}

#[test]
fn compose_identity_includes_the_resolved_working_directory() {
    let first = models::compose_project_identity("blog", &PathBuf::from("/tmp/first"));
    let second = models::compose_project_identity("blog", &PathBuf::from("/tmp/second"));

    assert_ne!(first, second);
    assert!(first.contains("/tmp/first"));
}

#[test]
fn compose_health_requires_every_requested_service() {
    let snapshot = DockerComposeSnapshot {
        project_name: "blog".to_owned(),
        working_directory: PathBuf::from("/tmp/blog"),
        services: vec![
            workstate::application::ports::DockerComposeServiceSnapshot {
                name: "api".to_owned(),
                container_id: Some("api-id".to_owned()),
                state: workstate::application::ports::DockerContainerState::Running,
                health: DockerHealthState::None,
            },
        ],
    };

    assert!(!snapshot.is_healthy(&["api".to_owned(), "db".to_owned()]));
}

#[test]
fn docker_errors_redact_secret_bearing_diagnostics() {
    let output = ProcessOutput {
        status: Some(1),
        stdout: Vec::new(),
        stderr: b"password=super-secret token=private-value".to_vec(),
    };
    let error = workstate::integrations::docker::errors::docker_error("inspect", &output);

    assert_eq!(error.category, ErrorCategory::Integration);
    let rendered = error.render();
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("private-value"));
}

#[test]
fn cleanup_policy_preserve_is_not_a_cleanup_candidate() -> TestResult {
    let identity = workstate::domain::ResourceIdentity::new(
        workstate::domain::ResourceKind::DockerContainer,
        "container-id",
    )?;
    let mut record =
        workstate::domain::ResourceRecord::new(identity, OwnershipStatus::CreatedByCurrentRun);
    record.cleanup_policy = CleanupPolicy::Preserve;

    assert!(!record.is_cleanup_candidate());
    Ok(())
}
