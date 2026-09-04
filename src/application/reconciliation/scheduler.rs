use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::task::JoinSet;

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionOutput,
            ActionOutputSink, CancellationToken, ExecutionPlan, PlanClassification,
            ReadinessCheckRunner, cancellation_error, is_cancellation_error, run_with_timeout,
        },
        ports::{Clock, SystemClock},
        reconciliation::{ApplicationEvent, EventSink},
    },
    domain::{ActionId, ActionSpec},
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRunStatus {
    Pending,
    Running,
    AlreadyCorrect,
    Ready,
    Skipped,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub environment: crate::domain::EnvironmentSlug,
    pub dry_run: bool,
    pub statuses: BTreeMap<ActionId, ActionRunStatus>,
    pub results: BTreeMap<ActionId, ActionExecutionResult>,
    pub changed_count: usize,
    pub already_correct_count: usize,
    pub skipped_count: usize,
    pub planned_change_count: usize,
    pub elapsed_milliseconds: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerOptions {
    pub max_concurrency: usize,
    pub default_action_timeout: Duration,
    pub default_readiness_timeout: Duration,
}

impl SchedulerOptions {
    pub fn new(
        max_concurrency: usize,
        default_action_timeout: Duration,
        default_readiness_timeout: Duration,
    ) -> Result<Self> {
        if max_concurrency == 0 {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "scheduler concurrency must be greater than zero",
            ));
        }
        if default_action_timeout.is_zero() || default_readiness_timeout.is_zero() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "scheduler timeouts must be greater than zero",
            ));
        }
        Ok(Self {
            max_concurrency,
            default_action_timeout,
            default_readiness_timeout,
        })
    }
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        let max_concurrency = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        Self {
            max_concurrency,
            default_action_timeout: Duration::from_secs(30),
            default_readiness_timeout: Duration::from_secs(30),
        }
    }
}

pub struct Scheduler {
    handlers: Arc<ActionHandlerRegistry>,
    readiness_runner: Arc<dyn ReadinessCheckRunner>,
    clock: Arc<dyn Clock>,
    options: SchedulerOptions,
}

impl Scheduler {
    pub fn new(
        handlers: Arc<ActionHandlerRegistry>,
        readiness_runner: Arc<dyn ReadinessCheckRunner>,
        options: SchedulerOptions,
    ) -> Self {
        Self::with_clock(handlers, readiness_runner, Arc::new(SystemClock), options)
    }

    pub fn with_clock(
        handlers: Arc<ActionHandlerRegistry>,
        readiness_runner: Arc<dyn ReadinessCheckRunner>,
        clock: Arc<dyn Clock>,
        options: SchedulerOptions,
    ) -> Self {
        Self {
            handlers,
            readiness_runner,
            clock,
            options,
        }
    }

    pub async fn execute(
        &self,
        plan: &ExecutionPlan,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
        dry_run: bool,
    ) -> Result<RunReport> {
        self.validate_options()?;
        self.validate_plan(plan)?;
        let started_at = self.clock.monotonic_now();

        if dry_run {
            return self
                .execute_dry_run(plan, started_at, events, cancellation)
                .await;
        }

        let mut statuses = plan
            .ordered_action_ids()
            .iter()
            .map(|action_id| (action_id.clone(), ActionRunStatus::Pending))
            .collect::<BTreeMap<_, _>>();
        let mut results = BTreeMap::new();
        let mut running_ids = BTreeSet::new();
        let mut tasks = JoinSet::new();
        let mut primary_error = None;
        let mut primary_action = None;
        let mut cancellation_requested = cancellation.is_cancelled();

        loop {
            if primary_error.is_none() && cancellation.is_cancelled() {
                cancellation_requested = true;
                primary_error = Some(cancellation_error(None, "scheduling"));
            }

            if primary_error.is_none() {
                self.mark_already_correct(plan, &mut statuses, &events)
                    .await?;

                let available_slots = self
                    .options
                    .max_concurrency
                    .saturating_sub(running_ids.len());
                if available_slots > 0 {
                    let candidates = plan
                        .ordered_action_ids()
                        .iter()
                        .filter(|action_id| {
                            statuses
                                .get(*action_id)
                                .is_some_and(|status| *status == ActionRunStatus::Pending)
                        })
                        .filter(|action_id| {
                            plan.entry(action_id).is_some_and(|entry| {
                                entry.classification == PlanClassification::RequiresChange
                                    && dependencies_ready(entry, &statuses)
                            })
                        })
                        .take(available_slots)
                        .cloned()
                        .collect::<Vec<_>>();

                    for action_id in candidates {
                        let Some(entry) = plan.entry(&action_id) else {
                            primary_error = Some(internal_scheduler_error(
                                "a runnable action disappeared from the execution plan",
                            ));
                            primary_action = Some(action_id);
                            break;
                        };
                        let Some(handler) = self.handlers.handler_for(&entry.action.kind) else {
                            primary_error = Some(internal_scheduler_error(format!(
                                "no handler is available for action '{action_id}'",
                            )));
                            primary_action = Some(action_id);
                            break;
                        };
                        statuses.insert(action_id.clone(), ActionRunStatus::Running);
                        running_ids.insert(action_id.clone());
                        events
                            .emit(ApplicationEvent::ActionStarted {
                                action_id: action_id.clone(),
                                attempt: 1,
                                execution_mode: entry.execution_mode,
                            })
                            .await?;
                        let action = entry.action.clone();
                        let token = cancellation.clone();
                        let runner = Arc::clone(&self.readiness_runner);
                        let action_timeout =
                            entry.timeout.unwrap_or(self.options.default_action_timeout);
                        let readiness_timeout = self.options.default_readiness_timeout;
                        let options = entry.retry_policy.clone();
                        let action_events = Arc::clone(&events);
                        let execution_mode = entry.execution_mode;
                        let execution_context = ActionExecutionContext {
                            readiness_runner: runner,
                            cancellation: token,
                            action_timeout,
                            default_readiness_timeout: readiness_timeout,
                            retry_policy: options,
                            events: action_events,
                            execution_mode,
                        };
                        tasks.spawn(async move {
                            let result = execute_action(handler, action, execution_context).await;
                            ActionTaskResult { action_id, result }
                        });
                    }
                }
            } else if !running_ids.is_empty() {
                cancellation.cancel();
            }

            if primary_error.is_some() && running_ids.is_empty() {
                if cancellation_requested {
                    self.cancel_pending(plan, &mut statuses, &events).await?;
                } else {
                    self.skip_pending(plan, &mut statuses, &events, primary_action.as_ref())
                        .await?;
                }
                break;
            }

            if primary_error.is_none()
                && running_ids.is_empty()
                && statuses.values().all(is_terminal)
            {
                break;
            }

            if running_ids.is_empty() {
                let error = internal_scheduler_error(
                    "the scheduler could not make progress through the execution plan",
                );
                primary_error = Some(error);
                continue;
            }

            let Some(joined) = tasks.join_next().await else {
                primary_error = Some(internal_scheduler_error(
                    "the scheduler lost a running action task",
                ));
                continue;
            };
            let task = match joined {
                Ok(task) => task,
                Err(error) => {
                    primary_error = Some(
                        WorkstateError::new(
                            ErrorCategory::Runtime,
                            "an action task ended without returning a result",
                        )
                        .with_context("join_error", error.to_string()),
                    );
                    cancellation_requested = true;
                    cancellation.cancel();
                    continue;
                }
            };
            running_ids.remove(&task.action_id);
            match task.result {
                Ok(result) => {
                    let output = result.outputs.clone();
                    results.insert(task.action_id.clone(), result);
                    statuses.insert(task.action_id.clone(), ActionRunStatus::Ready);
                    for output in output {
                        events
                            .emit(ApplicationEvent::ActionOutput {
                                action_id: task.action_id.clone(),
                                stream: output.stream,
                                message: output.message,
                            })
                            .await?;
                    }
                    events
                        .emit(ApplicationEvent::ActionReady {
                            action_id: task.action_id,
                            already_correct: false,
                        })
                        .await?;
                }
                Err(error) if is_cancellation_error(&error) => {
                    statuses.insert(task.action_id.clone(), ActionRunStatus::Cancelled);
                    events
                        .emit(ApplicationEvent::ActionCancelled {
                            action_id: task.action_id.clone(),
                            reason: error.to_string(),
                        })
                        .await?;
                    if primary_error.is_none() {
                        cancellation_requested = true;
                        primary_action = Some(task.action_id);
                        primary_error = Some(error);
                    }
                }
                Err(error) => {
                    statuses.insert(task.action_id.clone(), ActionRunStatus::Failed);
                    events
                        .emit(ApplicationEvent::ActionFailed {
                            action_id: task.action_id.clone(),
                            error: error.to_string(),
                        })
                        .await?;
                    if primary_error.is_none() {
                        primary_action = Some(task.action_id);
                        primary_error = Some(error);
                        cancellation.cancel();
                    }
                }
            }
        }

        if let Some(error) = primary_error {
            return Err(error);
        }

        let changed_count = results.values().filter(|result| result.changed).count();
        let already_correct_count = statuses
            .values()
            .filter(|status| **status == ActionRunStatus::AlreadyCorrect)
            .count();
        let skipped_count = statuses
            .values()
            .filter(|status| **status == ActionRunStatus::Skipped)
            .count();
        Ok(RunReport {
            environment: plan.environment.clone(),
            dry_run: false,
            statuses,
            results,
            changed_count,
            already_correct_count,
            skipped_count,
            planned_change_count: plan.expected_mutation_count(),
            elapsed_milliseconds: self.clock.elapsed_since(started_at).as_millis(),
        })
    }

    fn validate_plan(&self, plan: &ExecutionPlan) -> Result<()> {
        if plan.len() != plan.ordered_action_ids().len() {
            return Err(internal_scheduler_error(
                "the execution plan contains an inconsistent action index",
            ));
        }
        if !plan.is_observed() {
            return Err(internal_scheduler_error(
                "the execution plan must be observed before scheduling",
            ));
        }
        for entry in plan.entries() {
            match entry.classification {
                PlanClassification::BlockedByMissingCapability => {
                    let capabilities = entry
                        .missing_capabilities
                        .iter()
                        .map(|capability| capability.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        format!(
                            "action '{}' is blocked by missing capabilities: {capabilities}",
                            entry.action_id
                        ),
                    )
                    .with_context("action_id", entry.action_id.to_string())
                    .with_context("missing_capabilities", capabilities));
                }
                PlanClassification::Invalid => {
                    return Err(WorkstateError::new(
                        ErrorCategory::Runtime,
                        entry
                            .classification_detail
                            .clone()
                            .unwrap_or_else(|| format!("action '{}' is invalid", entry.action_id)),
                    )
                    .with_context("action_id", entry.action_id.to_string()));
                }
                PlanClassification::Unknown => {
                    return Err(internal_scheduler_error(format!(
                        "action '{}' has not been observed",
                        entry.action_id
                    ))
                    .with_context("action_id", entry.action_id.to_string()));
                }
                PlanClassification::AlreadyCorrect | PlanClassification::RequiresChange => {}
            }
        }
        Ok(())
    }

    fn validate_options(&self) -> Result<()> {
        SchedulerOptions::new(
            self.options.max_concurrency,
            self.options.default_action_timeout,
            self.options.default_readiness_timeout,
        )
        .map(|_| ())
    }

    async fn mark_already_correct(
        &self,
        plan: &ExecutionPlan,
        statuses: &mut BTreeMap<ActionId, ActionRunStatus>,
        events: &Arc<dyn EventSink>,
    ) -> Result<()> {
        for action_id in plan.ordered_action_ids() {
            let is_pending = statuses
                .get(action_id)
                .is_some_and(|status| *status == ActionRunStatus::Pending);
            let Some(entry) = plan.entry(action_id) else {
                return Err(internal_scheduler_error(
                    "an action ID was missing while marking observations",
                ));
            };
            if is_pending
                && entry.classification == PlanClassification::AlreadyCorrect
                && dependencies_ready(entry, statuses)
            {
                statuses.insert(action_id.clone(), ActionRunStatus::AlreadyCorrect);
                events
                    .emit(ApplicationEvent::ActionReady {
                        action_id: action_id.clone(),
                        already_correct: true,
                    })
                    .await?;
            }
        }
        Ok(())
    }

    async fn execute_dry_run(
        &self,
        plan: &ExecutionPlan,
        started_at: Instant,
        events: Arc<dyn EventSink>,
        cancellation: CancellationToken,
    ) -> Result<RunReport> {
        let mut statuses = BTreeMap::new();
        for (index, action_id) in plan.ordered_action_ids().iter().enumerate() {
            if cancellation.is_cancelled() {
                for cancelled_id in plan.ordered_action_ids().iter().skip(index) {
                    statuses.insert(cancelled_id.clone(), ActionRunStatus::Cancelled);
                    events
                        .emit(ApplicationEvent::ActionCancelled {
                            action_id: cancelled_id.clone(),
                            reason: "run cancelled before the dry run completed".to_owned(),
                        })
                        .await?;
                }
                return Err(cancellation_error(Some(action_id), "dry-run scheduling"));
            }
            let Some(entry) = plan.entry(action_id) else {
                return Err(internal_scheduler_error(
                    "an action ID was missing during dry-run scheduling",
                ));
            };
            match entry.classification {
                PlanClassification::AlreadyCorrect => {
                    statuses.insert(action_id.clone(), ActionRunStatus::AlreadyCorrect);
                    events
                        .emit(ApplicationEvent::ActionReady {
                            action_id: action_id.clone(),
                            already_correct: true,
                        })
                        .await?;
                }
                PlanClassification::RequiresChange => {
                    statuses.insert(action_id.clone(), ActionRunStatus::Skipped);
                    events
                        .emit(ApplicationEvent::ActionSkipped {
                            action_id: action_id.clone(),
                            reason: "dry run: mutation was not applied".to_owned(),
                        })
                        .await?;
                }
                PlanClassification::BlockedByMissingCapability
                | PlanClassification::Invalid
                | PlanClassification::Unknown => {
                    return Err(internal_scheduler_error(
                        "invalid plan classification reached dry-run execution",
                    )
                    .with_context("action_id", action_id.to_string()));
                }
            }
        }
        Ok(RunReport {
            environment: plan.environment.clone(),
            dry_run: true,
            statuses,
            results: BTreeMap::new(),
            changed_count: 0,
            already_correct_count: plan
                .entries()
                .filter(|entry| entry.classification == PlanClassification::AlreadyCorrect)
                .count(),
            skipped_count: plan
                .entries()
                .filter(|entry| entry.classification == PlanClassification::RequiresChange)
                .count(),
            planned_change_count: plan.expected_mutation_count(),
            elapsed_milliseconds: self.clock.elapsed_since(started_at).as_millis(),
        })
    }

    async fn skip_pending(
        &self,
        plan: &ExecutionPlan,
        statuses: &mut BTreeMap<ActionId, ActionRunStatus>,
        events: &Arc<dyn EventSink>,
        failed_action: Option<&ActionId>,
    ) -> Result<()> {
        let reason = failed_action
            .map(|action_id| format!("not started because action '{action_id}' failed"))
            .unwrap_or_else(|| "not started because the run failed".to_owned());
        for action_id in plan.ordered_action_ids() {
            if statuses
                .get(action_id)
                .is_some_and(|status| *status == ActionRunStatus::Pending)
            {
                statuses.insert(action_id.clone(), ActionRunStatus::Skipped);
                events
                    .emit(ApplicationEvent::ActionSkipped {
                        action_id: action_id.clone(),
                        reason: reason.clone(),
                    })
                    .await?;
            }
        }
        Ok(())
    }

    async fn cancel_pending(
        &self,
        plan: &ExecutionPlan,
        statuses: &mut BTreeMap<ActionId, ActionRunStatus>,
        events: &Arc<dyn EventSink>,
    ) -> Result<()> {
        for action_id in plan.ordered_action_ids() {
            if statuses
                .get(action_id)
                .is_some_and(|status| *status == ActionRunStatus::Pending)
            {
                statuses.insert(action_id.clone(), ActionRunStatus::Cancelled);
                events
                    .emit(ApplicationEvent::ActionCancelled {
                        action_id: action_id.clone(),
                        reason: "run cancelled before the action started".to_owned(),
                    })
                    .await?;
            }
        }
        Ok(())
    }
}

struct ActionTaskResult {
    action_id: ActionId,
    result: Result<ActionExecutionResult>,
}

struct ActionExecutionContext {
    readiness_runner: Arc<dyn ReadinessCheckRunner>,
    cancellation: CancellationToken,
    action_timeout: Duration,
    default_readiness_timeout: Duration,
    retry_policy: crate::domain::RetryPolicy,
    events: Arc<dyn EventSink>,
    execution_mode: Option<crate::domain::ExecutionMode>,
}

async fn execute_action(
    handler: Arc<dyn ActionHandler>,
    action: ActionSpec,
    context: ActionExecutionContext,
) -> Result<ActionExecutionResult> {
    let mut attempt = 1u32;
    loop {
        if attempt > 1 {
            context
                .events
                .emit(ApplicationEvent::ActionStarted {
                    action_id: action.id.clone(),
                    attempt,
                    execution_mode: context.execution_mode,
                })
                .await?;
        }
        context
            .cancellation
            .check()
            .map_err(|_| cancellation_error(Some(&action.id), "action execution"))?;
        let result = run_with_timeout(
            execute_attempt(
                Arc::clone(&handler),
                &action,
                Arc::clone(&context.readiness_runner),
                context.cancellation.clone(),
                context.default_readiness_timeout,
                Arc::new(ActionEventOutputSink {
                    action_id: action.id.clone(),
                    events: Arc::clone(&context.events),
                }),
            ),
            context.action_timeout,
            context.cancellation.clone(),
            Some(&action.id),
            "action execution",
        )
        .await;
        match result {
            Ok(result) => return Ok(result),
            Err(error) if is_cancellation_error(&error) => {
                return Err(error.with_context("action_id", action.id.to_string()));
            }
            Err(error) if attempt >= context.retry_policy.max_attempts => {
                return Err(error
                    .with_context("action_id", action.id.to_string())
                    .with_context("attempts", attempt.to_string()));
            }
            Err(_) => {
                if context.retry_policy.delay_milliseconds > 0 {
                    tokio::select! {
                        _ = context.cancellation.cancelled() => {
                            return Err(cancellation_error(Some(&action.id), "retry delay"));
                        }
                        _ = tokio::time::sleep(Duration::from_millis(context.retry_policy.delay_milliseconds)) => {}
                    }
                }
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

struct ActionEventOutputSink {
    action_id: ActionId,
    events: Arc<dyn EventSink>,
}

impl ActionOutputSink for ActionEventOutputSink {
    fn emit<'a>(
        &'a self,
        output: ActionOutput,
    ) -> crate::application::ports::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.events
                .emit(ApplicationEvent::ActionOutput {
                    action_id: self.action_id.clone(),
                    stream: output.stream,
                    message: output.message,
                })
                .await
        })
    }
}

async fn execute_attempt(
    handler: Arc<dyn ActionHandler>,
    action: &ActionSpec,
    readiness_runner: Arc<dyn ReadinessCheckRunner>,
    cancellation: CancellationToken,
    default_readiness_timeout: Duration,
    output: Arc<dyn ActionOutputSink>,
) -> Result<ActionExecutionResult> {
    cancellation.check()?;
    let result = match action.execution_mode {
        Some(crate::domain::ExecutionMode::Background) => {
            handler
                .start_background_with_output(action, cancellation.clone(), output)
                .await?
        }
        _ => {
            handler
                .run_once_with_output(action, cancellation.clone(), output)
                .await?
        }
    };
    if action.execution_mode == Some(crate::domain::ExecutionMode::Background)
        && result.resources.is_empty()
    {
        return Err(WorkstateError::new(
            ErrorCategory::Runtime,
            format!(
                "background action '{}' did not return a persistent resource identity",
                action.id
            ),
        )
        .with_context("action_id", action.id.to_string()));
    }
    handler
        .wait_for_readiness(
            action,
            readiness_runner.as_ref(),
            default_readiness_timeout,
            cancellation,
        )
        .await?;
    Ok(result)
}

fn dependencies_ready(
    entry: &crate::application::planner::PlanEntry,
    statuses: &BTreeMap<ActionId, ActionRunStatus>,
) -> bool {
    entry.dependencies.iter().all(|dependency| {
        statuses.get(dependency).is_some_and(|status| {
            matches!(
                status,
                ActionRunStatus::AlreadyCorrect | ActionRunStatus::Ready
            )
        })
    })
}

fn is_terminal(status: &ActionRunStatus) -> bool {
    matches!(
        status,
        ActionRunStatus::AlreadyCorrect
            | ActionRunStatus::Ready
            | ActionRunStatus::Skipped
            | ActionRunStatus::Failed
            | ActionRunStatus::Cancelled
    )
}

fn internal_scheduler_error(message: impl Into<String>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Runtime, message)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use crate::{
        application::{
            planner::{
                ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
                CancellationToken, ObservationStatus, ReadinessCheckResult, ReadinessCheckRunner,
            },
            ports::BoxFuture,
            reconciliation::{ApplicationEvent, InMemoryEventSink, Scheduler, SchedulerOptions},
        },
        domain::{
            ActionId, ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, ExecutionMode,
            ReadinessCheck,
        },
        integrations::IntegrationRegistry,
    };

    struct FakeHandler {
        key: &'static str,
        observation: ObservationStatus,
        delay: Duration,
        calls: Arc<Mutex<Vec<(String, Instant)>>>,
        fail: bool,
    }

    impl ActionHandler for FakeHandler {
        fn action_key(&self) -> &str {
            self.key
        }

        fn observe<'a>(
            &'a self,
            action: &'a ActionSpec,
            cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ActionObservation>> {
            let observation = self.observation;
            let delay = self.delay;
            let action_id = action.id.to_string();
            Box::pin(async move {
                if delay > Duration::ZERO {
                    tokio::select! {
                        _ = cancellation.cancelled() => {
                            return Err(crate::error::WorkstateError::new(
                                crate::error::ErrorCategory::Runtime,
                                "observation cancelled",
                            ).with_context("cancelled", "true"));
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                Ok(ActionObservation {
                    status: observation,
                    detail: Some(action_id),
                    resources: Vec::new(),
                })
            })
        }

        fn apply<'a>(
            &'a self,
            action: &'a ActionSpec,
            cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ActionExecutionResult>> {
            let delay = self.delay;
            let calls = Arc::clone(&self.calls);
            let fail = self.fail;
            let action_id = action.id.to_string();
            Box::pin(async move {
                cancellation.check()?;
                calls
                    .lock()
                    .map_err(|_| {
                        crate::error::WorkstateError::new(
                            crate::error::ErrorCategory::Runtime,
                            "call recorder lock failed",
                        )
                    })?
                    .push((action_id.clone(), Instant::now()));
                if delay > Duration::ZERO {
                    tokio::select! {
                        _ = cancellation.cancelled() => {
                            return Err(crate::error::WorkstateError::new(
                                crate::error::ErrorCategory::Runtime,
                                "action cancelled",
                            ).with_context("cancelled", "true"));
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                if fail {
                    Err(crate::error::WorkstateError::new(
                        crate::error::ErrorCategory::Runtime,
                        format!("fake action '{action_id}' failed"),
                    )
                    .with_context("action_id", action_id))
                } else {
                    Ok(ActionExecutionResult::default())
                }
            })
        }
    }

    struct PassingReadiness;

    impl ReadinessCheckRunner for PassingReadiness {
        fn check<'a>(
            &'a self,
            _action_id: &'a ActionId,
            _check: &'a ReadinessCheck,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ReadinessCheckResult>> {
            Box::pin(async { Ok(ReadinessCheckResult::passed()) })
        }
    }

    fn command_action(id: &str) -> Option<ActionSpec> {
        let mut action = ActionSpec::new(id, ActionKind::RunCommand).ok()?;
        action.parameters.command = Some(CommandSpec::new("test-command"));
        action.execution_mode = Some(ExecutionMode::RunOnce);
        Some(action)
    }

    fn test_integrations() -> IntegrationRegistry {
        let mut registry = IntegrationRegistry::new();
        assert!(
            registry
                .set_capability_availability(
                    crate::platform::CapabilityId::BackgroundProcesses,
                    true,
                    None,
                )
                .is_ok()
        );
        registry
    }

    #[tokio::test]
    async fn independent_actions_run_concurrently_and_dependents_wait() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let Some(first) = command_action("first") else {
            return;
        };
        let Some(mut second) = command_action("second") else {
            return;
        };
        let Some(first_id) = ActionId::new("first").ok() else {
            return;
        };
        second.depends_on.push(first_id);
        let mut configuration = EnvironmentConfig::new("Blog").ok();
        assert!(configuration.is_some());
        let Some(mut configuration) = configuration.take() else {
            return;
        };
        configuration.actions = vec![first, second];
        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "run_command",
                    observation: ObservationStatus::RequiresChange,
                    delay: Duration::from_millis(5),
                    calls: Arc::clone(&calls),
                    fail: false,
                })
                .is_ok()
        );
        let integrations = test_integrations();
        let planner = crate::application::planner::Planner::new(&integrations, &handlers);
        let plan = planner.build(&configuration);
        assert!(plan.is_ok());
        let Some(mut plan) = plan.ok() else {
            return;
        };
        assert!(
            planner
                .observe(&mut plan, CancellationToken::new())
                .await
                .is_ok()
        );
        let scheduler = Scheduler::new(
            Arc::new(handlers),
            Arc::new(PassingReadiness),
            SchedulerOptions::new(2, Duration::from_secs(1), Duration::from_secs(1))
                .ok()
                .unwrap_or_default(),
        );
        let events = Arc::new(InMemoryEventSink::default());
        let result = scheduler
            .execute(&plan, CancellationToken::new(), events.clone(), false)
            .await;
        assert!(result.is_ok());
        let Some(result) = result.ok() else {
            return;
        };
        assert_eq!(result.changed_count, 0);
        assert!(
            result
                .statuses
                .values()
                .all(|status| { matches!(status, super::ActionRunStatus::Ready) })
        );
        let snapshot = events.snapshot();
        assert!(snapshot.is_ok());
        let Some(snapshot) = snapshot.ok() else {
            return;
        };
        let started = snapshot
            .iter()
            .filter(|event| matches!(event, ApplicationEvent::ActionStarted { .. }))
            .count();
        assert_eq!(started, 2);
    }

    #[tokio::test]
    async fn failed_action_skips_pending_dependents() {
        let Some(failing) = command_action("fail") else {
            return;
        };
        let Some(mut dependent) = command_action("dependent") else {
            return;
        };
        let Some(failing_id) = ActionId::new("fail").ok() else {
            return;
        };
        dependent.depends_on.push(failing_id);
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        configuration.actions = vec![failing, dependent];
        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "run_command",
                    observation: ObservationStatus::RequiresChange,
                    delay: Duration::ZERO,
                    calls: Arc::new(Mutex::new(Vec::new())),
                    fail: true,
                })
                .is_ok()
        );
        let integrations = test_integrations();
        let planner = crate::application::planner::Planner::new(&integrations, &handlers);
        let plan = planner.build(&configuration);
        assert!(plan.is_ok());
        let Some(mut plan) = plan.ok() else {
            return;
        };
        assert!(
            planner
                .observe(&mut plan, CancellationToken::new())
                .await
                .is_ok()
        );
        let scheduler = Scheduler::new(
            Arc::new(handlers),
            Arc::new(PassingReadiness),
            SchedulerOptions::default(),
        );
        let sink = Arc::new(InMemoryEventSink::default());
        assert!(
            scheduler
                .execute(&plan, CancellationToken::new(), sink.clone(), false)
                .await
                .is_err()
        );
        let snapshot = sink.snapshot();
        assert!(snapshot.is_ok());
        let Some(snapshot) = snapshot.ok() else {
            return;
        };
        assert!(snapshot.iter().any(|event| matches!(
            event,
            ApplicationEvent::ActionSkipped { action_id, .. } if action_id.as_str() == "dependent"
        )));
    }

    #[tokio::test]
    async fn dry_run_never_calls_apply() {
        let Some(action) = command_action("api") else {
            return;
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut configuration = EnvironmentConfig::new("Blog").ok();
        assert!(configuration.is_some());
        let Some(mut configuration) = configuration.take() else {
            return;
        };
        configuration.actions.push(action);
        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "run_command",
                    observation: ObservationStatus::RequiresChange,
                    delay: Duration::ZERO,
                    calls: Arc::clone(&calls),
                    fail: false,
                })
                .is_ok()
        );
        let integrations = test_integrations();
        let planner = crate::application::planner::Planner::new(&integrations, &handlers);
        let plan = planner.build(&configuration);
        assert!(plan.is_ok());
        let Some(mut plan) = plan.ok() else {
            return;
        };
        assert!(
            planner
                .observe(&mut plan, CancellationToken::new())
                .await
                .is_ok()
        );
        let scheduler = Scheduler::new(
            Arc::new(handlers),
            Arc::new(PassingReadiness),
            SchedulerOptions::default(),
        );
        let result = scheduler
            .execute(
                &plan,
                CancellationToken::new(),
                Arc::new(InMemoryEventSink::default()),
                true,
            )
            .await;
        assert!(result.is_ok());
        assert!(calls.lock().map(|calls| calls.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn readiness_timeout_returns_an_action_specific_error() {
        struct SlowReadiness;
        impl ReadinessCheckRunner for SlowReadiness {
            fn check<'a>(
                &'a self,
                _action_id: &'a ActionId,
                _check: &'a ReadinessCheck,
                _cancellation: CancellationToken,
            ) -> BoxFuture<'a, crate::error::Result<ReadinessCheckResult>> {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Ok(ReadinessCheckResult::passed())
                })
            }
        }
        let Some(mut action) = command_action("api") else {
            return;
        };
        action.readiness_checks = vec![ReadinessCheck::Delay { milliseconds: 1 }];
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        configuration.actions.push(action);
        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "run_command",
                    observation: ObservationStatus::RequiresChange,
                    delay: Duration::ZERO,
                    calls: Arc::new(Mutex::new(Vec::new())),
                    fail: false,
                })
                .is_ok()
        );
        let integrations = test_integrations();
        let planner = crate::application::planner::Planner::new(&integrations, &handlers);
        let plan = planner.build(&configuration);
        assert!(plan.is_ok());
        let Some(mut plan) = plan.ok() else {
            return;
        };
        assert!(
            planner
                .observe(&mut plan, CancellationToken::new())
                .await
                .is_ok()
        );
        let scheduler = Scheduler::new(
            Arc::new(handlers),
            Arc::new(SlowReadiness),
            SchedulerOptions::new(1, Duration::from_secs(1), Duration::from_millis(5))
                .ok()
                .unwrap_or_default(),
        );
        let result = scheduler
            .execute(
                &plan,
                CancellationToken::new(),
                Arc::new(InMemoryEventSink::default()),
                false,
            )
            .await;
        assert!(result.is_err());
        let Some(error) = result.err() else {
            return;
        };
        assert_eq!(
            error.context.get("action_id").map(String::as_str),
            Some("api")
        );
        assert!(error.message.contains("timed out"));
    }
}
