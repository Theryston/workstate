#[path = "fakes/fake_process.rs"]
mod fake_process;
#[path = "fakes/fake_tmux.rs"]
mod fake_tmux;

use std::{
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use fake_process::FakeProcessRunner;
use fake_tmux::{FakeTmux, TmuxCall};
use workstate::{
    application::{
        planner::{
            ActionHandler, ActionHandlerRegistry, ActionOutput, ActionOutputSink, CancellationToken,
        },
        ports::{
            BoxFuture, FileSystem, ProcessOutput, ProcessRunner, ProcessStream, TmuxBackend,
            TmuxSessionSnapshot, TmuxWindowSnapshot,
        },
    },
    domain::{
        ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, ExecutionMode, OwnershipStatus,
        ResourceIdentity, ResourceKind, ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::{filesystem::local::LocalFileSystem, process::LocalProcessRunner},
    integrations::{
        CommandActionHandler, TmuxProcessBackend,
        tmux::{session_name, window_name},
    },
};

type TestResult = std::result::Result<(), Box<dyn Error>>;

#[derive(Clone, Default)]
struct RecordingOutput {
    messages: Arc<Mutex<Vec<ActionOutput>>>,
}

impl RecordingOutput {
    fn snapshot(&self) -> Result<Vec<ActionOutput>> {
        self.messages
            .lock()
            .map(|messages| messages.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output lock failed"))
    }
}

impl ActionOutputSink for RecordingOutput {
    fn emit<'a>(&'a self, output: ActionOutput) -> BoxFuture<'a, Result<()>> {
        let result = self
            .messages
            .lock()
            .map(|mut messages| messages.push(output))
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output lock failed"));
        Box::pin(async move { result })
    }
}

fn action(configuration: &EnvironmentConfig, id: &str, mode: ExecutionMode) -> Result<ActionSpec> {
    let mut action = ActionSpec::new(id, ActionKind::RunCommand).map_err(WorkstateError::from)?;
    action.parameters.command = Some(CommandSpec::new("bun"));
    action.working_directory = Some(std::env::temp_dir().display().to_string());
    action.execution_mode = Some(mode);
    action.resolved_environment = Some(configuration.slug.clone());
    Ok(action)
}

fn command_handler(
    runner: Arc<dyn ProcessRunner>,
    tmux: Arc<dyn TmuxBackend>,
) -> Result<CommandActionHandler> {
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    CommandActionHandler::new("run_command", runner, tmux, file_system)
}

#[tokio::test]
async fn one_shot_commands_use_argv_execution_and_stream_output_attribution() -> TestResult {
    let runner = FakeProcessRunner::with_responses([ProcessOutput {
        status: Some(0),
        stdout: b"ready\n".to_vec(),
        stderr: b"diagnostic\n".to_vec(),
    }]);
    let runner_view = runner.clone();
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let tmux_view = FakeTmux::default();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux_view.clone());
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::RunOnce)?;
    let output = RecordingOutput::default();

    let result = handler
        .run_once_with_output(&action, CancellationToken::new(), Arc::new(output.clone()))
        .await?;

    assert!(result.resources.is_empty());
    assert!(tmux_view.calls()?.is_empty());
    let requests = runner_view.requests()?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].program, "bun");
    assert!(requests[0].working_directory.is_some());
    let messages = output.snapshot()?;
    assert!(messages.iter().any(|message| {
        message.stream == workstate::application::planner::ActionOutputStream::Stdout
            && message.message == "ready\n"
    }));
    assert!(messages.iter().any(|message| {
        message.stream == workstate::application::planner::ActionOutputStream::Stderr
            && message.message == "diagnostic\n"
    }));
    Ok(())
}

#[tokio::test]
async fn one_shot_non_zero_exit_is_returned_as_a_typed_failure() -> TestResult {
    let runner = FakeProcessRunner::with_responses([ProcessOutput {
        status: Some(7),
        stdout: Vec::new(),
        stderr: b"command failed\n".to_vec(),
    }]);
    let runner_view = runner.clone();
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let tmux_view = FakeTmux::default();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux_view.clone());
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::RunOnce)?;

    let result = handler.run_once(&action, CancellationToken::new()).await;

    assert!(result.is_err());
    assert_eq!(runner_view.requests()?.len(), 1);
    assert!(tmux_view.calls()?.is_empty());
    let error = result.err().ok_or("missing command failure")?;
    assert_eq!(
        error.context.get("action_id").map(String::as_str),
        Some("api")
    );
    assert_eq!(
        error.context.get("exit_status").map(String::as_str),
        Some("7")
    );
    Ok(())
}

#[tokio::test]
async fn invalid_working_directory_fails_before_spawning() -> TestResult {
    let runner = FakeProcessRunner::default();
    let runner_view = runner.clone();
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let tmux: Arc<dyn TmuxBackend> = Arc::new(FakeTmux::default());
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let mut action = action(&configuration, "api", ExecutionMode::RunOnce)?;
    action.working_directory = Some(
        std::env::temp_dir()
            .join(format!("workstate-task-08-missing-{}", std::process::id()))
            .display()
            .to_string(),
    );
    if std::fs::metadata(action.working_directory.as_deref().unwrap_or_default()).is_ok() {
        return Ok(());
    }

    let result = handler.run_once(&action, CancellationToken::new()).await;

    assert!(result.is_err());
    assert!(runner_view.requests()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn two_persistent_actions_share_one_session_and_use_distinct_windows() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let mut handlers = ActionHandlerRegistry::new();
    workstate::integrations::command::register_handlers(
        &mut handlers,
        runner,
        Arc::clone(&tmux),
        Arc::new(LocalFileSystem),
    )?;
    let Some(handler) = handlers.handler_for(&ActionKind::RunCommand) else {
        return Err(std::io::Error::other("run command handler was not registered").into());
    };
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let api = action(&configuration, "api", ExecutionMode::Background)?;
    let web = action(&configuration, "web", ExecutionMode::Background)?;

    let first = handler
        .start_background(&api, CancellationToken::new())
        .await?;
    let second = handler
        .start_background(&web, CancellationToken::new())
        .await?;

    assert_eq!(first.resources.len(), 2);
    assert_eq!(second.resources.len(), 2);
    let sessions = tmux_view.sessions()?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name, session_name(&configuration.slug));
    assert_eq!(sessions[0].windows.len(), 2);
    assert!(
        sessions[0]
            .windows
            .iter()
            .any(|window| window.name == window_name(&api.id))
    );
    assert!(
        sessions[0]
            .windows
            .iter()
            .any(|window| window.name == window_name(&web.id))
    );
    let calls = tmux_view.calls()?;
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, TmuxCall::CreateSession { .. }))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, TmuxCall::CreateWindow { .. }))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn healthy_persistent_window_is_reused_without_claiming_a_duplicate() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    let started = handler
        .start_background(&action, CancellationToken::new())
        .await?;
    let create_count = tmux_view
        .calls()?
        .iter()
        .filter(|call| {
            matches!(
                call,
                TmuxCall::CreateSession { .. } | TmuxCall::CreateWindow { .. }
            )
        })
        .count();

    let observation = handler
        .observe_with_resources(&action, &started.resources, CancellationToken::new())
        .await?;

    assert_eq!(
        observation.status,
        workstate::application::planner::ObservationStatus::AlreadyCorrect
    );
    assert_eq!(
        tmux_view
            .calls()?
            .iter()
            .filter(|call| matches!(
                call,
                TmuxCall::CreateSession { .. } | TmuxCall::CreateWindow { .. }
            ))
            .count(),
        create_count
    );
    Ok(())
}

#[tokio::test]
async fn package_manager_window_is_healthy_when_child_process_is_foreground() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let mut action = action(&configuration, "api", ExecutionMode::Background)?;
    let Some(command) = action.parameters.command.as_mut() else {
        return Err(std::io::Error::other("missing command specification").into());
    };
    command.program = "yarn".to_owned();
    command.arguments = vec!["start:dev".to_owned()];
    tmux.insert_session(TmuxSessionSnapshot {
        identity: "session-0".to_owned(),
        name: session_name(&configuration.slug),
        windows: vec![TmuxWindowSnapshot {
            identity: "@0".to_owned(),
            name: window_name(&action.id),
            command: Some("node".to_owned()),
            start_command: Some("yarn start:dev".to_owned()),
            working_directory: Some(std::env::temp_dir()),
            process_id: Some(42),
            is_dead: false,
        }],
    })?;
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;

    let result = handler
        .start_background(&action, CancellationToken::new())
        .await?;

    assert!(!result.changed);
    assert!(tmux_view.calls()?.iter().all(|call| {
        !matches!(
            call,
            TmuxCall::CreateSession { .. } | TmuxCall::CreateWindow { .. }
        )
    }));
    Ok(())
}

#[tokio::test]
async fn a_missing_owned_window_is_recreated_in_the_existing_session() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux.clone())?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    let started = handler
        .start_background(&action, CancellationToken::new())
        .await?;
    let window_identity = started
        .resources
        .iter()
        .find(|record| record.resource.kind == ResourceKind::TmuxWindow)
        .map(|record| record.resource.stable_identity.clone())
        .ok_or("missing created tmux window")?;
    tmux.kill_window(&session_name(&configuration.slug), &window_identity)
        .await?;

    let recreated = handler
        .start_background(&action, CancellationToken::new())
        .await?;

    assert!(recreated.changed);
    assert_eq!(tmux_view.sessions()?.len(), 1);
    assert_eq!(
        tmux_view.sessions()?.first().map(|s| s.windows.len()),
        Some(1)
    );
    assert_eq!(
        tmux_view
            .calls()?
            .iter()
            .filter(|call| matches!(call, TmuxCall::CreateSession { .. }))
            .count(),
        1
    );
    assert_eq!(
        tmux_view
            .calls()?
            .iter()
            .filter(|call| matches!(call, TmuxCall::CreateWindow { .. }))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_prefix_collision_does_not_match_the_environment_session() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    tmux.insert_session(TmuxSessionSnapshot {
        identity: "external-session".to_owned(),
        name: "workstate-personal-blog-extra".to_owned(),
        windows: Vec::new(),
    })?;
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;

    let result = handler
        .start_background(&action, CancellationToken::new())
        .await?;

    assert!(result.changed);
    let sessions = tmux_view.sessions()?;
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|session| {
        session.name == session_name(&configuration.slug) && session.windows.len() == 1
    }));
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_window_identity_fails_without_takeover() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    tmux.insert_session(TmuxSessionSnapshot {
        identity: "session-0".to_owned(),
        name: session_name(&configuration.slug),
        windows: vec![
            TmuxWindowSnapshot {
                identity: "@0".to_owned(),
                name: window_name(&action.id),
                command: Some("bun".to_owned()),
                start_command: Some("bun".to_owned()),
                working_directory: Some(std::env::temp_dir()),
                process_id: Some(1),
                is_dead: false,
            },
            TmuxWindowSnapshot {
                identity: "@1".to_owned(),
                name: window_name(&action.id),
                command: Some("bun".to_owned()),
                start_command: Some("bun".to_owned()),
                working_directory: Some(std::env::temp_dir()),
                process_id: Some(2),
                is_dead: false,
            },
        ],
    })?;
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;

    let observation = handler
        .observe_with_resources(&action, &[], CancellationToken::new())
        .await?;
    let result = handler
        .start_background(&action, CancellationToken::new())
        .await;

    assert_eq!(
        observation.status,
        workstate::application::planner::ObservationStatus::Unknown
    );
    assert!(result.is_err());
    assert_eq!(
        tmux_view.sessions()?.first().map(|s| s.windows.len()),
        Some(2)
    );
    Ok(())
}

#[tokio::test]
async fn stop_removes_owned_window_and_session_idempotently() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    let started = handler
        .start_background(&action, CancellationToken::new())
        .await?;

    handler
        .stop(&action, &started.resources, CancellationToken::new())
        .await?;
    handler
        .stop(&action, &started.resources, CancellationToken::new())
        .await?;

    assert!(tmux_view.sessions()?.is_empty());
    let calls = tmux_view.calls()?;
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, TmuxCall::KillWindow { .. }))
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, TmuxCall::KillSession { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn an_existing_session_with_unmanaged_windows_is_preserved() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    tmux_view.insert_session(TmuxSessionSnapshot {
        identity: "external-session".to_owned(),
        name: "workstate-personal-blog".to_owned(),
        windows: vec![TmuxWindowSnapshot {
            identity: "@external".to_owned(),
            name: "external".to_owned(),
            command: Some("bash".to_owned()),
            start_command: Some("bash".to_owned()),
            working_directory: Some(std::env::temp_dir()),
            process_id: Some(42),
            is_dead: false,
        }],
    })?;
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    let started = handler
        .start_background(&action, CancellationToken::new())
        .await?;

    handler
        .stop(&action, &started.resources, CancellationToken::new())
        .await?;

    let sessions = tmux_view.sessions()?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].windows.len(), 1);
    assert_eq!(sessions[0].windows[0].name, "external");
    assert!(
        !tmux_view
            .calls()?
            .iter()
            .any(|call| matches!(call, TmuxCall::KillSession { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn direct_cleanup_ignores_reused_resources() -> TestResult {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::default());
    let tmux = FakeTmux::default();
    let tmux_view = tmux.clone();
    tmux_view.insert_session(TmuxSessionSnapshot {
        identity: "external-session".to_owned(),
        name: "workstate-personal-blog".to_owned(),
        windows: vec![TmuxWindowSnapshot {
            identity: "@external".to_owned(),
            name: "external".to_owned(),
            command: Some("bash".to_owned()),
            start_command: Some("bash".to_owned()),
            working_directory: Some(std::env::temp_dir()),
            process_id: Some(42),
            is_dead: false,
        }],
    })?;
    let tmux: Arc<dyn TmuxBackend> = Arc::new(tmux);
    let handler = command_handler(runner, tmux)?;
    let configuration = EnvironmentConfig::new("Personal Blog")?;
    let action = action(&configuration, "api", ExecutionMode::Background)?;
    let identity = ResourceIdentity::new(ResourceKind::TmuxWindow, "@external")
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, OwnershipStatus::ReusedExisting)
        .with_action(action.id.clone());
    record
        .integration_metadata
        .insert("session_name".to_owned(), session_name(&configuration.slug));
    record
        .integration_metadata
        .insert("window_name".to_owned(), "external".to_owned());

    handler
        .stop(&action, &[record], CancellationToken::new())
        .await?;

    assert!(tmux_view.calls()?.iter().all(|call| {
        !matches!(
            call,
            TmuxCall::KillWindow { .. } | TmuxCall::KillSession { .. }
        )
    }));
    assert_eq!(tmux_view.sessions()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn tmux_adapter_parses_sessions_and_quotes_structured_commands() -> TestResult {
    let fixture = include_str!("fixtures/tmux/sessions.tsv");
    let runner = FakeProcessRunner::with_responses([
        ProcessOutput {
            status: Some(0),
            stdout: fixture.as_bytes().to_vec(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        ProcessOutput {
            status: Some(0),
            stdout: fixture.as_bytes().to_vec(),
            stderr: Vec::new(),
        },
    ]);
    let runner_view = runner.clone();
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let backend = TmuxProcessBackend::new(runner, PathBuf::from("/usr/bin/tmux"))?;

    let sessions = backend.observe().await?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].windows.len(), 2);
    assert_eq!(sessions[0].windows[0].process_id, Some(1234));
    assert_eq!(
        sessions[0].windows[1].start_command.as_deref(),
        Some("yarn start:dev")
    );
    assert!(!sessions[0].windows[0].is_dead);

    let created = backend
        .create_session(
            "workstate-personal-blog",
            workstate::application::ports::TmuxWindowRequest {
                name: "workstate-api".to_owned(),
                process: workstate::application::ports::ProcessRequest {
                    program: "bun".to_owned(),
                    arguments: vec!["dev server".to_owned()],
                    working_directory: Some(PathBuf::from("/home/example/project")),
                    environment: vec![("GREETING".to_owned(), "hello world".to_owned())],
                },
            },
        )
        .await?;
    assert_eq!(created.name, "workstate-personal-blog");
    let requests = runner_view.requests()?;
    let create_request = requests
        .get(1)
        .ok_or_else(|| std::io::Error::other("tmux create request was not recorded"))?;
    let command = create_request.arguments.last().cloned().unwrap_or_default();
    assert!(command.contains("'GREETING'='hello world'"));
    assert!(command.contains("'dev server'"));
    Ok(())
}

#[tokio::test]
async fn missing_tmux_server_is_observed_as_an_empty_state() -> TestResult {
    let runner = FakeProcessRunner::with_responses([ProcessOutput {
        status: Some(1),
        stdout: Vec::new(),
        stderr: b"no server running on /tmp/tmux-1000/default\n".to_vec(),
    }]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let backend = TmuxProcessBackend::new(runner, PathBuf::from("/usr/bin/tmux"))?;

    assert!(backend.observe().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_tmux_socket_is_observed_as_an_empty_state() -> TestResult {
    let runner = FakeProcessRunner::with_responses([ProcessOutput {
        status: Some(1),
        stdout: Vec::new(),
        stderr: b"error connecting to /tmp/tmux-1000/default (No such file or directory)\n"
            .to_vec(),
    }]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(runner);
    let backend = TmuxProcessBackend::new(runner, PathBuf::from("/usr/bin/tmux"))?;

    assert!(backend.observe().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn local_runner_streams_stdout_and_stderr_without_a_shell_by_default() -> TestResult {
    let output = RecordingOutput::default();
    let runner = LocalProcessRunner;
    let result = runner
        .run_with_output(
            workstate::application::ports::ProcessRequest {
                program: "/bin/sh".to_owned(),
                arguments: vec![
                    "-c".to_owned(),
                    "printf stdout; printf stderr >&2".to_owned(),
                ],
                working_directory: None,
                environment: Vec::new(),
            },
            Arc::new(OutputProcessSink {
                output: output.clone(),
            }),
        )
        .await?;

    assert!(result.succeeded());
    let messages = output.snapshot()?;
    assert!(messages.iter().any(|message| {
        message.stream == workstate::application::planner::ActionOutputStream::Stdout
            && message.message == "stdout"
    }));
    assert!(messages.iter().any(|message| {
        message.stream == workstate::application::planner::ActionOutputStream::Stderr
            && message.message == "stderr"
    }));
    Ok(())
}

struct OutputProcessSink {
    output: RecordingOutput,
}

impl workstate::application::ports::ProcessOutputSink for OutputProcessSink {
    fn emit<'a>(
        &'a self,
        chunk: workstate::application::ports::ProcessOutputChunk,
    ) -> BoxFuture<'a, Result<()>> {
        let stream = match chunk.stream {
            ProcessStream::Stdout => workstate::application::planner::ActionOutputStream::Stdout,
            ProcessStream::Stderr => workstate::application::planner::ActionOutputStream::Stderr,
        };
        let result = self
            .output
            .messages
            .lock()
            .map(|mut messages| {
                messages.push(ActionOutput {
                    stream,
                    message: String::from_utf8_lossy(&chunk.bytes).into_owned(),
                });
            })
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output lock failed"));
        Box::pin(async move { result })
    }
}
