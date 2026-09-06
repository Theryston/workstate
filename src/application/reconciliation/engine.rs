use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandlerRegistry, ActionObservation, CancellationToken,
            ExecutionPlan, ObservationStatus, ReadinessCheckRunner, enrich_workspace_context,
            run_with_timeout,
        },
        ports::{Clock, ConfigStore, DesktopBackend, StateStore},
        reconciliation::{
            ApplicationEvent, EventSink, ExecutionObserver, ReconciliationEngine, RunReport,
            RunRequest, SchedulerOptions,
        },
    },
    domain::{
        ActionGraph, ActionId, ActionSpec, CleanupPolicy, CleanupStatus, EnvironmentConfig,
        EnvironmentSlug, ResourceIdentity, ResourceRecord, RunStatus, RuntimeState,
    },
    error::{ErrorCategory, Result, WorkstateError},
    integrations::IntegrationRegistry,
};

use super::{
    ownership::{CleanupDecision, OwnershipRegistry},
    rollback::{RollbackEngine, RollbackFailure, RollbackReport},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleRunResult {
    pub report: RunReport,
    pub state: RuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopResult {
    pub environment: EnvironmentSlug,
    pub cleaned_resources: usize,
    pub preserved_resources: usize,
    pub stale_resources: usize,
    pub state_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteResult {
    pub environment: EnvironmentSlug,
    pub stopped: bool,
    pub removed: bool,
}

pub struct LifecycleEngine<'a> {
    core: ReconciliationEngine<'a>,
    integrations: &'a IntegrationRegistry,
    handlers: Arc<ActionHandlerRegistry>,
    readiness_runner: Arc<dyn ReadinessCheckRunner>,
    clock: Arc<dyn Clock>,
    config_store: Arc<dyn ConfigStore>,
    state_store: Arc<dyn StateStore>,
    options: SchedulerOptions,
}

impl<'a> LifecycleEngine<'a> {
    pub fn new(
        integrations: &'a IntegrationRegistry,
        handlers: Arc<ActionHandlerRegistry>,
        readiness_runner: Arc<dyn ReadinessCheckRunner>,
        clock: Arc<dyn Clock>,
        config_store: Arc<dyn ConfigStore>,
        state_store: Arc<dyn StateStore>,
        options: SchedulerOptions,
    ) -> Self {
        let core = ReconciliationEngine::with_clock(
            integrations,
            Arc::clone(&handlers),
            Arc::clone(&readiness_runner),
            Arc::clone(&clock),
            options.clone(),
        );
        Self {
            core,
            integrations,
            handlers,
            readiness_runner,
            clock,
            config_store,
            state_store,
            options,
        }
    }

    pub fn with_desktop_backend(mut self, desktop_backend: Arc<dyn DesktopBackend>) -> Self {
        self.core = ReconciliationEngine::with_clock_and_desktop(
            self.integrations,
            Arc::clone(&self.handlers),
            Arc::clone(&self.readiness_runner),
            desktop_backend,
            Arc::clone(&self.clock),
            self.options.clone(),
        );
        self
    }

    pub async fn run(
        &self,
        configuration: &EnvironmentConfig,
        request: RunRequest,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> Result<LifecycleRunResult> {
        configuration.validate().map_err(WorkstateError::from)?;
        if request.dry_run {
            let report = self
                .core
                .run(configuration, request, cancellation, events)
                .await?;
            return Ok(LifecycleRunResult {
                report,
                state: RuntimeState::new(configuration.slug.clone(), "dry-run"),
            });
        }

        let existing = self.state_store.load(&configuration.slug)?;
        let plan = self
            .core
            .prepare_with_runtime_state(
                configuration,
                &request,
                cancellation.clone(),
                events.clone(),
                existing.as_ref(),
            )
            .await?;
        let ownership = OwnershipRegistry::load(
            &configuration.slug,
            self.config_store.as_ref(),
            self.state_store.as_ref(),
        )?;
        let state = initial_run_state(configuration, &request, existing, self.clock.as_ref())?;
        let journal = Arc::new(RuntimeJournal::new(
            state,
            ownership,
            Arc::clone(&self.state_store),
            Arc::clone(&self.clock),
        )?);
        journal.set_action_policies(&plan)?;
        journal.persist()?;
        journal.record_observations(&plan)?;
        journal.activate()?;

        let observer: Arc<dyn ExecutionObserver> = journal.clone();
        let result = self
            .core
            .execute_plan(&plan, request, cancellation, events.clone(), Some(observer))
            .await;

        match result {
            Ok(report) => match journal.mark_ready() {
                Ok(()) => Ok(LifecycleRunResult {
                    report,
                    state: journal.snapshot()?,
                }),
                Err(error) => {
                    let final_error = self
                        .rollback_after_failure(&plan, &journal, events, error)
                        .await?;
                    Err(final_error)
                }
            },
            Err(error) => {
                let final_error = self
                    .rollback_after_failure(&plan, &journal, events, error)
                    .await?;
                Err(final_error)
            }
        }
    }

    pub async fn stop(
        &self,
        configuration: &EnvironmentConfig,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> Result<StopResult> {
        configuration.validate().map_err(WorkstateError::from)?;
        let environment = configuration.slug.clone();
        events
            .emit(ApplicationEvent::StopStarted {
                environment: environment.clone(),
            })
            .await?;

        let Some(existing) = self.state_store.load(&environment)? else {
            events
                .emit(ApplicationEvent::StopCompleted {
                    environment: environment.clone(),
                    cleaned_resources: 0,
                    preserved_resources: 0,
                })
                .await?;
            return Ok(StopResult {
                environment,
                cleaned_resources: 0,
                preserved_resources: 0,
                stale_resources: 0,
                state_removed: false,
            });
        };
        let ownership = OwnershipRegistry::load(
            &environment,
            self.config_store.as_ref(),
            self.state_store.as_ref(),
        )?;
        let journal = RuntimeJournal::for_cleanup(
            existing,
            ownership,
            Arc::clone(&self.state_store),
            Arc::clone(&self.clock),
        )?;
        journal.begin_stopping()?;

        let workspace_ids = configuration
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<BTreeSet<_>>();
        let graph = ActionGraph::validate(&configuration.actions, &workspace_ids)
            .map_err(WorkstateError::from)?;
        let actions = configuration
            .actions
            .iter()
            .map(|source_action| {
                let mut action = source_action.clone();
                enrich_workspace_context(&mut action, configuration);
                (action.id.clone(), action)
            })
            .collect::<BTreeMap<_, _>>();
        let mut resources_by_action = BTreeMap::<ActionId, Vec<ResourceRecord>>::new();
        let mut mutations_by_action = BTreeMap::<ActionId, usize>::new();
        let mut unattributed = Vec::new();
        let mut unattributed_mutations = Vec::new();
        let snapshot = journal.snapshot()?;
        for record in snapshot.resources {
            if let Some(action_id) = record.action_id.clone() {
                resources_by_action
                    .entry(action_id)
                    .or_default()
                    .push(record);
            } else {
                unattributed.push(record);
            }
        }
        for mutation in snapshot.mutations {
            if !mutation.restored {
                if let Some(action_id) = mutation.action_id {
                    *mutations_by_action.entry(action_id).or_default() += 1;
                } else {
                    unattributed_mutations.push(mutation.target);
                }
            }
        }

        let mut summary = CleanupSummary::default();
        for target in unattributed_mutations {
            summary.failure(format!(
                "mutation '{target}' has no owning action and was preserved"
            ));
        }
        for record in unattributed {
            summary.preserved_resources += 1;
            summary.failure(format!(
                "resource '{}' has no owning action and was preserved",
                record.resource
            ));
            events
                .emit(ApplicationEvent::ResourceCleanupSkipped {
                    resource: record.resource.to_string(),
                    reason: "the resource has no stable owning action".to_owned(),
                })
                .await?;
        }

        for action_id in graph.ordered_action_ids().iter().rev() {
            let resources = resources_by_action.remove(action_id).unwrap_or_default();
            let Some(action) = actions.get(action_id) else {
                summary.failure(format!("action '{action_id}' was not found while stopping"));
                continue;
            };
            events
                .emit(ApplicationEvent::ActionStarted {
                    action_id: action_id.clone(),
                    attempt: 1,
                    execution_mode: action.execution_mode,
                })
                .await?;
            let Some(handler) = self.handlers.handler_for(&action.kind) else {
                if !resources.is_empty() || journal.has_pending_mutations(action_id)? {
                    let error = format!(
                        "no handler is registered for action '{action_id}', so owned resources were preserved"
                    );
                    summary.failure(error.clone());
                    events
                        .emit(ApplicationEvent::ActionFailed {
                            action_id: action_id.clone(),
                            error,
                        })
                        .await?;
                } else {
                    events
                        .emit(ApplicationEvent::ActionReady {
                            action_id: action_id.clone(),
                            already_correct: true,
                        })
                        .await?;
                }
                mutations_by_action.remove(action_id);
                continue;
            };
            mutations_by_action.remove(action_id);

            let current_resources = self
                .observe_cleanup_resources(
                    action,
                    handler.clone(),
                    &resources,
                    cancellation.clone(),
                )
                .await;
            let observed = match current_resources {
                Ok(observation) => observation,
                Err(error) => {
                    let error_message =
                        format!("action '{action_id}' could not be re-observed: {error}");
                    summary.failure(error_message.clone());
                    events
                        .emit(ApplicationEvent::ActionFailed {
                            action_id: action_id.clone(),
                            error: error_message,
                        })
                        .await?;
                    continue;
                }
            };
            if observed.status == ObservationStatus::Unknown {
                let error_message = format!(
                    "action '{action_id}' returned an ambiguous observation; its resources were preserved"
                );
                summary.failure(error_message.clone());
                events
                    .emit(ApplicationEvent::ActionFailed {
                        action_id: action_id.clone(),
                        error: error_message,
                    })
                    .await?;
                continue;
            }

            let observed_ids = observed
                .resources
                .iter()
                .map(|record| record.resource.clone())
                .collect::<BTreeSet<_>>();
            let mut stoppable = Vec::new();
            let mut action_error = None;
            for resource in resources {
                if !observed_ids.contains(&resource.resource) {
                    journal.mark_resource_cleaned(&resource.resource)?;
                    summary.stale_resources += 1;
                    summary.cleaned_resources += 1;
                    continue;
                }
                let current = journal
                    .snapshot()?
                    .resource(&resource.resource)
                    .cloned()
                    .unwrap_or(resource.clone());
                let decision = journal.ownership().cleanup_decision(&current);
                if decision.is_cleanup_allowed() {
                    stoppable.push(current);
                    continue;
                }

                summary.preserved_resources += 1;
                events
                    .emit(ApplicationEvent::ResourceCleanupSkipped {
                        resource: current.resource.to_string(),
                        reason: cleanup_reason(decision).to_owned(),
                    })
                    .await?;
                if decision.is_safe_preservation() {
                    journal.release_resource(&current.resource)?;
                } else {
                    let error_message = format!(
                        "resource '{}' ownership was ambiguous and it was preserved",
                        current.resource
                    );
                    summary.failure(error_message.clone());
                    if action_error.is_none() {
                        action_error = Some(error_message);
                    }
                }
            }

            if !stoppable.is_empty() {
                let result = run_with_timeout(
                    handler.stop(action, &stoppable, cancellation.clone()),
                    action
                        .timeout
                        .as_ref()
                        .map(|timeout| Duration::from_millis(timeout.milliseconds))
                        .unwrap_or(self.options.default_action_timeout),
                    cancellation.clone(),
                    Some(action_id),
                    "stop",
                )
                .await;
                match result {
                    Ok(compensation) => {
                        for output in compensation.outputs {
                            events
                                .emit(ApplicationEvent::ActionOutput {
                                    action_id: action_id.clone(),
                                    stream: output.stream,
                                    message: output.message,
                                })
                                .await?;
                        }
                        for resource in &stoppable {
                            journal.mark_resource_cleaned(&resource.resource)?;
                            summary.cleaned_resources += 1;
                        }
                    }
                    Err(error) => {
                        let error_message =
                            format!("action '{action_id}' resource cleanup failed: {error}");
                        summary.failure(error_message.clone());
                        if action_error.is_none() {
                            action_error = Some(error_message);
                        }
                    }
                }
            }

            let mutations = journal.cleanup_result_for_action(action_id)?;
            if !mutations.mutations.is_empty() {
                let result = run_with_timeout(
                    handler.compensate(action, &mutations, cancellation.clone()),
                    action
                        .timeout
                        .as_ref()
                        .map(|timeout| Duration::from_millis(timeout.milliseconds))
                        .unwrap_or(self.options.default_action_timeout),
                    cancellation.clone(),
                    Some(action_id),
                    "configuration restoration",
                )
                .await;
                match result {
                    Ok(compensation) => {
                        for output in compensation.outputs {
                            events
                                .emit(ApplicationEvent::ActionOutput {
                                    action_id: action_id.clone(),
                                    stream: output.stream,
                                    message: output.message,
                                })
                                .await?;
                        }
                        journal.mark_compensated(action_id, &mutations)?;
                    }
                    Err(error) => {
                        let error_message = format!(
                            "action '{action_id}' configuration restoration failed: {error}"
                        );
                        summary.failure(error_message.clone());
                        if action_error.is_none() {
                            action_error = Some(error_message);
                        }
                    }
                }
            }
            if let Some(error) = action_error {
                events
                    .emit(ApplicationEvent::ActionFailed {
                        action_id: action_id.clone(),
                        error,
                    })
                    .await?;
            } else {
                events
                    .emit(ApplicationEvent::ActionReady {
                        action_id: action_id.clone(),
                        already_correct: false,
                    })
                    .await?;
            }
        }

        if !resources_by_action.is_empty() {
            for (action_id, resources) in resources_by_action {
                if !resources.is_empty() {
                    summary.failure(format!(
                        "action '{action_id}' was not in the validated graph; resources were preserved"
                    ));
                }
            }
        }
        if !mutations_by_action.is_empty() {
            for (action_id, count) in mutations_by_action {
                if count > 0 {
                    summary.failure(format!(
                        "action '{action_id}' was not in the validated graph; {count} mutation(s) were preserved"
                    ));
                }
            }
        }

        if !summary.failures.is_empty() {
            journal.finish_stopping_failure(&summary.failures)?;
            let error = cleanup_error(&environment, &summary.failures);
            events
                .emit(ApplicationEvent::StopFailed {
                    environment,
                    error: error.to_string(),
                })
                .await?;
            return Err(error);
        }

        journal.finish_stopping_success()?;
        self.state_store.delete(&journal.environment()?)?;
        events
            .emit(ApplicationEvent::StopCompleted {
                environment: environment.clone(),
                cleaned_resources: summary.cleaned_resources,
                preserved_resources: summary.preserved_resources,
            })
            .await?;
        Ok(StopResult {
            environment,
            cleaned_resources: summary.cleaned_resources,
            preserved_resources: summary.preserved_resources,
            stale_resources: summary.stale_resources,
            state_removed: true,
        })
    }

    pub async fn delete(
        &self,
        configuration: &EnvironmentConfig,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> Result<DeleteResult> {
        configuration.validate().map_err(WorkstateError::from)?;
        let environment = configuration.slug.clone();
        events
            .emit(ApplicationEvent::DeleteStarted {
                environment: environment.clone(),
            })
            .await?;
        let existing = self.state_store.load(&environment)?;
        let needs_cleanup = existing.as_ref().is_some_and(|state| {
            state.status.is_active()
                || !state.resources.is_empty()
                || state.mutations.iter().any(|mutation| !mutation.restored)
        });
        if needs_cleanup {
            let stopped_state = existing.ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Persistence,
                    "runtime state disappeared before delete cleanup started",
                )
            })?;
            let ownership = OwnershipRegistry::load(
                &environment,
                self.config_store.as_ref(),
                self.state_store.as_ref(),
            )?;
            let journal = RuntimeJournal::for_cleanup(
                stopped_state,
                ownership,
                Arc::clone(&self.state_store),
                Arc::clone(&self.clock),
            )?;
            journal.begin_deleting()?;
        }
        let stopped = if needs_cleanup {
            self.stop(configuration, cancellation, events.clone())
                .await?;
            true
        } else {
            false
        };
        self.config_store.delete(&environment)?;
        events
            .emit(ApplicationEvent::DeleteCompleted {
                environment: environment.clone(),
            })
            .await?;
        Ok(DeleteResult {
            environment,
            stopped,
            removed: true,
        })
    }

    async fn observe_cleanup_resources(
        &self,
        action: &ActionSpec,
        handler: Arc<dyn crate::application::planner::ActionHandler>,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        run_with_timeout(
            handler.observe_for_cleanup(action, resources, cancellation.clone()),
            action
                .timeout
                .as_ref()
                .map(|timeout| Duration::from_millis(timeout.milliseconds))
                .unwrap_or(self.options.default_action_timeout),
            cancellation,
            Some(&action.id),
            "cleanup observation",
        )
        .await
    }

    async fn rollback_after_failure(
        &self,
        plan: &ExecutionPlan,
        journal: &Arc<RuntimeJournal>,
        events: Arc<dyn EventSink>,
        primary: WorkstateError,
    ) -> Result<WorkstateError> {
        let rollback = RollbackEngine::new(
            Arc::clone(&self.handlers),
            self.options.default_action_timeout,
        )?;
        let rollback_result = rollback.execute(plan, journal.as_ref(), events).await;
        let mut error = primary;
        match rollback_result {
            Ok(report) if report.succeeded() => {
                error = error.with_context("rollback", report.summary());
            }
            Ok(report) => {
                error = append_rollback_failure(error, &report);
            }
            Err(rollback_error) => {
                error = error.with_context("rollback_error", rollback_error.to_string());
            }
        }
        Ok(error)
    }
}

pub struct RuntimeJournal {
    state: Mutex<RuntimeState>,
    ownership: OwnershipRegistry,
    action_policies: Mutex<BTreeMap<ActionId, CleanupPolicy>>,
    completed_results: Mutex<BTreeMap<ActionId, ActionExecutionResult>>,
    cleanup_failures: Mutex<Vec<String>>,
    state_store: Arc<dyn StateStore>,
    clock: Arc<dyn Clock>,
}

impl RuntimeJournal {
    pub fn new(
        state: RuntimeState,
        ownership: OwnershipRegistry,
        state_store: Arc<dyn StateStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        state.validate().map_err(WorkstateError::from)?;
        Ok(Self {
            state: Mutex::new(state),
            ownership,
            action_policies: Mutex::new(BTreeMap::new()),
            completed_results: Mutex::new(BTreeMap::new()),
            cleanup_failures: Mutex::new(Vec::new()),
            state_store,
            clock,
        })
    }

    pub fn for_cleanup(
        state: RuntimeState,
        ownership: OwnershipRegistry,
        state_store: Arc<dyn StateStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::new(state, ownership, state_store, clock)
    }

    pub fn snapshot(&self) -> Result<RuntimeState> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| journal_lock_error("runtime state"))
    }

    pub fn environment(&self) -> Result<EnvironmentSlug> {
        self.state
            .lock()
            .map(|state| state.environment_slug.clone())
            .map_err(|_| journal_lock_error("runtime state"))
    }

    pub fn ownership(&self) -> &OwnershipRegistry {
        &self.ownership
    }

    pub fn set_action_policies(&self, plan: &ExecutionPlan) -> Result<()> {
        let mut policies = self
            .action_policies
            .lock()
            .map_err(|_| journal_lock_error("action cleanup policies"))?;
        policies.clear();
        policies.extend(
            plan.entries()
                .map(|entry| (entry.action_id.clone(), entry.cleanup_policy)),
        );
        Ok(())
    }

    pub fn persist(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| journal_lock_error("runtime state"))?;
        let previous = state.clone();
        let mut next = previous.clone();
        next.set_updated_at(current_timestamp(self.clock.as_ref()));
        next.validate().map_err(WorkstateError::from)?;
        self.state_store.save_if_changed(&next, Some(&previous))?;
        *state = next;
        Ok(())
    }

    pub fn record_observations(&self, plan: &ExecutionPlan) -> Result<()> {
        for entry in plan.entries() {
            if entry.observed_resources.is_empty() {
                continue;
            }
            let state = self.snapshot()?;
            let records = entry
                .observed_resources
                .iter()
                .cloned()
                .map(|record| {
                    let record = apply_cleanup_policy(record, entry.cleanup_policy);
                    self.ownership
                        .classify(record.with_action(entry.action_id.clone()), &state)
                })
                .collect::<Vec<_>>();
            self.mutate(|state| {
                for record in records {
                    state
                        .upsert_resource(record)
                        .map_err(WorkstateError::from)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn activate(&self) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::Active)
                .map_err(WorkstateError::from)
        })
    }

    pub fn begin_rollback(&self) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::RollingBack)
                .map_err(WorkstateError::from)?;
            state.set_cleanup_status(CleanupStatus::InProgress);
            Ok(())
        })
    }

    pub fn action_started(&self, action_id: &ActionId) -> Result<()> {
        self.mutate(|state| {
            state.add_active_task(action_id.clone());
            if state.status == RunStatus::Planning {
                state
                    .transition_to(RunStatus::Active)
                    .map_err(WorkstateError::from)?;
            }
            Ok(())
        })
    }

    pub fn action_succeeded(
        &self,
        action_id: &ActionId,
        result: &ActionExecutionResult,
    ) -> Result<()> {
        let state = self.snapshot()?;
        let cleanup_policy = self
            .action_policies
            .lock()
            .map_err(|_| journal_lock_error("action cleanup policies"))?
            .get(action_id)
            .copied()
            .unwrap_or_default();
        let resources = result
            .resources
            .iter()
            .cloned()
            .map(|record| {
                let record = apply_cleanup_policy(record, cleanup_policy);
                self.ownership
                    .classify(record.with_action(action_id.clone()), &state)
            })
            .collect::<Vec<_>>();
        let mutations = result
            .mutations
            .iter()
            .cloned()
            .map(|mutating| {
                let mut mutating = mutating;
                if cleanup_policy == CleanupPolicy::Preserve {
                    mutating.cleanup_policy = CleanupPolicy::Preserve;
                }
                if mutating.action_id.is_none() {
                    mutating.action_id = Some(action_id.clone());
                }
                self.ownership.classify_mutation(mutating, &state)
            })
            .collect::<Vec<_>>();
        let journal_result = ActionExecutionResult {
            changed: result.changed,
            resources: resources.clone(),
            mutations: mutations.clone(),
            outputs: result.outputs.clone(),
        };
        self.completed_results
            .lock()
            .map_err(|_| journal_lock_error("completed action results"))?
            .insert(action_id.clone(), journal_result);
        self.mutate(|state| {
            for record in resources {
                state
                    .upsert_resource(record)
                    .map_err(WorkstateError::from)?;
            }
            for mutation in mutations {
                state
                    .upsert_mutation(mutation)
                    .map_err(WorkstateError::from)?;
            }
            state.remove_active_task(action_id);
            if state.status == RunStatus::Planning {
                state
                    .transition_to(RunStatus::Active)
                    .map_err(WorkstateError::from)?;
            }
            Ok(())
        })
    }

    pub fn mark_ready(&self) -> Result<()> {
        self.mutate(|state| {
            if state.status == RunStatus::Planning {
                state
                    .transition_to(RunStatus::Active)
                    .map_err(WorkstateError::from)?;
            }
            state
                .transition_to(RunStatus::Ready)
                .map_err(WorkstateError::from)?;
            state.active_tasks.clear();
            state.set_cleanup_status(CleanupStatus::NotRequired);
            Ok(())
        })
    }

    pub fn completed_results(&self) -> Result<BTreeMap<ActionId, ActionExecutionResult>> {
        self.completed_results
            .lock()
            .map(|results| results.clone())
            .map_err(|_| journal_lock_error("completed action results"))
    }

    pub fn compensating_result(
        &self,
        result: &ActionExecutionResult,
    ) -> Result<ActionExecutionResult> {
        self.filter_result(result)
    }

    pub fn cleanup_result_for_action(&self, action_id: &ActionId) -> Result<ActionExecutionResult> {
        let state = self.snapshot()?;
        let result = ActionExecutionResult {
            changed: true,
            resources: state
                .resources
                .iter()
                .filter(|record| record.action_id.as_ref() == Some(action_id))
                .cloned()
                .collect(),
            mutations: state
                .mutations
                .iter()
                .filter(|mutation| mutation.action_id.as_ref() == Some(action_id))
                .cloned()
                .collect(),
            outputs: Vec::new(),
        };
        self.filter_result(&result)
    }

    pub fn mark_compensated(
        &self,
        action_id: &ActionId,
        result: &ActionExecutionResult,
    ) -> Result<()> {
        let resource_ids = result
            .resources
            .iter()
            .map(|record| record.resource.clone())
            .collect::<BTreeSet<_>>();
        let mutation_targets = result
            .mutations
            .iter()
            .map(|mutation| mutation.target.clone())
            .collect::<BTreeSet<_>>();
        self.mutate(|state| {
            state
                .resources
                .retain(|record| !resource_ids.contains(&record.resource));
            for mutation in &mut state.mutations {
                if mutation_targets.contains(&mutation.target) {
                    mutation.mark_restored();
                }
            }
            state.remove_active_task(action_id);
            Ok(())
        })
    }

    pub fn record_compensation_failure(&self, failure: &RollbackFailure) -> Result<()> {
        let message = failure
            .action_id
            .as_ref()
            .map(|action_id| format!("{action_id}: {}", failure.message))
            .unwrap_or_else(|| failure.message.clone());
        self.cleanup_failures
            .lock()
            .map_err(|_| journal_lock_error("cleanup failures"))?
            .push(message.clone());
        self.mutate(|state| {
            for mutation in &mut state.mutations {
                if failure.action_id.as_ref() == mutation.action_id.as_ref() {
                    mutation.mark_restore_failed();
                }
            }
            state.set_cleanup_status(CleanupStatus::Failed {
                errors: vec![message.clone()],
            });
            Ok(())
        })
    }

    pub fn finish_rollback(&self, report: &RollbackReport) -> Result<()> {
        self.mutate(|state| {
            state.active_tasks.clear();
            if report.succeeded() {
                state
                    .transition_to(RunStatus::Stopped)
                    .map_err(WorkstateError::from)?;
                state.set_cleanup_status(CleanupStatus::Complete);
            } else {
                state
                    .transition_to(RunStatus::RollbackFailed)
                    .map_err(WorkstateError::from)?;
                state.set_cleanup_status(CleanupStatus::Failed {
                    errors: report
                        .failures
                        .iter()
                        .map(|failure| failure.message.clone())
                        .collect(),
                });
            }
            Ok(())
        })
    }

    pub fn begin_stopping(&self) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::Stopping)
                .map_err(WorkstateError::from)?;
            state.set_cleanup_status(CleanupStatus::InProgress);
            Ok(())
        })
    }

    pub fn begin_deleting(&self) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::Deleting)
                .map_err(WorkstateError::from)
        })
    }

    pub fn mark_resource_cleaned(&self, identity: &ResourceIdentity) -> Result<()> {
        self.mutate(|state| {
            state
                .resources
                .retain(|record| &record.resource != identity);
            Ok(())
        })
    }

    pub fn release_resource(&self, identity: &ResourceIdentity) -> Result<()> {
        self.mark_resource_cleaned(identity)
    }

    pub fn has_pending_mutations(&self, action_id: &ActionId) -> Result<bool> {
        Ok(self
            .snapshot()?
            .mutations
            .iter()
            .any(|mutation| mutation.action_id.as_ref() == Some(action_id) && !mutation.restored))
    }

    pub fn finish_stopping_success(&self) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::Stopped)
                .map_err(WorkstateError::from)?;
            state.active_tasks.clear();
            state.set_cleanup_status(CleanupStatus::Complete);
            Ok(())
        })
    }

    pub fn finish_stopping_failure(&self, failures: &[String]) -> Result<()> {
        self.mutate(|state| {
            state
                .transition_to(RunStatus::Partial)
                .map_err(WorkstateError::from)?;
            state.set_cleanup_status(CleanupStatus::Failed {
                errors: failures.to_vec(),
            });
            Ok(())
        })
    }

    fn filter_result(&self, result: &ActionExecutionResult) -> Result<ActionExecutionResult> {
        let state = self.snapshot()?;
        let mut resources = Vec::new();
        for resource in &result.resources {
            let current = state
                .resource(&resource.resource)
                .cloned()
                .unwrap_or_else(|| resource.clone());
            if self
                .ownership
                .cleanup_decision(&current)
                .is_cleanup_allowed()
            {
                resources.push(current);
            }
        }
        let mut mutations = Vec::new();
        for mutation in &result.mutations {
            let current = state
                .mutations
                .iter()
                .find(|candidate| candidate.target == mutation.target)
                .cloned()
                .unwrap_or_else(|| mutation.clone());
            if current.restored
                || matches!(
                    current.restoration_status,
                    crate::domain::RestorationStatus::Restored
                        | crate::domain::RestorationStatus::NotRequired
                )
                || current.cleanup_policy != crate::domain::CleanupPolicy::OwnedOnly
                || current.compensation == crate::domain::CompensationOperation::None
                || !current.ownership.is_environment_owned()
            {
                continue;
            }
            if current.resource.is_none() && !self.ownership.uncertain_environments().is_empty() {
                continue;
            }
            mutations.push(current);
        }
        mutations.reverse();
        Ok(ActionExecutionResult {
            changed: !resources.is_empty() || !mutations.is_empty(),
            resources,
            mutations,
            outputs: Vec::new(),
        })
    }

    fn mutate<F>(&self, update: F) -> Result<()>
    where
        F: FnOnce(&mut RuntimeState) -> Result<()>,
    {
        let mut state = self
            .state
            .lock()
            .map_err(|_| journal_lock_error("runtime state"))?;
        let previous = state.clone();
        let mut next = previous.clone();
        update(&mut next)?;
        next.set_updated_at(current_timestamp(self.clock.as_ref()));
        next.validate().map_err(WorkstateError::from)?;
        self.state_store.save_if_changed(&next, Some(&previous))?;
        *state = next;
        Ok(())
    }
}

impl ExecutionObserver for RuntimeJournal {
    fn action_started<'a>(
        &'a self,
        action_id: &'a ActionId,
    ) -> crate::application::ports::BoxFuture<'a, Result<()>> {
        Box::pin(async move { RuntimeJournal::action_started(self, action_id) })
    }

    fn action_succeeded<'a>(
        &'a self,
        action_id: &'a ActionId,
        result: &'a ActionExecutionResult,
    ) -> crate::application::ports::BoxFuture<'a, Result<()>> {
        Box::pin(async move { RuntimeJournal::action_succeeded(self, action_id, result) })
    }
}

#[derive(Debug, Default)]
struct CleanupSummary {
    cleaned_resources: usize,
    preserved_resources: usize,
    stale_resources: usize,
    failures: Vec<String>,
}

impl CleanupSummary {
    fn failure(&mut self, message: String) {
        self.failures.push(message);
    }
}

fn initial_run_state(
    configuration: &EnvironmentConfig,
    request: &RunRequest,
    existing: Option<RuntimeState>,
    clock: &dyn Clock,
) -> Result<RuntimeState> {
    let mut state = existing
        .unwrap_or_else(|| RuntimeState::new(configuration.slug.clone(), request.run_id.clone()));
    if state.environment_slug != configuration.slug {
        return Err(WorkstateError::new(
            ErrorCategory::Persistence,
            "runtime state belongs to a different environment",
        )
        .with_context("expected_environment", configuration.slug.to_string())
        .with_context("actual_environment", state.environment_slug.to_string()));
    }
    state
        .begin_run(request.run_id.clone())
        .map_err(WorkstateError::from)?;
    state.set_started_at(current_timestamp(clock));
    state.set_updated_at(current_timestamp(clock));
    state.set_cleanup_status(CleanupStatus::Pending);
    Ok(state)
}

fn current_timestamp(clock: &dyn Clock) -> u64 {
    clock
        .now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn apply_cleanup_policy(mut record: ResourceRecord, policy: CleanupPolicy) -> ResourceRecord {
    if policy == CleanupPolicy::Preserve {
        record.cleanup_policy = CleanupPolicy::Preserve;
    }
    record
}

fn cleanup_reason(decision: CleanupDecision) -> &'static str {
    match decision {
        CleanupDecision::Clean => "resource is owned by this environment",
        CleanupDecision::PreserveByPolicy => "cleanup policy preserves this resource",
        CleanupDecision::PreservePreExisting => "resource existed before this environment",
        CleanupDecision::PreserveShared => "resource is shared with another active environment",
        CleanupDecision::PreserveUnknown => "resource ownership could not be determined safely",
    }
}

fn cleanup_error(environment: &EnvironmentSlug, failures: &[String]) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Runtime,
        format!("could not stop environment '{environment}' safely"),
    )
    .with_context("environment", environment.to_string())
    .with_context("cleanup_errors", failures.join("; "))
}

fn append_rollback_failure(mut error: WorkstateError, report: &RollbackReport) -> WorkstateError {
    let failures = report
        .failures
        .iter()
        .map(|failure| {
            failure
                .action_id
                .as_ref()
                .map(|action_id| format!("{action_id}: {}", failure.message))
                .unwrap_or_else(|| failure.message.clone())
        })
        .collect::<Vec<_>>();
    error = error
        .with_context("rollback", "failed")
        .with_context("rollback_errors", failures.join("; "));
    error
}

fn journal_lock_error(resource: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Runtime,
        format!("could not lock {resource} journal"),
    )
}
