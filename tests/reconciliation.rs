use std::{
    collections::BTreeSet,
    error::Error,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Barrier, Notify};
use workstate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, ActionOutputSink, ActionOutputStream, CancellationToken, ExecutionPlan,
            ObservationStatus, Planner, ReadinessCheckResult, ReadinessCheckRunner,
        },
        ports::{BackgroundProcess, BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner},
        reconciliation::{
            ApplicationEvent, ChannelEventSink, EventSink, InMemoryEventSink, Scheduler,
            SchedulerOptions,
        },
    },
    domain::{
        ActionId, ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, ExecutionMode,
        OwnershipStatus, ReadinessCheck, ResourceIdentity, ResourceKind, ResourceRecord,
        RetryPolicy,
    },
    error::{ErrorCategory, WorkstateError},
    integrations::IntegrationRegistry,
    platform::CapabilityId,
};

type TestResult = std::result::Result<(), Box<dyn Error>>;

struct TestState {
    apply_calls: AtomicUsize,
    run_once_calls: AtomicUsize,
    background_calls: AtomicUsize,
    active_actions: Arc<AtomicUsize>,
    max_active_actions: AtomicUsize,
    attempts: AtomicUsize,
    starts: Mutex<Vec<(String, Instant)>>,
    started: Notify,
}

impl TestState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            apply_calls: AtomicUsize::new(0),
            run_once_calls: AtomicUsize::new(0),
            background_calls: AtomicUsize::new(0),
            active_actions: Arc::new(AtomicUsize::new(0)),
            max_active_actions: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            starts: Mutex::new(Vec::new()),
            started: Notify::new(),
        })
    }
}

struct ActiveActionGuard {
    active_actions: Arc<AtomicUsize>,
}

impl ActiveActionGuard {
    fn new(state: &TestState) -> Self {
        let active = state.active_actions.fetch_add(1, Ordering::AcqRel) + 1;
        let mut maximum = state.max_active_actions.load(Ordering::Acquire);
        while active > maximum {
            match state.max_active_actions.compare_exchange(
                maximum,
                active,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => maximum = observed,
            }
        }
        Self {
            active_actions: Arc::clone(&state.active_actions),
        }
    }
}

impl Drop for ActiveActionGuard {
    fn drop(&mut self) {
        self.active_actions.fetch_sub(1, Ordering::AcqRel);
    }
}

struct TestHandler {
    key: &'static str,
    observation: ObservationStatus,
    state: Arc<TestState>,
    barrier: Option<Arc<Barrier>>,
    barrier_actions: BTreeSet<String>,
    fail_action: Option<String>,
    fail_first: bool,
    cancellation_action: Option<String>,
    output_action: Option<String>,
    return_background_resource: bool,
}

impl TestHandler {
    fn operation<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        background: bool,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        let state = Arc::clone(&self.state);
        let barrier = self.barrier.clone();
        let barrier_actions = self.barrier_actions.clone();
        let fail_action = self.fail_action.clone();
        let fail_first = self.fail_first;
        let cancellation_action = self.cancellation_action.clone();
        let output_action = self.output_action.clone();
        let return_background_resource = self.return_background_resource;
        let action_id = action.id.to_string();
        Box::pin(async move {
            cancellation.check()?;
            state.apply_calls.fetch_add(1, Ordering::AcqRel);
            if background {
                state.background_calls.fetch_add(1, Ordering::AcqRel);
            } else {
                state.run_once_calls.fetch_add(1, Ordering::AcqRel);
            }
            state
                .starts
                .lock()
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "start recorder lock failed")
                })?
                .push((action_id.clone(), Instant::now()));
            state.started.notify_one();
            let _active_guard = ActiveActionGuard::new(&state);

            if cancellation_action.as_deref() == Some(action_id.as_str()) {
                cancellation.cancelled().await;
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    "fake action was cancelled",
                )
                .with_context("cancelled", "true")
                .with_context("action_id", action_id));
            }

            if barrier_actions.contains(action_id.as_str())
                && let Some(barrier) = barrier
            {
                barrier.wait().await;
            }

            let attempt = state.attempts.fetch_add(1, Ordering::AcqRel);
            if fail_action.as_deref() == Some(action_id.as_str()) {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("fake action '{action_id}' failed"),
                )
                .with_context("action_id", action_id));
            }
            if fail_first && attempt == 0 {
                return Err(
                    WorkstateError::new(ErrorCategory::Runtime, "fake transient failure")
                        .with_context("action_id", action_id),
                );
            }

            let resources = if background && return_background_resource {
                let identity =
                    ResourceIdentity::new(ResourceKind::Process, format!("background-{action_id}"))
                        .map_err(WorkstateError::from)?;
                let mut record =
                    ResourceRecord::new(identity, OwnershipStatus::CreatedByCurrentRun);
                record.action_id = Some(action.id.clone());
                vec![record]
            } else {
                Vec::new()
            };
            let outputs = if output_action.as_deref() == Some(action_id.as_str()) {
                vec![
                    ActionOutput::stdout("stdout message"),
                    ActionOutput::stderr("stderr message"),
                    ActionOutput::log("log message"),
                ]
            } else {
                Vec::new()
            };
            Ok(ActionExecutionResult {
                changed: true,
                resources,
                mutations: Vec::new(),
                outputs,
            })
        })
    }
}

impl ActionHandler for TestHandler {
    fn action_key(&self) -> &str {
        self.key
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionObservation>> {
        let observation = self.observation;
        Box::pin(async move {
            cancellation.check()?;
            Ok(ActionObservation {
                status: observation,
                detail: Some(format!("observed {}", action.id)),
                resources: Vec::new(),
            })
        })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        self.operation(action, cancellation, false)
    }

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        self.operation(action, cancellation, false)
    }

    fn start_background<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        self.operation(action, cancellation, true)
    }
}

struct PassingReadiness;

impl ReadinessCheckRunner for PassingReadiness {
    fn check<'a>(
        &'a self,
        _action_id: &'a ActionId,
        _check: &'a ReadinessCheck,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ReadinessCheckResult>> {
        Box::pin(async move {
            cancellation.check()?;
            Ok(ReadinessCheckResult::passed())
        })
    }
}

struct SlowReadiness;

impl ReadinessCheckRunner for SlowReadiness {
    fn check<'a>(
        &'a self,
        _action_id: &'a ActionId,
        _check: &'a ReadinessCheck,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ReadinessCheckResult>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(ReadinessCheckResult::passed())
        })
    }
}

struct FakeProcessRunner {
    run_calls: Arc<AtomicUsize>,
    background_calls: Arc<AtomicUsize>,
}

impl ProcessRunner for FakeProcessRunner {
    fn run<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, workstate::Result<ProcessOutput>> {
        self.run_calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: format!("{} output", request.program).into_bytes(),
                stderr: Vec::new(),
            })
        })
    }

    fn start_background<'a>(
        &'a self,
        _request: ProcessRequest,
    ) -> BoxFuture<'a, workstate::Result<BackgroundProcess>> {
        self.background_calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { BackgroundProcess::new("fake-session/fake-window") })
    }
}

struct ProcessBackedHandler {
    runner: Arc<dyn ProcessRunner>,
}

impl ProcessBackedHandler {
    fn request_for(action: &ActionSpec) -> ProcessRequest {
        let command = action.parameters.command.as_ref();
        ProcessRequest {
            program: command
                .map(|command| command.program.clone())
                .unwrap_or_else(|| "test-command".to_owned()),
            arguments: command
                .map(|command| command.arguments.clone())
                .unwrap_or_default(),
            working_directory: action.working_directory.clone().map(PathBuf::from),
            environment: command
                .map(|command| {
                    command
                        .environment
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

impl ActionHandler for ProcessBackedHandler {
    fn action_key(&self) -> &str {
        "run_command"
    }

    fn observe<'a>(
        &'a self,
        _action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionObservation>> {
        Box::pin(async move {
            cancellation.check()?;
            Ok(ActionObservation::requires_change())
        })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        self.run_once(action, cancellation)
    }

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        let runner = Arc::clone(&self.runner);
        let request = Self::request_for(action);
        Box::pin(async move {
            let output = runner.run(request).await?;
            if !output.succeeded() {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "fake run-once command failed",
                ));
            }
            cancellation.check()?;
            Ok(ActionExecutionResult {
                changed: true,
                resources: Vec::new(),
                mutations: Vec::new(),
                outputs: vec![ActionOutput::stdout(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )],
            })
        })
    }

    fn run_once_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        let runner = Arc::clone(&self.runner);
        let request = Self::request_for(action);
        Box::pin(async move {
            let process_output = runner.run(request).await?;
            if !process_output.succeeded() {
                return Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "fake run-once command failed",
                ));
            }
            cancellation.check()?;
            output
                .emit(ActionOutput::stdout(
                    String::from_utf8_lossy(&process_output.stdout).into_owned(),
                ))
                .await?;
            Ok(ActionExecutionResult {
                changed: true,
                resources: Vec::new(),
                mutations: Vec::new(),
                outputs: Vec::new(),
            })
        })
    }

    fn start_background<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, workstate::Result<ActionExecutionResult>> {
        let runner = Arc::clone(&self.runner);
        let request = Self::request_for(action);
        Box::pin(async move {
            let process = runner.start_background(request).await?;
            cancellation.check()?;
            let identity = ResourceIdentity::new(ResourceKind::Process, process.identity)
                .map_err(WorkstateError::from)?;
            let mut resource = ResourceRecord::new(identity, OwnershipStatus::CreatedByCurrentRun);
            resource.action_id = Some(action.id.clone());
            Ok(ActionExecutionResult {
                changed: true,
                resources: vec![resource],
                mutations: Vec::new(),
                outputs: Vec::new(),
            })
        })
    }
}

fn command_action(
    id: &str,
    mode: ExecutionMode,
) -> std::result::Result<ActionSpec, Box<dyn Error>> {
    let mut action = ActionSpec::new(id, ActionKind::RunCommand)?;
    action.parameters.command = Some(CommandSpec::new("test-command"));
    action.execution_mode = Some(mode);
    Ok(action)
}

fn integrations_with_background_processes()
-> std::result::Result<IntegrationRegistry, Box<dyn Error>> {
    let mut integrations = IntegrationRegistry::new();
    integrations.set_capability_availability(CapabilityId::BackgroundProcesses, true, None)?;
    Ok(integrations)
}

async fn observed_plan(
    configuration: &EnvironmentConfig,
    integrations: &IntegrationRegistry,
    handlers: &ActionHandlerRegistry,
) -> std::result::Result<ExecutionPlan, Box<dyn Error>> {
    let planner = Planner::new(integrations, handlers);
    let mut plan = planner.build(configuration)?;
    planner.observe(&mut plan, CancellationToken::new()).await?;
    Ok(plan)
}

fn scheduler(handlers: &ActionHandlerRegistry, options: SchedulerOptions) -> Scheduler {
    Scheduler::new(
        Arc::new(handlers.clone()),
        Arc::new(PassingReadiness),
        options,
    )
}

#[tokio::test]
async fn already_correct_actions_are_not_restarted() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration
        .actions
        .push(command_action("api", ExecutionMode::RunOnce)?);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::AlreadyCorrect,
        state: Arc::clone(&state),
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let events = Arc::new(InMemoryEventSink::default());
    let report = scheduler(&handlers, SchedulerOptions::default())
        .execute(&plan, CancellationToken::new(), events.clone(), false)
        .await?;

    assert_eq!(report.already_correct_count, 1);
    assert_eq!(report.changed_count, 0);
    assert_eq!(state.run_once_calls.load(Ordering::Acquire), 0);
    assert!(report.statuses.values().all(|status| {
        *status == workstate::application::reconciliation::ActionRunStatus::AlreadyCorrect
    }));
    let snapshot = events.snapshot()?;
    assert!(snapshot.iter().any(|event| matches!(
        event,
        ApplicationEvent::ActionReady {
            action_id,
            already_correct: true
        } if action_id.as_str() == "api"
    )));
    Ok(())
}

#[tokio::test]
async fn independent_actions_run_concurrently_and_dependents_wait() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    let first = command_action("first", ExecutionMode::RunOnce)?;
    let second = command_action("second", ExecutionMode::RunOnce)?;
    let mut dependent = command_action("dependent", ExecutionMode::RunOnce)?;
    dependent.depends_on = vec![ActionId::new("first")?, ActionId::new("second")?];
    configuration.actions = vec![first, second, dependent];
    let state = TestState::new();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_actions = BTreeSet::from(["first".to_owned(), "second".to_owned()]);
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state: Arc::clone(&state),
        barrier: Some(barrier),
        barrier_actions,
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let report = scheduler(
        &handlers,
        SchedulerOptions::new(2, Duration::from_secs(1), Duration::from_secs(1))?,
    )
    .execute(
        &plan,
        CancellationToken::new(),
        Arc::new(InMemoryEventSink::default()),
        false,
    )
    .await?;

    let starts = state
        .starts
        .lock()
        .map_err(|_| std::io::Error::other("start recorder lock failed"))?
        .clone();
    assert_eq!(starts.len(), 3);
    assert_eq!(starts.first().map(|entry| entry.0.as_str()), Some("first"));
    assert_eq!(starts.get(1).map(|entry| entry.0.as_str()), Some("second"));
    assert_eq!(
        starts.get(2).map(|entry| entry.0.as_str()),
        Some("dependent")
    );
    assert_eq!(state.max_active_actions.load(Ordering::Acquire), 2);
    assert_eq!(report.changed_count, 3);
    Ok(())
}

#[tokio::test]
async fn action_failure_blocks_dependents_and_reports_the_failed_action() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration.actions = vec![
        command_action("fail", ExecutionMode::RunOnce)?,
        command_action("dependent", ExecutionMode::RunOnce)?,
    ];
    configuration.actions[1]
        .depends_on
        .push(ActionId::new("fail")?);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state: Arc::clone(&state),
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: Some("fail".to_owned()),
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let events = Arc::new(InMemoryEventSink::default());
    let result = scheduler(&handlers, SchedulerOptions::default())
        .execute(&plan, CancellationToken::new(), events.clone(), false)
        .await;
    let error = match result {
        Ok(_) => {
            return Err(std::io::Error::other("the failing action unexpectedly succeeded").into());
        }
        Err(error) => error,
    };
    assert_eq!(
        error.context.get("action_id").map(String::as_str),
        Some("fail")
    );
    let starts = state
        .starts
        .lock()
        .map_err(|_| std::io::Error::other("start recorder lock failed"))?
        .clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts.first().map(|entry| entry.0.as_str()), Some("fail"));
    let snapshot = events.snapshot()?;
    assert!(snapshot.iter().any(|event| matches!(
        event,
        ApplicationEvent::ActionSkipped { action_id, .. }
            if action_id.as_str() == "dependent"
    )));
    Ok(())
}

#[tokio::test]
async fn run_once_and_background_actions_use_distinct_process_lifetimes() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration.actions = vec![
        command_action("once", ExecutionMode::RunOnce)?,
        command_action("daemon", ExecutionMode::Background)?,
    ];
    let run_calls = Arc::new(AtomicUsize::new(0));
    let background_calls = Arc::new(AtomicUsize::new(0));
    let handler = ProcessBackedHandler {
        runner: Arc::new(FakeProcessRunner {
            run_calls: Arc::clone(&run_calls),
            background_calls: Arc::clone(&background_calls),
        }),
    };
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(handler)?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let events = Arc::new(InMemoryEventSink::default());
    let report = scheduler(&handlers, SchedulerOptions::default())
        .execute(&plan, CancellationToken::new(), events.clone(), false)
        .await?;

    let daemon_id = ActionId::new("daemon")?;
    assert_eq!(run_calls.load(Ordering::Acquire), 1);
    assert_eq!(background_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        report
            .results
            .get(&daemon_id)
            .map(|result| result.resources.len()),
        Some(1)
    );
    let snapshot = events.snapshot()?;
    assert!(snapshot.iter().any(|event| matches!(
        event,
        ApplicationEvent::ActionOutput {
            action_id,
            stream: ActionOutputStream::Stdout,
            message
        } if action_id.as_str() == "once" && message == "test-command output"
    )));
    assert!(snapshot.iter().any(|event| matches!(
        event,
        ApplicationEvent::ActionStarted {
            action_id,
            execution_mode: Some(ExecutionMode::Background),
            ..
        } if action_id.as_str() == "daemon"
    )));
    Ok(())
}

#[tokio::test]
async fn action_outputs_are_emitted_before_the_action_ready_event() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration
        .actions
        .push(command_action("output", ExecutionMode::RunOnce)?);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state,
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: Some("output".to_owned()),
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let events = Arc::new(InMemoryEventSink::default());
    scheduler(&handlers, SchedulerOptions::default())
        .execute(&plan, CancellationToken::new(), events.clone(), false)
        .await?;
    let snapshot = events.snapshot()?;
    let action_events = snapshot
        .iter()
        .filter_map(|event| match event {
            ApplicationEvent::ActionOutput {
                action_id, message, ..
            } if action_id.as_str() == "output" => Some(message.clone()),
            ApplicationEvent::ActionReady { action_id, .. } if action_id.as_str() == "output" => {
                Some("ready".to_owned())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        action_events,
        vec![
            "stdout message".to_owned(),
            "stderr message".to_owned(),
            "log message".to_owned(),
            "ready".to_owned(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn retry_attempts_are_visible_in_action_started_events() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    let mut action = command_action("retry", ExecutionMode::RunOnce)?;
    action.retry_policy = RetryPolicy {
        max_attempts: 2,
        delay_milliseconds: 0,
    };
    configuration.actions.push(action);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state: Arc::clone(&state),
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: true,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let events = Arc::new(InMemoryEventSink::default());
    scheduler(&handlers, SchedulerOptions::default())
        .execute(&plan, CancellationToken::new(), events.clone(), false)
        .await?;
    assert_eq!(state.attempts.load(Ordering::Acquire), 2);
    let snapshot = events.snapshot()?;
    let attempts = snapshot
        .iter()
        .filter_map(|event| match event {
            ApplicationEvent::ActionStarted {
                action_id, attempt, ..
            } if action_id.as_str() == "retry" => Some(*attempt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attempts, vec![1, 2]);
    Ok(())
}

#[tokio::test]
async fn cancellation_stops_running_work_and_marks_the_action_cancelled() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration
        .actions
        .push(command_action("long", ExecutionMode::RunOnce)?);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state: Arc::clone(&state),
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: Some("long".to_owned()),
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let scheduler = scheduler(&handlers, SchedulerOptions::default());
    let events = Arc::new(InMemoryEventSink::default());
    let cancellation = CancellationToken::new();
    let run_cancellation = cancellation.clone();
    let run_plan = plan.clone();
    let run_events = Arc::clone(&events);
    let started = state.started.notified();
    let task = tokio::spawn(async move {
        scheduler
            .execute(&run_plan, run_cancellation, run_events, false)
            .await
    });
    tokio::time::timeout(Duration::from_millis(100), started).await?;
    cancellation.cancel();
    let result = task.await?;
    let error = match result {
        Ok(_) => {
            return Err(std::io::Error::other("cancelled action unexpectedly succeeded").into());
        }
        Err(error) => error,
    };
    assert_eq!(
        error.context.get("cancelled").map(String::as_str),
        Some("true")
    );
    let snapshot = events.snapshot()?;
    assert!(snapshot.iter().any(|event| matches!(
        event,
        ApplicationEvent::ActionCancelled { action_id, .. }
            if action_id.as_str() == "long"
    )));
    Ok(())
}

#[tokio::test]
async fn readiness_timeout_contains_the_action_identity() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    let mut action = command_action("api", ExecutionMode::RunOnce)?;
    action
        .readiness_checks
        .push(ReadinessCheck::Delay { milliseconds: 1 });
    configuration.actions.push(action);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state,
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let result = Scheduler::new(
        Arc::new(handlers),
        Arc::new(SlowReadiness),
        SchedulerOptions::new(1, Duration::from_secs(1), Duration::from_millis(5))?,
    )
    .execute(
        &plan,
        CancellationToken::new(),
        Arc::new(InMemoryEventSink::default()),
        false,
    )
    .await;
    let error = match result {
        Ok(_) => return Err(std::io::Error::other("readiness unexpectedly succeeded").into()),
        Err(error) => error,
    };
    assert_eq!(
        error.context.get("action_id").map(String::as_str),
        Some("api")
    );
    assert_eq!(
        error
            .context
            .get("timeout_milliseconds")
            .map(String::as_str),
        Some("1")
    );
    Ok(())
}

#[tokio::test]
async fn dry_run_skips_mutations_and_background_handoffs() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration.actions = vec![
        command_action("once", ExecutionMode::RunOnce)?,
        command_action("daemon", ExecutionMode::Background)?,
    ];
    let run_calls = Arc::new(AtomicUsize::new(0));
    let background_calls = Arc::new(AtomicUsize::new(0));
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(ProcessBackedHandler {
        runner: Arc::new(FakeProcessRunner {
            run_calls: Arc::clone(&run_calls),
            background_calls: Arc::clone(&background_calls),
        }),
    })?;
    let integrations = integrations_with_background_processes()?;
    let plan = observed_plan(&configuration, &integrations, &handlers).await?;
    let report = scheduler(&handlers, SchedulerOptions::default())
        .execute(
            &plan,
            CancellationToken::new(),
            Arc::new(InMemoryEventSink::default()),
            true,
        )
        .await?;
    assert_eq!(report.changed_count, 0);
    assert_eq!(report.skipped_count, 2);
    assert_eq!(run_calls.load(Ordering::Acquire), 0);
    assert_eq!(background_calls.load(Ordering::Acquire), 0);
    Ok(())
}

#[tokio::test]
async fn bounded_event_sink_applies_backpressure_without_unbounded_storage() -> TestResult {
    let (sink, mut receiver) = ChannelEventSink::bounded(1)?;
    let action_id = ActionId::new("event")?;
    sink.emit(ApplicationEvent::ActionSkipped {
        action_id: action_id.clone(),
        reason: "first".to_owned(),
    })
    .await?;
    let mut pending = Box::pin(sink.emit(ApplicationEvent::ActionSkipped {
        action_id,
        reason: "second".to_owned(),
    }));
    let blocked = tokio::time::timeout(Duration::from_millis(20), &mut pending).await;
    assert!(blocked.is_err());
    let received = receiver.recv().await;
    assert!(received.is_some());
    assert!(pending.await.is_ok());
    Ok(())
}

#[test]
fn action_handler_registry_supports_typed_lookup_and_duplicate_rejection() -> TestResult {
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state: Arc::clone(&state),
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    assert!(handlers.handler_for(&ActionKind::RunCommand).is_some());
    assert!(handlers.handler_by_key("run_command").is_some());
    assert_eq!(handlers.len(), 1);
    assert!(
        handlers
            .register(TestHandler {
                key: "run_command",
                observation: ObservationStatus::RequiresChange,
                state,
                barrier: None,
                barrier_actions: BTreeSet::new(),
                fail_action: None,
                fail_first: false,
                cancellation_action: None,
                output_action: None,
                return_background_resource: false,
            })
            .is_err()
    );
    Ok(())
}

#[test]
fn planner_classifies_missing_capabilities_before_execution() -> TestResult {
    let mut configuration = EnvironmentConfig::new("Blog")?;
    configuration
        .actions
        .push(command_action("api", ExecutionMode::RunOnce)?);
    let state = TestState::new();
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(TestHandler {
        key: "run_command",
        observation: ObservationStatus::RequiresChange,
        state,
        barrier: None,
        barrier_actions: BTreeSet::new(),
        fail_action: None,
        fail_first: false,
        cancellation_action: None,
        output_action: None,
        return_background_resource: false,
    })?;
    let integrations = IntegrationRegistry::new();
    let planner = Planner::new(&integrations, &handlers);
    let plan = planner.build(&configuration)?;
    let entry = plan.entries().next();
    assert!(matches!(
        entry.map(|entry| entry.classification),
        Some(workstate::application::planner::PlanClassification::BlockedByMissingCapability)
    ));
    Ok(())
}
