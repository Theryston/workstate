#[path = "fakes/fake_docker.rs"]
mod fake_docker;
#[path = "fakes/fake_process.rs"]
mod fake_process;

use std::{
    collections::BTreeMap,
    error::Error,
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use fake_docker::{DockerCall, FakeDocker};
use fake_process::FakeProcessRunner;
use workstate::{
    application::{
        planner::{ActionHandler, CancellationToken},
        ports::{
            DockerActionContext, DockerBackend, DockerCleanupRequest, DockerComposeRequest,
            DockerComposeSnapshot, DockerContainerRequest, DockerEngineRequest,
            DockerEngineSnapshot, DockerHealthState, DockerOperationStatus, FileSystem,
            ProcessOutput,
        },
    },
    domain::{
        ActionKind, ActionSpec, CleanupPolicy, ComposeSpec, ContainerSpec, EnvironmentSlug,
        OwnershipStatus, ResourceIdentity, ResourceKind, ResourceRecord, Timeout,
    },
    error::{ErrorCategory, WorkstateError},
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

fn compose_request(action_id: &str, working_directory: &str) -> ValueResult<DockerComposeRequest> {
    Ok(DockerComposeRequest {
        context: context(action_id)?,
        specification: ComposeSpec {
            compose_file: Some("compose.yaml".to_owned()),
            services: Vec::new(),
            up_command: None,
            down_command: None,
        },
        working_directory: PathBuf::from(working_directory),
        readiness_checks: Vec::new(),
        environment: Vec::new(),
    })
}

fn process_output(status: i32, stdout: &str, stderr: &str) -> ProcessOutput {
    ProcessOutput {
        status: Some(status),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn engine_context_responses(
    context_name: &str,
    endpoint: &str,
    initial_detail: &str,
) -> Vec<ProcessOutput> {
    vec![
        process_output(1, "", initial_detail),
        process_output(0, &format!("{context_name}\n"), ""),
        process_output(
            0,
            &format!(
                "{{\"Name\":\"{}\",\"Endpoints\":{{\"docker\":{{\"Host\":\"{}\"}}}}}}",
                context_name, endpoint,
            ),
            "",
        ),
    ]
}

fn process_engine(
    runner: FakeProcessRunner,
    environment: Vec<(String, String)>,
) -> ValueResult<(FakeProcessRunner, DockerEngineController)> {
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let desktop = Arc::new(DockerDesktopController::new_for_platform(
        Arc::clone(&runner),
        true,
    ));
    let engine = DockerEngineController::new_for_platform(
        Arc::clone(&runner),
        PathBuf::from("docker"),
        desktop,
        true,
    )?
    .with_environment(environment);
    Ok((runner_view, engine))
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

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    let requests = runner_view.requests()?;
    assert_eq!(
        requests
            .first()
            .and_then(|request| request.arguments.first())
            .map(String::as_str),
        Some("info")
    );
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
    assert_eq!(
        requests
            .first()
            .and_then(|request| request.arguments.first())
            .map(String::as_str),
        Some("info")
    );
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
async fn process_backend_runs_compose_up_for_a_healthy_project() -> TestResult {
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
            stdout: include_bytes!("fixtures/docker/compose-ps.json").to_vec(),
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
        .ensure_compose(
            compose_request("stack", "/tmp/blog")?,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Repaired);
    assert_eq!(outcome.resources.len(), 1);
    let requests = runner_view.requests()?;
    assert_eq!(
        requests
            .first()
            .and_then(|request| request.arguments.first())
            .map(String::as_str),
        Some("info")
    );
    assert!(
        requests
            .iter()
            .any(|request| request.arguments.iter().any(|argument| argument == "up"))
    );
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
async fn process_backend_uses_compose_down_without_service_identity_comparison() -> TestResult {
    let runner = FakeProcessRunner::default();
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
    let request = compose_request("stack", "/tmp/blog")?;
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
    let project = models::compose_record(
        &request.context,
        &snapshot,
        OwnershipStatus::CreatedByCurrentRun,
    )?;
    let rotated_service = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DockerContainer, "rotated-service-id")?,
        OwnershipStatus::CreatedByCurrentRun,
    )
    .with_action(request.context.action_id.clone());
    let resources = vec![project, rotated_service];

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
    let requests = runner_view.requests()?;
    assert!(
        requests
            .iter()
            .any(|request| { request.arguments.iter().any(|argument| argument == "down") })
    );
    Ok(())
}

#[tokio::test]
async fn healthy_compose_project_is_reused() -> TestResult {
    let docker = FakeDocker::default();
    let request = compose_request("stack", "/tmp/blog")?;
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

#[test]
fn docker_start_actions_do_not_inherit_the_scheduler_action_timeout() -> TestResult {
    let docker: Arc<dyn workstate::application::ports::DockerBackend> =
        Arc::new(FakeDocker::default());
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let handler = workstate::integrations::docker::DockerActionHandler::new(
        "start_container",
        docker,
        file_system,
    )?;
    let action = ActionSpec::new("start-container", ActionKind::StartContainer)?;

    assert_eq!(
        handler.execution_timeout(&action, Duration::from_secs(30)),
        None
    );

    let mut bounded_action = action;
    bounded_action.timeout = Some(Timeout::new(60_000)?);
    assert_eq!(
        handler.execution_timeout(&bounded_action, Duration::from_secs(30)),
        Some(Duration::from_secs(60))
    );

    let compose_handler = workstate::integrations::docker::DockerActionHandler::new(
        "start_compose",
        Arc::new(FakeDocker::default()),
        Arc::new(LocalFileSystem),
    )?;
    let compose_action = ActionSpec::new("start-compose", ActionKind::StartCompose)?;
    assert_eq!(
        compose_handler.execution_timeout(&compose_action, Duration::from_secs(30)),
        None
    );
    Ok(())
}

#[tokio::test]
async fn compose_project_identity_separates_same_named_projects() -> TestResult {
    let docker = FakeDocker::default();
    let first = compose_request("first", "/tmp/first")?;
    let second = compose_request("second", "/tmp/second")?;
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
            .compose_project("second", &PathBuf::from("/tmp/second"))?
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
            status: Some(0),
            stdout: b"desktop-linux\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: br#"{"Name":"desktop-linux","Endpoints":{"docker":{"Host":"unix:///home/test/.docker/desktop/docker.sock"}}}"#.to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: b"loaded\n".to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(3),
            stdout: b"inactive\n".to_vec(),
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
                environment: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Created);
    assert!(runner_view.requests()?.iter().any(|request| {
        request.program == "systemctl"
            && request.arguments
                == vec![
                    "--user".to_owned(),
                    "start".to_owned(),
                    "docker-desktop".to_owned(),
                ]
    }));
    Ok(())
}

#[tokio::test]
async fn process_engine_probe_reuses_an_available_engine_without_system_services() -> TestResult {
    let runner = FakeProcessRunner::with_responses([process_output(0, "27.5.1\n", "")]);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let outcome = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("available-engine")?),
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert!(outcome.resources.is_empty());
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| request.program != "systemctl")
    );
    Ok(())
}

#[tokio::test]
async fn docker_desktop_startup_timeout_is_reported_and_cleaned_up() -> TestResult {
    let mut responses = engine_context_responses(
        "desktop-linux",
        "unix:///home/test/.docker/desktop/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.extend([
        process_output(0, "loaded\n", ""),
        process_output(3, "inactive\n", ""),
        process_output(0, "", ""),
    ]);
    for _ in 0..40 {
        responses.push(process_output(
            1,
            "",
            "Docker Desktop is still initializing",
        ));
    }
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let mut request = DockerEngineRequest::for_action(context("desktop-timeout")?);
    request.timeout = Duration::from_millis(15);
    request.poll_interval = Duration::from_millis(1);
    let result = engine.ensure_ready(request, CancellationToken::new()).await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("Docker Desktop timeout must return an error".into());
    };
    assert!(
        error
            .render()
            .contains("did not become ready before the timeout")
    );
    assert!(runner_view.requests()?.iter().any(|request| {
        request.program == "systemctl"
            && request.arguments
                == vec![
                    "--user".to_owned(),
                    "stop".to_owned(),
                    "docker-desktop".to_owned(),
                ]
    }));
    Ok(())
}

#[tokio::test]
async fn already_starting_desktop_service_is_waited_for_without_claiming_ownership() -> TestResult {
    let mut responses = engine_context_responses(
        "desktop-linux",
        "unix:///home/test/.docker/desktop/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.extend([
        process_output(0, "loaded\n", ""),
        process_output(3, "activating\n", ""),
        process_output(0, "27.5.1\n", ""),
    ]);
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let outcome = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("desktop-starting")?),
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert!(
        outcome
            .resources
            .iter()
            .all(|resource| { resource.ownership == OwnershipStatus::PreExisting })
    );
    assert!(runner_view.requests()?.iter().all(|request| {
        request.arguments
            != vec![
                "--user".to_owned(),
                "start".to_owned(),
                "docker-desktop".to_owned(),
            ]
    }));
    Ok(())
}

#[tokio::test]
async fn rootless_engine_is_started_through_the_user_service() -> TestResult {
    let mut responses = engine_context_responses(
        "rootless",
        "unix:///run/user/1000/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.extend([
        process_output(0, "loaded\n", ""),
        process_output(3, "inactive\n", ""),
        process_output(0, "", ""),
        process_output(0, "27.5.1\n", ""),
    ]);
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let mut request = DockerEngineRequest::for_action(context("rootless-engine")?);
    request.timeout = Duration::from_millis(100);
    request.poll_interval = Duration::from_millis(1);
    let outcome = engine
        .ensure_ready(request, CancellationToken::new())
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Created);
    assert!(outcome.resources.iter().any(|resource| {
        resource.resource.kind == ResourceKind::DockerEngine
            && resource
                .integration_metadata
                .get("service_name")
                .is_some_and(|name| name == "docker")
    }));
    assert!(runner_view.requests()?.iter().any(|request| {
        request.program == "systemctl"
            && request.arguments
                == vec!["--user".to_owned(), "start".to_owned(), "docker".to_owned()]
    }));
    Ok(())
}

#[tokio::test]
async fn starting_global_engine_is_waited_for_without_local_startup() -> TestResult {
    let mut responses = engine_context_responses(
        "default",
        "unix:///var/run/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.extend([
        process_output(3, "activating\n", ""),
        process_output(0, "27.5.1\n", ""),
    ]);
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let outcome = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("global-starting")?),
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(outcome.status, DockerOperationStatus::Reused);
    assert!(outcome.resources.is_empty());
    assert!(runner_view.requests()?.iter().all(|request| {
        request.program != "sudo" && !request.arguments.iter().any(|argument| argument == "start")
    }));
    Ok(())
}

#[tokio::test]
async fn global_engine_permission_error_does_not_start_a_local_service() -> TestResult {
    let mut responses = engine_context_responses(
        "default",
        "unix:///var/run/docker.sock",
        "permission denied while trying to connect to the Docker daemon socket",
    );
    responses.push(process_output(0, "active\n", ""));
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("global-permission")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("global permission failure must return an error".into());
    };
    assert!(
        error
            .render()
            .contains("current user cannot access the selected socket")
    );
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| { !request.arguments.iter().any(|argument| argument == "start") })
    );
    assert!(runner_view.requests()?.iter().all(|request| {
        !request
            .arguments
            .iter()
            .any(|argument| argument == "--user")
    }));
    Ok(())
}

#[tokio::test]
async fn stopped_global_engine_returns_manual_start_instructions() -> TestResult {
    let mut responses = engine_context_responses(
        "default",
        "unix:///var/run/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.push(process_output(3, "inactive\n", ""));
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("global-stopped")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("stopped global engine must return an error".into());
    };
    assert!(error.render().contains("sudo systemctl start docker"));
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| { !request.arguments.iter().any(|argument| argument == "start") })
    );
    assert!(runner_view.requests()?.iter().all(|request| {
        !request
            .arguments
            .iter()
            .any(|argument| argument == "--user")
    }));
    Ok(())
}

#[tokio::test]
async fn remote_docker_context_is_reported_without_local_service_changes() -> TestResult {
    let runner = FakeProcessRunner::with_responses(engine_context_responses(
        "remote-production",
        "ssh://docker.example",
        "Cannot connect to the Docker daemon",
    ));
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("remote-context")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("unavailable remote context must return an error".into());
    };
    assert!(error.render().contains("remote Engine"));
    let requests = runner_view.requests()?;
    assert!(
        requests
            .iter()
            .all(|request| request.program != "systemctl")
    );
    assert!(requests.iter().all(|request| {
        !request
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "context" && pair[1] == "use")
    }));
    Ok(())
}

#[tokio::test]
async fn invalid_docker_host_is_rejected_without_local_service_changes() -> TestResult {
    let runner = FakeProcessRunner::with_responses([process_output(
        1,
        "",
        "Cannot connect to the Docker daemon",
    )]);
    let (runner_view, engine) = process_engine(
        runner,
        vec![("DOCKER_HOST".to_owned(), "not-a-docker-host".to_owned())],
    )?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("invalid-host")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("invalid Docker host must return an error".into());
    };
    assert!(error.render().contains("host configuration is invalid"));
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| { request.program != "systemctl" })
    );
    Ok(())
}

#[tokio::test]
async fn docker_context_takes_precedence_over_docker_host() -> TestResult {
    let runner = FakeProcessRunner::with_responses(engine_context_responses(
        "remote-production",
        "ssh://docker.example",
        "Cannot connect to the Docker daemon",
    ));
    let (runner_view, engine) = process_engine(
        runner,
        vec![
            ("DOCKER_CONTEXT".to_owned(), "remote-production".to_owned()),
            (
                "DOCKER_HOST".to_owned(),
                "unix:///var/run/docker.sock".to_owned(),
            ),
        ],
    )?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("context-precedence")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("the selected Docker context must be reported when it is unavailable".into());
    };
    assert!(error.render().contains("remote Engine"));
    let requests = runner_view.requests()?;
    assert!(
        requests
            .iter()
            .any(|request| { request.arguments == vec!["context".to_owned(), "show".to_owned()] })
    );
    assert!(
        requests
            .iter()
            .all(|request| request.program != "systemctl")
    );
    Ok(())
}

#[tokio::test]
async fn remote_docker_host_is_reported_without_context_or_local_service_changes() -> TestResult {
    let runner = FakeProcessRunner::with_responses([process_output(
        1,
        "",
        "Cannot connect to the Docker daemon at tcp://remote.example:2376",
    )]);
    let (runner_view, engine) = process_engine(
        runner,
        vec![(
            "DOCKER_HOST".to_owned(),
            "tcp://remote.example:2376".to_owned(),
        )],
    )?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("remote-engine")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("unavailable remote engine must return an error".into());
    };
    assert!(
        error
            .render()
            .contains("will not start local Docker services")
    );
    let requests = runner_view.requests()?;
    assert!(
        requests
            .iter()
            .all(|request| request.program != "systemctl")
    );
    assert!(requests.iter().all(|request| {
        !request
            .arguments
            .iter()
            .any(|argument| argument == "show" || argument == "inspect")
    }));
    assert!(requests.iter().all(|request| {
        request
            .environment
            .iter()
            .any(|(key, value)| key == "DOCKER_HOST" && value == "tcp://remote.example:2376")
    }));
    Ok(())
}

#[tokio::test]
async fn missing_docker_cli_is_distinguished_from_an_unavailable_engine() -> TestResult {
    let missing = WorkstateError::with_source(
        ErrorCategory::Process,
        "could not execute 'docker'",
        io::Error::new(io::ErrorKind::NotFound, "docker was not found"),
    );
    let runner = FakeProcessRunner::with_results([Err(missing)]);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("missing-cli")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("missing Docker CLI must return an error".into());
    };
    assert!(error.render().contains("Docker CLI is not installed"));
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| request.program != "systemctl")
    );
    Ok(())
}

#[tokio::test]
async fn invalid_docker_context_is_not_replaced_automatically() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        process_output(1, "", "Cannot connect to the Docker daemon"),
        process_output(1, "", "context 'missing' does not exist"),
    ]);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("invalid-context")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("invalid Docker context must return an error".into());
    };
    assert!(
        error
            .render()
            .contains("selected Docker context is invalid")
    );
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| request.program != "systemctl")
    );
    Ok(())
}

#[tokio::test]
async fn unexpected_docker_info_failure_fails_closed() -> TestResult {
    let mut responses = engine_context_responses(
        "custom",
        "unix:///tmp/custom-docker.sock",
        "unexpected Docker response",
    );
    responses.push(process_output(0, "", ""));
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let result = engine
        .ensure_ready(
            DockerEngineRequest::for_action(context("unexpected-info")?),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    let Some(error) = result.err() else {
        return Err("unexpected Docker info failure must return an error".into());
    };
    assert!(error.render().contains("could not be determined safely"));
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| request.program != "systemctl")
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_engine_preflights_start_one_desktop_service() -> TestResult {
    let mut responses = engine_context_responses(
        "desktop-linux",
        "unix:///home/test/.docker/desktop/docker.sock",
        "Cannot connect to the Docker daemon",
    );
    responses.extend([
        process_output(0, "loaded\n", ""),
        process_output(3, "inactive\n", ""),
        process_output(0, "", ""),
        process_output(0, "27.5.1\n", ""),
    ]);
    let runner = FakeProcessRunner::with_responses(responses);
    let (runner_view, engine) = process_engine(runner, Vec::new())?;
    let first_request = DockerEngineRequest::for_action(context("concurrent-first")?);
    let second_request = DockerEngineRequest::for_action(context("concurrent-second")?);
    let (first, second) = tokio::join!(
        engine.ensure_ready(first_request, CancellationToken::new()),
        engine.ensure_ready(second_request, CancellationToken::new()),
    );

    assert!(first.is_ok());
    assert!(second.is_ok());
    let requests = runner_view.requests()?;
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.program == "systemctl"
                    && request.arguments
                        == vec![
                            "--user".to_owned(),
                            "start".to_owned(),
                            "docker-desktop".to_owned(),
                        ]
            })
            .count(),
        1
    );
    assert!(requests.iter().all(|request| request.program != "sudo"));
    assert!(requests.iter().all(|request| {
        !request
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "context" && pair[1] == "use")
    }));
    Ok(())
}

#[tokio::test]
async fn user_service_commands_are_disabled_for_non_linux_platforms() -> TestResult {
    let runner = FakeProcessRunner::default();
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let desktop = DockerDesktopController::new_for_platform(Arc::clone(&runner), false);
    let result = desktop
        .ensure_started(&context("non-linux")?, CancellationToken::new())
        .await;

    assert!(matches!(result, Ok(None)));
    assert!(runner_view.requests()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn non_linux_desktop_uses_its_existing_executable_backend() -> TestResult {
    let runner = FakeProcessRunner::with_responses([process_output(1, "", "")]);
    let runner_view = runner.clone();
    let runner: Arc<dyn workstate::application::ports::ProcessRunner> = Arc::new(runner);
    let desktop = DockerDesktopController::new_with_platform(
        Arc::clone(&runner),
        Some(PathBuf::from("/opt/docker-desktop")),
        false,
    );
    let resource = desktop
        .ensure_started(&context("non-linux-desktop")?, CancellationToken::new())
        .await?;

    let Some(resource) = resource else {
        return Err("the non-Linux Docker Desktop backend must start its executable".into());
    };
    assert_eq!(resource.ownership, OwnershipStatus::CreatedByCurrentRun);
    assert!(
        runner_view
            .requests()?
            .iter()
            .all(|request| { request.program != "systemctl" })
    );
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
