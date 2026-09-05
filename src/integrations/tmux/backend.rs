use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use crate::{
    application::ports::{
        BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner, TerminalBackend, TmuxBackend,
        TmuxSessionSnapshot, TmuxWindowRequest, TmuxWindowSnapshot,
    },
    application::timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{errors, models};

const TMUX_READY_TIMEOUT: Duration = DEFAULT_EXTERNAL_OPERATION_TIMEOUT;
const TMUX_READY_POLL: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub struct TmuxProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    executable: PathBuf,
}

impl TmuxProcessBackend {
    pub fn new(runner: Arc<dyn ProcessRunner>, executable: PathBuf) -> Result<Self> {
        if executable.as_os_str().is_empty() {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "tmux executable path must be non-empty",
            ));
        }
        Ok(Self { runner, executable })
    }

    pub fn executable(&self) -> &PathBuf {
        &self.executable
    }

    async fn execute(&self, operation: &str, arguments: Vec<String>) -> Result<ProcessOutput> {
        let output = self
            .runner
            .run(ProcessRequest {
                program: self.executable.to_string_lossy().into_owned(),
                arguments,
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|error| errors::operation_error(operation, error))?;
        if output.succeeded() {
            Ok(output)
        } else {
            Err(errors::command_failed(operation, &output))
        }
    }

    async fn execute_allow_missing(&self, operation: &str, arguments: Vec<String>) -> Result<()> {
        let output = self
            .runner
            .run(ProcessRequest {
                program: self.executable.to_string_lossy().into_owned(),
                arguments,
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|error| errors::operation_error(operation, error))?;
        if output.succeeded() || errors::missing_target(&output) {
            return Ok(());
        }
        Err(errors::command_failed(operation, &output))
    }

    async fn observe_inner(&self) -> Result<Vec<TmuxSessionSnapshot>> {
        let output = match self
            .runner
            .run(ProcessRequest {
                program: self.executable.to_string_lossy().into_owned(),
                arguments: vec![
                    "list-windows".to_owned(),
                    "-a".to_owned(),
                    "-F".to_owned(),
                    "#{session_id}\t#{session_name}\t#{window_id}\t#{window_name}\t#{pane_current_command}\t#{pane_start_command}\t#{pane_current_path}\t#{pane_pid}\t#{pane_dead}".to_owned(),
                ],
                working_directory: None,
                environment: Vec::new(),
            })
            .await
        {
            Ok(output) => output,
            Err(error) => return Err(errors::operation_error("list-windows", error)),
        };
        if !output.succeeded() {
            if errors::no_server(&output) {
                return Ok(Vec::new());
            }
            return Err(errors::command_failed("list-windows", &output));
        }
        parse_sessions(output.stdout)
    }

    async fn create_window_inner(
        &self,
        operation: &str,
        session_name: &str,
        window: TmuxWindowRequest,
        create_session: bool,
    ) -> Result<TmuxSessionSnapshot> {
        models::validate_name("session", session_name)?;
        models::validate_name("window", &window.name)?;
        models::validate_process(&window.process)?;
        let requested_window_name = window.name.clone();
        let command = render_process(&window.process)?;
        let mut arguments = if create_session {
            vec![
                "new-session".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                session_name.to_owned(),
            ]
        } else {
            vec![
                "new-window".to_owned(),
                "-d".to_owned(),
                "-t".to_owned(),
                session_name.to_owned(),
            ]
        };
        arguments.extend(["-n".to_owned(), window.name]);
        if let Some(directory) = &window.process.working_directory {
            arguments.extend(["-c".to_owned(), directory.to_string_lossy().into_owned()]);
        }
        arguments.extend(["--".to_owned(), command]);
        self.execute(operation, arguments).await?;
        self.wait_for_window(session_name, &requested_window_name)
            .await
    }

    async fn wait_for_window(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<TmuxSessionSnapshot> {
        let deadline = tokio::time::Instant::now() + TMUX_READY_TIMEOUT;
        loop {
            let sessions = self.observe_inner().await?;
            let matching = sessions
                .iter()
                .filter(|session| session.name == session_name)
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [session]
                    if session
                        .windows
                        .iter()
                        .any(|window| window.name == window_name && !window.is_dead) =>
                {
                    return Ok((*session).clone());
                }
                [] | [_] if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(TMUX_READY_POLL).await;
                }
                [] => return Err(errors::readiness_timeout(session_name, window_name)),
                [_] => return Err(errors::readiness_timeout(session_name, window_name)),
                _ => {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "tmux exposed multiple sessions with the same name",
                    )
                    .with_context("session_name", session_name.to_owned()));
                }
            }
        }
    }
}

impl TerminalBackend for TmuxProcessBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }
}

impl TmuxBackend for TmuxProcessBackend {
    fn observe<'a>(&'a self) -> BoxFuture<'a, Result<Vec<TmuxSessionSnapshot>>> {
        Box::pin(async move { self.observe_inner().await })
    }

    fn create_session<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>> {
        Box::pin(async move {
            self.create_window_inner("create-session", session_name, window, true)
                .await
        })
    }

    fn create_window<'a>(
        &'a self,
        session_name: &'a str,
        window: TmuxWindowRequest,
    ) -> BoxFuture<'a, Result<TmuxSessionSnapshot>> {
        Box::pin(async move {
            self.create_window_inner("create-window", session_name, window, false)
                .await
        })
    }

    fn kill_window<'a>(
        &'a self,
        session_name: &'a str,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            models::validate_name("session", session_name)?;
            models::validate_identity("window", window_identity)?;
            self.execute_allow_missing(
                "kill-window",
                vec![
                    "kill-window".to_owned(),
                    "-t".to_owned(),
                    format!("{session_name}:{window_identity}"),
                ],
            )
            .await
        })
    }

    fn kill_session<'a>(&'a self, session_name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            models::validate_name("session", session_name)?;
            self.execute_allow_missing(
                "kill-session",
                vec![
                    "kill-session".to_owned(),
                    "-t".to_owned(),
                    session_name.to_owned(),
                ],
            )
            .await
        })
    }
}

fn parse_sessions(bytes: Vec<u8>) -> Result<Vec<TmuxSessionSnapshot>> {
    let output = String::from_utf8(bytes).map_err(errors::invalid_utf8)?;
    let mut sessions = BTreeMap::<String, TmuxSessionSnapshot>::new();
    for line in output.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 9 {
            return Err(errors::malformed_data(format!(
                "expected 9 tab-separated fields, received {}",
                fields.len()
            )));
        }
        for (kind, value) in [
            ("session", fields[0]),
            ("session", fields[1]),
            ("window", fields[2]),
            ("window", fields[3]),
        ] {
            models::validate_identity(kind, value)?;
        }
        let process_id = if fields[7].is_empty() {
            None
        } else {
            Some(fields[7].parse::<u32>().map_err(|_| {
                errors::malformed_data(format!(
                    "pane PID '{}' is not an unsigned integer",
                    fields[7]
                ))
            })?)
        };
        let is_dead = match fields[8] {
            "0" => false,
            "1" => true,
            value => {
                return Err(errors::malformed_data(format!(
                    "pane dead flag '{value}' is not 0 or 1"
                )));
            }
        };
        let session_identity = fields[0].to_owned();
        let session_name = fields[1].to_owned();
        let window = TmuxWindowSnapshot {
            identity: fields[2].to_owned(),
            name: fields[3].to_owned(),
            command: (!fields[4].is_empty()).then(|| fields[4].to_owned()),
            start_command: (!fields[5].is_empty()).then(|| fields[5].to_owned()),
            working_directory: (!fields[6].is_empty()).then(|| PathBuf::from(fields[6])),
            process_id,
            is_dead,
        };
        let Some(session) = sessions.get_mut(&session_identity) else {
            sessions.insert(
                session_identity.clone(),
                TmuxSessionSnapshot {
                    identity: session_identity,
                    name: session_name,
                    windows: vec![window],
                },
            );
            continue;
        };
        if session.name != session_name {
            return Err(errors::malformed_data(
                "one session identity was reported with multiple names",
            ));
        }
        session.windows.push(window);
    }
    Ok(sessions.into_values().collect())
}

fn render_process(request: &ProcessRequest) -> Result<String> {
    models::validate_process(request)?;
    let mut parts = Vec::with_capacity(request.arguments.len() + request.environment.len() + 2);
    if !request.environment.is_empty() {
        parts.push("env".to_owned());
        parts.push("--".to_owned());
        parts.extend(
            request
                .environment
                .iter()
                .map(|(key, value)| format!("{}={}", shell_quote(key), shell_quote(value))),
        );
    }
    parts.push(shell_quote(&request.program));
    parts.extend(
        request
            .arguments
            .iter()
            .map(|argument| shell_quote(argument)),
    );
    Ok(parts.join(" "))
}

fn shell_quote(value: &str) -> String {
    crate::infrastructure::process::command_spec::shell_quote(value)
}
