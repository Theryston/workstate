use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, ActionOutputSink, CancellationToken, CompensationResult,
        },
        ports::{
            BoxFuture, FileSystem, ProcessOutputChunk, ProcessOutputSink, ProcessRequest,
            ProcessRunner, TmuxBackend, TmuxSessionSnapshot, TmuxWindowRequest, TmuxWindowSnapshot,
        },
    },
    domain::{
        ActionKind, ActionSpec, EnvironmentSlug, ExecutionMode, OwnershipStatus, ResourceIdentity,
        ResourceKind, ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::{filesystem::PathResolver, process::command_spec::to_process_request},
    platform::CapabilityId,
};

use crate::integrations::tmux::{session_name, window_name};

#[derive(Clone)]
pub struct CommandActionHandler {
    key: &'static str,
    process_runner: Arc<dyn ProcessRunner>,
    tmux: Arc<dyn TmuxBackend>,
    file_system: Arc<dyn FileSystem>,
    session_lock: Arc<tokio::sync::Mutex<()>>,
}

struct TmuxCommandTarget<'a> {
    action: &'a ActionSpec,
    session_name: &'a str,
    window_name: &'a str,
    request: &'a ProcessRequest,
}

impl CommandActionHandler {
    pub fn new(
        key: &'static str,
        process_runner: Arc<dyn ProcessRunner>,
        tmux: Arc<dyn TmuxBackend>,
        file_system: Arc<dyn FileSystem>,
    ) -> Result<Self> {
        if !matches!(key, "run_command" | "start_service") {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("unsupported command action handler key '{key}'"),
            ));
        }
        Ok(Self {
            key,
            process_runner,
            tmux,
            file_system,
            session_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn with_session_lock(mut self, session_lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        self.session_lock = session_lock;
        self
    }

    fn action_matches(&self, action: &ActionSpec) -> bool {
        matches!(
            (&action.kind, self.key),
            (ActionKind::RunCommand, "run_command") | (ActionKind::StartService, "start_service")
        )
    }

    fn request_for(&self, action: &ActionSpec) -> Result<ProcessRequest> {
        let command = action.parameters.command.as_ref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!("command action '{}' is missing its command", action.id),
            )
        })?;
        let working_directory =
            self.resolve_working_directory(action.working_directory.as_deref())?;
        to_process_request(command, Some(working_directory))
    }

    fn resolve_working_directory(&self, configured: Option<&str>) -> Result<PathBuf> {
        let home = self.file_system.home_directory().map_err(|error| {
            error.with_context("operation", "resolve command working directory")
        })?;
        let resolver = PathResolver::new(home.clone(), self.file_system.as_ref())?;
        match configured {
            Some(value) => resolver.canonicalize_for_execution(value).map_err(|error| {
                error.with_context("operation", "resolve command working directory")
            }),
            None => {
                let exists = self.file_system.exists(&home)?;
                if !exists || !self.file_system.is_directory(&home)? {
                    return Err(WorkstateError::new(
                        ErrorCategory::Process,
                        "the default command working directory is not a directory",
                    )
                    .with_context("working_directory", home.display().to_string()));
                }
                self.file_system.canonicalize(&home).map_err(|error| {
                    error.with_context("operation", "resolve command working directory")
                })
            }
        }
    }

    fn environment_for<'a>(&self, action: &'a ActionSpec) -> Result<&'a EnvironmentSlug> {
        action.resolved_environment.as_ref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Runtime,
                format!(
                    "command action '{}' was executed without an environment context",
                    action.id
                ),
            )
        })
    }

    async fn observe_background(
        &self,
        action: &ActionSpec,
        previous_resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let request = self.request_for(action)?;
        let environment = self.environment_for(action)?;
        let expected_session = session_name(environment);
        let expected_window = window_name(&action.id);
        let sessions = self.tmux.observe().await?;
        let matching_sessions = sessions
            .iter()
            .filter(|session| session.name == expected_session)
            .collect::<Vec<_>>();
        let session = match matching_sessions.as_slice() {
            [] => {
                return Ok(ActionObservation::requires_change()
                    .with_detail("the environment tmux session is missing"));
            }
            [session] => *session,
            _ => {
                return Ok(ActionObservation::unknown(
                    "multiple tmux sessions have the canonical environment name",
                ));
            }
        };
        let matching_windows = session
            .windows
            .iter()
            .filter(|window| window.name == expected_window)
            .collect::<Vec<_>>();
        let window = match matching_windows.as_slice() {
            [] => {
                return Ok(ActionObservation::requires_change()
                    .with_detail("the persistent tmux window is missing"));
            }
            [window] => *window,
            _ => {
                return Ok(ActionObservation::unknown(
                    "multiple tmux windows have the canonical action name",
                ));
            }
        };
        let known_window = previous_resources.iter().any(|record| {
            record.resource.kind == ResourceKind::TmuxWindow
                && record.resource.stable_identity == window.identity
                && record
                    .integration_metadata
                    .get("session_name")
                    .is_some_and(|name| name == &expected_session)
                && record
                    .integration_metadata
                    .get("window_name")
                    .is_some_and(|name| name == &expected_window)
        });
        if !known_window {
            return Ok(ActionObservation::unknown(
                "the canonical tmux window exists but is not owned by Workstate",
            ));
        }
        if !window_is_healthy(window, &request) {
            return Ok(ActionObservation::unknown(
                "the owned tmux window no longer matches the configured command",
            ));
        }
        let target = TmuxCommandTarget {
            action,
            session_name: &expected_session,
            window_name: &expected_window,
            request: &request,
        };
        Ok(ActionObservation::already_correct().with_resources(vec![
            session_record(
                target.action,
                session,
                OwnershipStatus::ReusedExisting,
                true,
                target.session_name,
            )?,
            window_record(
                &target,
                session,
                window,
                OwnershipStatus::ReusedExisting,
                true,
                OwnershipStatus::ReusedExisting,
            )?,
        ]))
    }

    async fn start_background_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let request = self.request_for(action)?;
        let environment = self.environment_for(action)?;
        let expected_session = session_name(environment);
        let expected_window = window_name(&action.id);
        let target = TmuxCommandTarget {
            action,
            session_name: &expected_session,
            window_name: &expected_window,
            request: &request,
        };
        let _session_guard = self.session_lock.lock().await;
        cancellation.check()?;
        let sessions = self.tmux.observe().await?;
        let matching_sessions = sessions
            .iter()
            .filter(|session| session.name == expected_session)
            .collect::<Vec<_>>();
        let session = match matching_sessions.as_slice() {
            [] => {
                let created = self
                    .tmux
                    .create_session(
                        &expected_session,
                        TmuxWindowRequest {
                            name: expected_window.clone(),
                            process: request.clone(),
                        },
                    )
                    .await?;
                return self.result_from_session(
                    &target,
                    &created,
                    OwnershipStatus::CreatedByCurrentRun,
                    OwnershipStatus::CreatedByCurrentRun,
                    true,
                );
            }
            [session] => *session,
            _ => {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "multiple tmux sessions have the canonical environment name",
                )
                .with_context("session_name", expected_session));
            }
        };
        let matching_windows = session
            .windows
            .iter()
            .filter(|window| window.name == expected_window)
            .collect::<Vec<_>>();
        match matching_windows.as_slice() {
            [] => {
                let updated = self
                    .tmux
                    .create_window(
                        &expected_session,
                        TmuxWindowRequest {
                            name: expected_window.clone(),
                            process: request.clone(),
                        },
                    )
                    .await?;
                self.result_from_session(
                    &target,
                    &updated,
                    OwnershipStatus::ReusedExisting,
                    OwnershipStatus::CreatedByCurrentRun,
                    true,
                )
            }
            [window] if window_is_healthy(window, &request) => {
                self.result_from_existing(&target, session)
            }
            [window] => Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the canonical tmux window exists but its command or working directory changed",
            )
            .with_context("session_name", expected_session)
            .with_context("window_identity", window.identity.clone())),
            _ => Err(WorkstateError::new(
                ErrorCategory::Integration,
                "multiple tmux windows have the canonical action name",
            )
            .with_context("session_name", expected_session)
            .with_context("window_name", expected_window)),
        }
    }

    fn result_from_session(
        &self,
        target: &TmuxCommandTarget<'_>,
        session: &TmuxSessionSnapshot,
        session_ownership: OwnershipStatus,
        window_ownership: OwnershipStatus,
        changed: bool,
    ) -> Result<ActionExecutionResult> {
        let matching_windows = session
            .windows
            .iter()
            .filter(|window| window.name == target.window_name)
            .collect::<Vec<_>>();
        let Some(window) = matching_windows.first().copied() else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "tmux did not expose the persistent window after creation",
            )
            .with_context("session_name", target.session_name)
            .with_context("window_name", target.window_name));
        };
        if matching_windows.len() != 1 {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "tmux exposed multiple windows with the canonical action name",
            )
            .with_context("session_name", target.session_name)
            .with_context("window_name", target.window_name));
        }
        if !window_is_healthy(window, target.request) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the persistent tmux window did not remain healthy after creation",
            )
            .with_context("session_name", target.session_name)
            .with_context("window_name", target.window_name));
        }
        Ok(ActionExecutionResult {
            changed,
            resources: vec![
                session_record(
                    target.action,
                    session,
                    session_ownership,
                    session_ownership != OwnershipStatus::CreatedByCurrentRun,
                    target.session_name,
                )?,
                window_record(
                    target,
                    session,
                    window,
                    window_ownership,
                    window_ownership != OwnershipStatus::CreatedByCurrentRun,
                    session_ownership,
                )?,
            ],
            mutations: Vec::new(),
            outputs: vec![ActionOutput::log(if changed {
                format!(
                    "started persistent command in tmux window '{}'",
                    target.window_name
                )
            } else {
                format!("reused persistent tmux window '{}'", target.window_name)
            })],
        })
    }

    fn result_from_existing(
        &self,
        target: &TmuxCommandTarget<'_>,
        session: &TmuxSessionSnapshot,
    ) -> Result<ActionExecutionResult> {
        self.result_from_session(
            target,
            session,
            OwnershipStatus::ReusedExisting,
            OwnershipStatus::ReusedExisting,
            false,
        )
    }

    async fn run_once_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let request = self.request_for(action)?;
        let output = self.process_runner.run(request).await?;
        cancellation.check()?;
        if !output.succeeded() {
            return Err(command_exit_error(action, &output));
        }
        let mut outputs = vec![ActionOutput::log(format!(
            "completed one-shot command for action '{}'",
            action.id
        ))];
        if !output.stdout.is_empty() {
            outputs.push(ActionOutput::stdout(
                String::from_utf8_lossy(&output.stdout).into_owned(),
            ));
        }
        if !output.stderr.is_empty() {
            outputs.push(ActionOutput::stderr(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(ActionExecutionResult {
            changed: true,
            resources: Vec::new(),
            mutations: Vec::new(),
            outputs,
        })
    }

    async fn run_once_with_output_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let request = self.request_for(action)?;
        output
            .emit(ActionOutput::log(format!(
                "running one-shot command for action '{}'",
                action.id
            )))
            .await?;
        let process_output = self
            .process_runner
            .run_with_output(request, Arc::new(ProcessOutputForwarder { output }))
            .await?;
        cancellation.check()?;
        if !process_output.succeeded() {
            return Err(command_exit_error(action, &process_output));
        }
        Ok(ActionExecutionResult {
            changed: true,
            resources: Vec::new(),
            mutations: Vec::new(),
            outputs: Vec::new(),
        })
    }

    async fn cleanup_inner(
        &self,
        action: &ActionSpec,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let windows = resources
            .iter()
            .filter(|record| {
                record.resource.kind == ResourceKind::TmuxWindow && record.is_cleanup_candidate()
            })
            .collect::<Vec<_>>();
        let sessions = resources
            .iter()
            .filter(|record| {
                record.resource.kind == ResourceKind::TmuxSession && record.is_cleanup_candidate()
            })
            .collect::<Vec<_>>();
        let mut outputs = Vec::new();
        for resource in windows {
            cancellation.check()?;
            let session_name = session_name_from_record(action, resource)?;
            let sessions_snapshot = self.tmux.observe().await?;
            let exists = sessions_snapshot.iter().any(|session| {
                session.name == session_name
                    && resource
                        .integration_metadata
                        .get("session_identity")
                        .is_none_or(|identity| identity == &session.identity)
                    && session.windows.iter().any(|window| {
                        window.identity == resource.resource.stable_identity
                            && window.name
                                == resource
                                    .integration_metadata
                                    .get("window_name")
                                    .map(String::as_str)
                                    .unwrap_or("")
                    })
            });
            if !exists {
                outputs.push(ActionOutput::log(format!(
                    "tmux window '{}' was already absent",
                    resource.resource.stable_identity
                )));
                continue;
            }
            self.tmux
                .kill_window(&session_name, &resource.resource.stable_identity)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "stopped owned tmux window '{}'",
                resource.resource.stable_identity
            )));
        }
        for resource in sessions {
            cancellation.check()?;
            let session_name = session_name_from_record(action, resource)?;
            let sessions_snapshot = self.tmux.observe().await?;
            let Some(session) = sessions_snapshot.iter().find(|session| {
                session.name == session_name
                    && session.identity == resource.resource.stable_identity
            }) else {
                outputs.push(ActionOutput::log(format!(
                    "tmux session '{session_name}' was already absent"
                )));
                continue;
            };
            if !session.windows.is_empty() {
                outputs.push(ActionOutput::log(format!(
                    "preserved tmux session '{session_name}' because unmanaged windows remain"
                )));
                continue;
            }
            self.tmux.kill_session(&session_name).await?;
            outputs.push(ActionOutput::log(format!(
                "stopped owned tmux session '{session_name}'"
            )));
        }
        Ok(CompensationResult { outputs })
    }

    async fn observe_cleanup_inner(
        &self,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let sessions = self.tmux.observe().await?;
        let mut observed = Vec::new();
        for resource in resources {
            let Some(session_name) = resource.integration_metadata.get("session_name") else {
                continue;
            };
            let Some(session) = sessions.iter().find(|session| {
                session.name == *session_name && session.identity == resource_identity(resource)
            }) else {
                continue;
            };
            if resource.resource.kind == ResourceKind::TmuxSession {
                observed.push(resource.clone());
                continue;
            }
            if resource.resource.kind == ResourceKind::TmuxWindow
                && session.windows.iter().any(|window| {
                    window.identity == resource.resource.stable_identity
                        && resource
                            .integration_metadata
                            .get("window_name")
                            .is_some_and(|name| name == &window.name)
                })
            {
                observed.push(resource.clone());
            }
        }
        Ok(ActionObservation::already_correct().with_resources(observed))
    }
}

impl ActionHandler for CommandActionHandler {
    fn action_key(&self) -> &str {
        self.key
    }

    fn required_capabilities(&self) -> BTreeSet<CapabilityId> {
        [CapabilityId::BackgroundProcesses].into_iter().collect()
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if !self.action_matches(action) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("handler '{}' received an incompatible action", self.key),
            ));
        }
        if action.parameters.command.is_none() {
            return Err(WorkstateError::new(
                ErrorCategory::Domain,
                format!("command action '{}' is missing its command", action.id),
            ));
        }
        self.request_for(action).map(|_| ())
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            match action.execution_mode {
                Some(ExecutionMode::Background) => {
                    self.observe_background(action, &[], cancellation).await
                }
                _ => {
                    cancellation.check()?;
                    Ok(ActionObservation::requires_change()
                        .with_detail("one-shot commands run during setup"))
                }
            }
        })
    }

    fn observe_with_resources<'a>(
        &'a self,
        action: &'a ActionSpec,
        previous_resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            match action.execution_mode {
                Some(ExecutionMode::Background) => {
                    self.observe_background(action, previous_resources, cancellation)
                        .await
                }
                _ => {
                    cancellation.check()?;
                    Ok(ActionObservation::requires_change()
                        .with_detail("one-shot commands run during setup"))
                }
            }
        })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move {
            match action.execution_mode {
                Some(ExecutionMode::Background) => {
                    self.start_background_inner(action, cancellation).await
                }
                _ => self.run_once_inner(action, cancellation).await,
            }
        })
    }

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move { self.run_once_inner(action, cancellation).await })
    }

    fn run_once_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move {
            self.run_once_with_output_inner(action, cancellation, output)
                .await
        })
    }

    fn start_background<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move { self.start_background_inner(action, cancellation).await })
    }

    fn start_background_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move {
            let result = self.start_background_inner(action, cancellation).await?;
            for item in &result.outputs {
                output.emit(item.clone()).await?;
            }
            Ok(ActionExecutionResult {
                outputs: Vec::new(),
                ..result
            })
        })
    }

    fn observe_for_cleanup<'a>(
        &'a self,
        _action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_cleanup_inner(resources, cancellation).await })
    }

    fn compensate<'a>(
        &'a self,
        action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move {
            self.cleanup_inner(action, &result.resources, cancellation)
                .await
        })
    }

    fn stop<'a>(
        &'a self,
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.cleanup_inner(action, resources, cancellation).await })
    }
}

struct ProcessOutputForwarder {
    output: Arc<dyn ActionOutputSink>,
}

impl ProcessOutputSink for ProcessOutputForwarder {
    fn emit<'a>(&'a self, chunk: ProcessOutputChunk) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let message = String::from_utf8_lossy(&chunk.bytes).into_owned();
            let output = match chunk.stream {
                crate::application::ports::ProcessStream::Stdout => ActionOutput::stdout(message),
                crate::application::ports::ProcessStream::Stderr => ActionOutput::stderr(message),
            };
            self.output.emit(output).await
        })
    }
}

fn session_record(
    action: &ActionSpec,
    session: &TmuxSessionSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
    session_name: &str,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::TmuxSession, session.identity.clone())
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(action.id.clone());
    record.observed_before = observed_before;
    record
        .integration_metadata
        .insert("session_name".to_owned(), session_name.to_owned());
    record.integration_metadata.insert(
        "environment_slug".to_owned(),
        action
            .resolved_environment
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "unknown".to_owned()),
    );
    Ok(record)
}

fn window_record(
    target: &TmuxCommandTarget<'_>,
    session: &TmuxSessionSnapshot,
    window: &TmuxWindowSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
    session_ownership: OwnershipStatus,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::TmuxWindow, window.identity.clone())
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(target.action.id.clone());
    record.observed_before = observed_before;
    record
        .integration_metadata
        .insert("session_name".to_owned(), target.session_name.to_owned());
    record
        .integration_metadata
        .insert("session_identity".to_owned(), session.identity.clone());
    record
        .integration_metadata
        .insert("window_name".to_owned(), target.window_name.to_owned());
    record.integration_metadata.insert(
        "session_owned".to_owned(),
        session_ownership.is_environment_owned().to_string(),
    );
    record
        .integration_metadata
        .insert("command_program".to_owned(), target.request.program.clone());
    record.integration_metadata.insert(
        "working_directory".to_owned(),
        target
            .request
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    );
    if let Some(process_id) = window.process_id {
        record
            .integration_metadata
            .insert("process_id".to_owned(), process_id.to_string());
    }
    Ok(record)
}

fn window_is_healthy(window: &TmuxWindowSnapshot, request: &ProcessRequest) -> bool {
    let command_matches = window
        .command
        .as_deref()
        .is_none_or(|command| executable_name(command) == executable_name(&request.program));
    let directory_matches = window
        .working_directory
        .as_ref()
        .zip(request.working_directory.as_ref())
        .is_none_or(|(actual, expected)| actual == expected);
    command_matches && directory_matches
}

fn executable_name(value: &str) -> &str {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
}

fn session_name_from_record(action: &ActionSpec, resource: &ResourceRecord) -> Result<String> {
    resource
        .integration_metadata
        .get("session_name")
        .cloned()
        .or_else(|| action.resolved_environment.as_ref().map(session_name))
        .ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Persistence,
                "tmux resource is missing its canonical session name",
            )
            .with_context("resource", resource.resource.to_string())
        })
}

fn resource_identity(record: &ResourceRecord) -> String {
    if record.resource.kind == ResourceKind::TmuxSession {
        record.resource.stable_identity.clone()
    } else {
        record
            .integration_metadata
            .get("session_identity")
            .cloned()
            .unwrap_or_default()
    }
}

fn command_exit_error(
    action: &ActionSpec,
    output: &crate::application::ports::ProcessOutput,
) -> WorkstateError {
    let mut error = WorkstateError::new(
        ErrorCategory::Process,
        format!("command for action '{}' exited unsuccessfully", action.id),
    )
    .with_context("action_id", action.id.to_string())
    .with_context(
        "exit_status",
        output
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        error = error.with_context("stderr", stderr);
    }
    error
}

pub fn register_handlers(
    registry: &mut ActionHandlerRegistry,
    process_runner: Arc<dyn ProcessRunner>,
    tmux: Arc<dyn TmuxBackend>,
    file_system: Arc<dyn FileSystem>,
) -> Result<()> {
    let session_lock = Arc::new(tokio::sync::Mutex::new(()));
    for key in ["run_command", "start_service"] {
        let handler = CommandActionHandler::new(
            key,
            Arc::clone(&process_runner),
            Arc::clone(&tmux),
            Arc::clone(&file_system),
        )?
        .with_session_lock(Arc::clone(&session_lock));
        registry.register(handler)?;
    }
    Ok(())
}
