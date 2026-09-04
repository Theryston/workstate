use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::mpsc;

use crate::{
    application::{
        planner::{
            ActionHandlerRegistry, ActionOutputStream, CancellationToken, PlanClassification,
            Planner, ReadinessCheckRunner,
        },
        ports::{BoxFuture, Clock, SystemClock},
    },
    domain::{ActionId, ActionKind, EnvironmentConfig, EnvironmentSlug, ExecutionMode},
    error::{ErrorCategory, Result, WorkstateError},
    integrations::IntegrationRegistry,
};

pub mod scheduler;

pub use scheduler::{ActionRunStatus, RunReport, Scheduler, SchedulerOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEventEntry {
    pub action_id: ActionId,
    pub action_kind: ActionKind,
    pub dependencies: Vec<ActionId>,
    pub execution_mode: Option<ExecutionMode>,
    pub required_capabilities: Vec<crate::platform::CapabilityId>,
    pub classification: PlanClassification,
    pub expected_change: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplicationEvent {
    RunStarted {
        environment: EnvironmentSlug,
        run_id: String,
    },
    PlanBuilt {
        environment: EnvironmentSlug,
        actions: Vec<PlanEventEntry>,
    },
    ActionObserved {
        action_id: ActionId,
        classification: PlanClassification,
        detail: Option<String>,
    },
    ActionStarted {
        action_id: ActionId,
        attempt: u32,
        execution_mode: Option<ExecutionMode>,
    },
    ActionOutput {
        action_id: ActionId,
        stream: ActionOutputStream,
        message: String,
    },
    ActionReady {
        action_id: ActionId,
        already_correct: bool,
    },
    ActionSkipped {
        action_id: ActionId,
        reason: String,
    },
    ActionFailed {
        action_id: ActionId,
        error: String,
    },
    ActionCancelled {
        action_id: ActionId,
        reason: String,
    },
    RollbackStarted,
    RollbackActionStarted {
        action_id: ActionId,
    },
    RollbackActionCompleted {
        action_id: ActionId,
        success: bool,
        detail: Option<String>,
    },
    RunCompleted {
        environment: EnvironmentSlug,
        elapsed_milliseconds: u128,
        already_correct: bool,
    },
    RunFailed {
        environment: EnvironmentSlug,
        action_id: Option<ActionId>,
        error: String,
    },
}

pub trait EventSink: Send + Sync {
    fn emit<'a>(&'a self, event: ApplicationEvent) -> BoxFuture<'a, Result<()>>;
}

#[derive(Clone)]
pub struct ChannelEventSink {
    sender: mpsc::Sender<ApplicationEvent>,
}

impl ChannelEventSink {
    pub fn bounded(capacity: usize) -> Result<(Self, mpsc::Receiver<ApplicationEvent>)> {
        if capacity == 0 {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "application event channel capacity must be greater than zero",
            ));
        }
        let (sender, receiver) = mpsc::channel(capacity);
        Ok((Self { sender }, receiver))
    }
}

impl EventSink for ChannelEventSink {
    fn emit<'a>(&'a self, event: ApplicationEvent) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.sender.send(event).await.map_err(|_| {
                WorkstateError::new(
                    ErrorCategory::Runtime,
                    "application event consumer closed before the run completed",
                )
            })
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryEventSink {
    events: Arc<Mutex<Vec<ApplicationEvent>>>,
}

impl InMemoryEventSink {
    pub fn snapshot(&self) -> Result<Vec<ApplicationEvent>> {
        self.events
            .lock()
            .map(|events| events.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "event buffer lock failed"))
    }
}

impl EventSink for InMemoryEventSink {
    fn emit<'a>(&'a self, event: ApplicationEvent) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.events
                .lock()
                .map(|mut events| events.push(event))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "event buffer lock failed")
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: String,
    pub dry_run: bool,
}

impl RunRequest {
    pub fn new(run_id: impl Into<String>, dry_run: bool) -> Result<Self> {
        let run_id = run_id.into();
        if run_id.is_empty() || run_id.contains('\0') {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "run ID must be non-empty and contain no NUL characters",
            ));
        }
        Ok(Self { run_id, dry_run })
    }
}

pub struct ReconciliationEngine<'a> {
    integrations: &'a IntegrationRegistry,
    handlers: Arc<ActionHandlerRegistry>,
    clock: Arc<dyn Clock>,
    scheduler: Scheduler,
    observation_timeout: Duration,
}

impl<'a> ReconciliationEngine<'a> {
    pub fn new(
        integrations: &'a IntegrationRegistry,
        handlers: Arc<ActionHandlerRegistry>,
        readiness_runner: Arc<dyn ReadinessCheckRunner>,
        options: SchedulerOptions,
    ) -> Self {
        Self::with_clock(
            integrations,
            handlers,
            readiness_runner,
            Arc::new(SystemClock),
            options,
        )
    }

    pub fn with_clock(
        integrations: &'a IntegrationRegistry,
        handlers: Arc<ActionHandlerRegistry>,
        readiness_runner: Arc<dyn ReadinessCheckRunner>,
        clock: Arc<dyn Clock>,
        options: SchedulerOptions,
    ) -> Self {
        let observation_timeout = options.default_action_timeout;
        let scheduler = Scheduler::with_clock(
            Arc::clone(&handlers),
            readiness_runner,
            Arc::clone(&clock),
            options,
        );
        Self {
            integrations,
            handlers,
            clock,
            scheduler,
            observation_timeout,
        }
    }

    pub async fn run(
        &self,
        configuration: &EnvironmentConfig,
        request: RunRequest,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> Result<RunReport> {
        let started_at = self.clock.monotonic_now();
        let environment = configuration.slug.clone();
        events
            .emit(ApplicationEvent::RunStarted {
                environment: environment.clone(),
                run_id: request.run_id.clone(),
            })
            .await?;

        let planner = Planner::new(self.integrations, self.handlers.as_ref());
        let mut plan = match planner.build(configuration) {
            Ok(plan) => plan,
            Err(error) => {
                self.emit_failure(&events, &environment, &error).await?;
                return Err(error);
            }
        };

        if let Err(error) = planner
            .observe_with_timeout(&mut plan, cancellation.clone(), self.observation_timeout)
            .await
        {
            self.emit_failure(&events, &environment, &error).await?;
            return Err(error);
        }

        let plan_actions = plan
            .entries()
            .map(|entry| PlanEventEntry {
                action_id: entry.action_id.clone(),
                action_kind: entry.action_kind.clone(),
                dependencies: entry.dependencies.clone(),
                execution_mode: entry.execution_mode,
                required_capabilities: entry.required_capabilities.iter().copied().collect(),
                classification: entry.classification,
                expected_change: entry.requires_change(),
                detail: entry.classification_detail.clone(),
            })
            .collect();
        events
            .emit(ApplicationEvent::PlanBuilt {
                environment: environment.clone(),
                actions: plan_actions,
            })
            .await?;

        for entry in plan.entries() {
            events
                .emit(ApplicationEvent::ActionObserved {
                    action_id: entry.action_id.clone(),
                    classification: entry.classification,
                    detail: entry.classification_detail.clone(),
                })
                .await?;
        }

        let result = self
            .scheduler
            .execute(&plan, cancellation, events.clone(), request.dry_run)
            .await;
        match result {
            Ok(report) => {
                events
                    .emit(ApplicationEvent::RunCompleted {
                        environment,
                        elapsed_milliseconds: self.clock.elapsed_since(started_at).as_millis(),
                        already_correct: report.planned_change_count == 0,
                    })
                    .await?;
                Ok(report)
            }
            Err(error) => {
                self.emit_failure(&events, &configuration.slug, &error)
                    .await?;
                Err(error)
            }
        }
    }

    async fn emit_failure(
        &self,
        events: &Arc<dyn EventSink>,
        environment: &EnvironmentSlug,
        error: &WorkstateError,
    ) -> Result<()> {
        let action_id = error
            .context
            .get("action_id")
            .and_then(|value| ActionId::new(value.clone()).ok());
        events
            .emit(ApplicationEvent::RunFailed {
                environment: environment.clone(),
                action_id,
                error: error.to_string(),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use crate::{
        application::{
            planner::{
                ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
                CancellationToken, ReadinessCheckResult, ReadinessCheckRunner,
            },
            ports::BoxFuture,
            reconciliation::{
                ApplicationEvent, InMemoryEventSink, ReconciliationEngine, RunRequest,
                SchedulerOptions,
            },
        },
        domain::{
            ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, ExecutionMode, ReadinessCheck,
        },
        integrations::IntegrationRegistry,
    };

    struct Handler;

    impl ActionHandler for Handler {
        fn action_key(&self) -> &str {
            "run_command"
        }

        fn observe<'a>(
            &'a self,
            _action: &'a ActionSpec,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ActionObservation>> {
            Box::pin(async { Ok(ActionObservation::requires_change()) })
        }

        fn apply<'a>(
            &'a self,
            _action: &'a ActionSpec,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ActionExecutionResult>> {
            Box::pin(async { Ok(ActionExecutionResult::default()) })
        }
    }

    struct PassingReadiness;

    impl ReadinessCheckRunner for PassingReadiness {
        fn check<'a>(
            &'a self,
            _action_id: &'a crate::domain::ActionId,
            _check: &'a ReadinessCheck,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'a, crate::error::Result<ReadinessCheckResult>> {
            Box::pin(async { Ok(ReadinessCheckResult::passed()) })
        }
    }

    #[tokio::test]
    async fn engine_emits_run_lifecycle_events() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(mut action) = ActionSpec::new("api", ActionKind::RunCommand).ok() else {
            return;
        };
        action.parameters.command = Some(CommandSpec::new("test-command"));
        action.execution_mode = Some(ExecutionMode::RunOnce);
        configuration.actions.push(action);
        let mut handlers = ActionHandlerRegistry::new();
        assert!(handlers.register(Handler).is_ok());
        let mut integrations = IntegrationRegistry::new();
        assert!(
            integrations
                .set_capability_availability(
                    crate::platform::CapabilityId::BackgroundProcesses,
                    true,
                    None,
                )
                .is_ok()
        );
        let engine = ReconciliationEngine::new(
            &integrations,
            Arc::new(handlers),
            Arc::new(PassingReadiness),
            SchedulerOptions::new(2, Duration::from_secs(1), Duration::from_secs(1))
                .ok()
                .unwrap_or_default(),
        );
        let events = Arc::new(InMemoryEventSink::default());
        let request = RunRequest::new("run-1", false);
        assert!(request.is_ok());
        let Some(request) = request.ok() else {
            return;
        };
        assert!(
            engine
                .run(
                    &configuration,
                    request,
                    CancellationToken::new(),
                    events.clone()
                )
                .await
                .is_ok()
        );
        let snapshot = events.snapshot();
        assert!(snapshot.is_ok());
        let Some(snapshot) = snapshot.ok() else {
            return;
        };
        assert!(matches!(
            snapshot.first(),
            Some(ApplicationEvent::RunStarted { .. })
        ));
        assert!(matches!(
            snapshot.last(),
            Some(ApplicationEvent::RunCompleted { .. })
        ));
    }
}
