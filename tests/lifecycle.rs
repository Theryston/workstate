use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use workstate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            CancellationToken, NoopReadinessCheckRunner,
        },
        ports::{BoxFuture, Clock, ConfigStore, StateStore},
        reconciliation::{
            EventSink, InMemoryEventSink, LifecycleEngine, RunRequest, SchedulerOptions,
        },
    },
    domain::{
        ActionId, ActionKind, ActionSpec, CleanupPolicy, CommandSpec, EnvironmentConfig,
        EnvironmentSlug, ExecutionMode, MutationRecord, OwnershipStatus, ResourceIdentity,
        ResourceKind, ResourceRecord, RunStatus, RuntimeState,
    },
    error::{ErrorCategory, Result, WorkstateError},
    integrations::IntegrationRegistry,
    platform::CapabilityId,
};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[derive(Default)]
struct MemoryConfigStore {
    configurations: Mutex<BTreeMap<EnvironmentSlug, EnvironmentConfig>>,
}

impl MemoryConfigStore {
    fn insert(&self, configuration: EnvironmentConfig) -> Result<()> {
        self.configurations
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "configuration lock failed"))?
            .insert(configuration.slug.clone(), configuration);
        Ok(())
    }

    fn contains(&self, environment: &EnvironmentSlug) -> bool {
        self.configurations
            .lock()
            .map(|configurations| configurations.contains_key(environment))
            .unwrap_or(false)
    }
}

impl ConfigStore for MemoryConfigStore {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<EnvironmentConfig>> {
        self.configurations
            .lock()
            .map(|configurations| configurations.get(environment).cloned())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "configuration lock failed"))
    }

    fn create(&self, configuration: &EnvironmentConfig) -> Result<()> {
        self.insert(configuration.clone())
    }

    fn save(&self, configuration: &EnvironmentConfig) -> Result<()> {
        self.insert(configuration.clone())
    }

    fn delete(&self, environment: &EnvironmentSlug) -> Result<()> {
        self.configurations
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "configuration lock failed"))?
            .remove(environment);
        Ok(())
    }

    fn list(&self) -> Result<Vec<EnvironmentSlug>> {
        self.configurations
            .lock()
            .map(|configurations| configurations.keys().cloned().collect())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "configuration lock failed"))
    }
}

#[derive(Default)]
struct MemoryStateStore {
    states: Mutex<BTreeMap<EnvironmentSlug, RuntimeState>>,
    save_count: AtomicUsize,
    delete_count: AtomicUsize,
}

impl MemoryStateStore {
    fn insert(&self, state: RuntimeState) -> Result<()> {
        self.save(&state)
    }

    fn save_count(&self) -> usize {
        self.save_count.load(Ordering::Acquire)
    }

    fn state(&self, environment: &EnvironmentSlug) -> Result<Option<RuntimeState>> {
        self.load(environment)
    }
}

impl StateStore for MemoryStateStore {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<RuntimeState>> {
        self.states
            .lock()
            .map(|states| states.get(environment).cloned())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))
    }

    fn save(&self, state: &RuntimeState) -> Result<()> {
        state.validate().map_err(WorkstateError::from)?;
        self.save_count.fetch_add(1, Ordering::AcqRel);
        self.states
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))?
            .insert(state.environment_slug.clone(), state.clone());
        Ok(())
    }

    fn delete(&self, environment: &EnvironmentSlug) -> Result<()> {
        self.delete_count.fetch_add(1, Ordering::AcqRel);
        self.states
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))?
            .remove(environment);
        Ok(())
    }
}

#[derive(Default)]
struct HandlerState {
    started: BTreeSet<String>,
    compensation_order: Vec<String>,
    compensation_mutations: Vec<Vec<MutationRecord>>,
    stop_order: Vec<String>,
    fail_action: Option<String>,
    fail_compensation: bool,
    fail_stop: bool,
    observations_already_correct: BTreeSet<String>,
    observed_resources: BTreeMap<String, Vec<ResourceRecord>>,
}

struct RecordingHandler {
    state: Arc<Mutex<HandlerState>>,
    mutation: bool,
}

impl RecordingHandler {
    fn resource(action_id: &ActionId) -> Option<ResourceRecord> {
        let identity =
            ResourceIdentity::new(ResourceKind::Process, format!("resource-{}", action_id)).ok()?;
        Some(ResourceRecord::new(
            identity,
            OwnershipStatus::CreatedByCurrentRun,
        ))
    }

    fn action_name(action: &ActionSpec) -> String {
        action.id.to_string()
    }
}

impl ActionHandler for RecordingHandler {
    fn action_key(&self) -> &str {
        "run_command"
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let action_name = Self::action_name(action);
            let state = state
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "handler lock failed"))?;
            if state.observations_already_correct.contains(&action_name) {
                return Ok(ActionObservation::already_correct());
            }
            if state.started.contains(&action_name) {
                let resources = state
                    .observed_resources
                    .get(&action_name)
                    .cloned()
                    .unwrap_or_else(|| Self::resource(&action.id).into_iter().collect());
                return Ok(ActionObservation::requires_change().with_resources(resources));
            }
            Ok(ActionObservation::requires_change())
        })
    }

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        let state = Arc::clone(&self.state);
        let mutation = self.mutation;
        Box::pin(async move {
            cancellation.check()?;
            let action_name = Self::action_name(action);
            let mut state = state
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "handler lock failed"))?;
            if state.fail_action.as_deref() == Some(action_name.as_str()) {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("action '{action_name}' failed"),
                )
                .with_context("action_id", action_name));
            }
            state.started.insert(action_name);
            let resources = Self::resource(&action.id).into_iter().collect();
            let mutations = if mutation {
                let mut record = MutationRecord::new(format!("workspace:{}", action.id))
                    .map_err(WorkstateError::from)?;
                record.action_id = Some(action.id.clone());
                record.previous_value = Some("disabled".to_owned());
                record.applied_value = Some("enabled".to_owned());
                vec![record]
            } else {
                Vec::new()
            };
            Ok(ActionExecutionResult {
                changed: true,
                resources,
                mutations,
                outputs: Vec::new(),
            })
        })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.run_once(action, cancellation)
    }

    fn compensate<'a>(
        &'a self,
        action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<workstate::application::planner::CompensationResult>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let action_name = Self::action_name(action);
            let mut state = state
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "handler lock failed"))?;
            state.compensation_order.push(action_name.clone());
            state.compensation_mutations.push(result.mutations.clone());
            if state.fail_compensation {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("compensation for '{action_name}' failed"),
                ));
            }
            state.started.remove(&action_name);
            Ok(Default::default())
        })
    }

    fn stop<'a>(
        &'a self,
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<workstate::application::planner::CompensationResult>> {
        let state = Arc::clone(&self.state);
        let resource_count = resources.len();
        Box::pin(async move {
            cancellation.check()?;
            let action_name = Self::action_name(action);
            let mut state = state
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "handler lock failed"))?;
            if resource_count > 0 {
                state.stop_order.push(action_name.clone());
                if state.fail_stop {
                    return Err(WorkstateError::new(
                        ErrorCategory::Runtime,
                        format!("stopping '{action_name}' failed"),
                    ));
                }
                state.started.remove(&action_name);
            }
            Ok(Default::default())
        })
    }
}

fn integrations() -> Result<IntegrationRegistry> {
    let mut registry = IntegrationRegistry::new();
    registry.set_capability_availability(CapabilityId::BackgroundProcesses, true, None)?;
    Ok(registry)
}

fn command_action(id: &str) -> Result<ActionSpec> {
    let mut action = ActionSpec::new(id, ActionKind::RunCommand).map_err(WorkstateError::from)?;
    action.parameters.command = Some(CommandSpec::new("test-command"));
    action.execution_mode = Some(ExecutionMode::RunOnce);
    Ok(action)
}

fn configuration(name: &str, action_ids: &[&str]) -> Result<EnvironmentConfig> {
    let mut configuration = EnvironmentConfig::new(name).map_err(WorkstateError::from)?;
    configuration.actions = action_ids
        .iter()
        .map(|id| command_action(id))
        .collect::<Result<Vec<_>>>()?;
    configuration.validate().map_err(WorkstateError::from)?;
    Ok(configuration)
}

fn lifecycle_engine(
    configurations: &Arc<MemoryConfigStore>,
    states: &Arc<MemoryStateStore>,
    handler_state: &Arc<Mutex<HandlerState>>,
    mutation: bool,
    max_concurrency: usize,
) -> Result<LifecycleEngine<'static>> {
    let integrations = Box::leak(Box::new(integrations()?));
    let mut handlers = ActionHandlerRegistry::new();
    handlers.register(RecordingHandler {
        state: Arc::clone(handler_state),
        mutation,
    })?;
    let readiness = Arc::new(NoopReadinessCheckRunner);
    let clock: Arc<dyn Clock> = Arc::new(workstate::application::ports::SystemClock);
    let options = SchedulerOptions::new(
        max_concurrency,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )?;
    Ok(LifecycleEngine::new(
        integrations,
        Arc::new(handlers),
        readiness,
        clock,
        configurations.clone(),
        states.clone(),
        options,
    ))
}

fn request(id: &str) -> Result<RunRequest> {
    RunRequest::new(id, false)
}

fn resource_for(action_id: &str, ownership: OwnershipStatus) -> Option<ResourceRecord> {
    let id = ActionId::new(action_id.to_owned()).ok()?;
    let identity =
        ResourceIdentity::new(ResourceKind::Process, format!("resource-{action_id}")).ok()?;
    Some(ResourceRecord::new(identity, ownership).with_action(id))
}

#[tokio::test]
async fn successful_run_journals_mutations_and_marks_state_ready() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let configuration = configuration("Blog", &["first", "second"])?;
    configurations.insert(configuration.clone())?;
    let engine = lifecycle_engine(&configurations, &states, &handler_state, true, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let result = engine
        .run(
            &configuration,
            request("run-success")?,
            CancellationToken::new(),
            events,
        )
        .await?;

    assert_eq!(result.state.status, RunStatus::Ready);
    assert_eq!(result.state.resources.len(), 2);
    assert_eq!(result.state.mutations.len(), 2);
    assert!(
        result
            .state
            .mutations
            .iter()
            .all(|mutation| !mutation.restored)
    );
    assert!(states.save_count() >= 5);
    Ok(())
}

#[tokio::test]
async fn action_cleanup_policy_is_persisted_and_preserves_resources() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let mut configuration = configuration("Blog", &["first"])?;
    configuration.actions[0].cleanup_policy = CleanupPolicy::Preserve;
    configurations.insert(configuration.clone())?;
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let run = engine
        .run(
            &configuration,
            request("run-preserve")?,
            CancellationToken::new(),
            events.clone(),
        )
        .await?;
    assert_eq!(
        run.state.resources[0].cleanup_policy,
        CleanupPolicy::Preserve
    );

    let stopped = engine
        .stop(&configuration, CancellationToken::new(), events)
        .await?;
    assert_eq!(stopped.preserved_resources, 1);
    assert!(
        handler_state
            .lock()
            .map_err(|_| std::io::Error::other("handler lock failed"))?
            .stop_order
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn stop_restores_the_exact_recorded_configuration_value() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let configuration = configuration("Blog", &["first"])?;
    configurations.insert(configuration.clone())?;
    let mut state = RuntimeState::new(configuration.slug.clone(), "run-tiling");
    state.set_status(RunStatus::Ready);
    state.record_resource(
        resource_for("first", OwnershipStatus::CreatedByEnvironment)
            .ok_or_else(|| std::io::Error::other("resource creation failed"))?,
    )?;
    let mut mutation = MutationRecord::new("desktop:tiling:first").map_err(WorkstateError::from)?;
    mutation.action_id = Some(ActionId::new("first")?);
    mutation.previous_value = Some("disabled".to_owned());
    mutation.applied_value = Some("enabled".to_owned());
    state.record_mutation(mutation)?;
    states.insert(state)?;
    handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .started
        .insert("first".to_owned());
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events = Arc::new(InMemoryEventSink::default());

    engine
        .stop(&configuration, CancellationToken::new(), events.clone())
        .await?;

    let snapshot = events.snapshot()?;
    assert!(snapshot.iter().any(|event| matches!(
        event,
        workstate::application::reconciliation::ApplicationEvent::ActionStarted {
            action_id, ..
        } if action_id.as_str() == "first"
    )));
    assert!(snapshot.iter().any(|event| matches!(
        event,
        workstate::application::reconciliation::ApplicationEvent::ActionReady { action_id, .. }
            if action_id.as_str() == "first"
    )));

    let state = handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?;
    assert_eq!(state.compensation_mutations.len(), 1);
    assert_eq!(state.compensation_mutations[0].len(), 1);
    let restored = &state.compensation_mutations[0][0];
    assert_eq!(restored.previous_value.as_deref(), Some("disabled"));
    assert_eq!(restored.applied_value.as_deref(), Some("enabled"));
    assert!(!restored.restored);
    Ok(())
}

#[tokio::test]
async fn already_correct_actions_do_not_restart_or_create_runtime_state_changes() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let configuration = configuration("Blog", &["first"])?;
    configurations.insert(configuration.clone())?;
    let mut state = HandlerState::default();
    state
        .observations_already_correct
        .insert("first".to_owned());
    let handler_state = Arc::new(Mutex::new(state));
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let result = engine
        .run(
            &configuration,
            request("run-correct")?,
            CancellationToken::new(),
            events,
        )
        .await?;

    assert_eq!(result.report.changed_count, 0);
    assert_eq!(result.report.already_correct_count, 1);
    assert_eq!(
        states.state(&configuration.slug)?.map(|state| state.status),
        Some(RunStatus::Ready)
    );
    Ok(())
}

#[tokio::test]
async fn failure_rolls_back_completed_actions_in_reverse_order() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState {
        fail_action: Some("third".to_owned()),
        ..HandlerState::default()
    }));
    let configuration = configuration("Blog", &["first", "second", "third"])?;
    configurations.insert(configuration.clone())?;
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let result = engine
        .run(
            &configuration,
            request("run-failure")?,
            CancellationToken::new(),
            events,
        )
        .await;
    assert!(result.is_err());
    let state = states
        .state(&configuration.slug)?
        .ok_or_else(|| std::io::Error::other("rollback state was not persisted"))?;
    assert_eq!(state.status, RunStatus::Stopped);
    let compensation_order = handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .compensation_order
        .clone();
    assert_eq!(compensation_order, vec!["second", "first"]);
    Ok(())
}

#[tokio::test]
async fn rollback_failure_preserves_runtime_state_for_follow_up_cleanup() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState {
        fail_action: Some("second".to_owned()),
        fail_compensation: true,
        ..HandlerState::default()
    }));
    let configuration = configuration("Blog", &["first", "second"])?;
    configurations.insert(configuration.clone())?;
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let result = engine
        .run(
            &configuration,
            request("run-rollback-failure")?,
            CancellationToken::new(),
            events,
        )
        .await;
    assert!(result.is_err());
    let state = states
        .state(&configuration.slug)?
        .ok_or_else(|| std::io::Error::other("failed rollback state was not persisted"))?;
    assert_eq!(state.status, RunStatus::RollbackFailed);
    assert!(!state.resources.is_empty());
    Ok(())
}

#[tokio::test]
async fn stop_preserves_pre_existing_and_shared_resources() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let current_configuration = configuration("Current", &["current"])?;
    let other_configuration = configuration("Other", &["other"])?;
    configurations.insert(current_configuration.clone())?;
    configurations.insert(other_configuration.clone())?;

    let pre_existing = resource_for("current", OwnershipStatus::PreExisting)
        .ok_or_else(|| std::io::Error::other("pre-existing resource creation failed"))?;
    let shared = resource_for("shared", OwnershipStatus::CreatedByEnvironment)
        .ok_or_else(|| std::io::Error::other("shared resource creation failed"))?;
    let mut current_state = RuntimeState::new(current_configuration.slug.clone(), "run-current");
    current_state.set_status(RunStatus::Ready);
    current_state.record_resource(pre_existing.clone())?;
    states.insert(current_state)?;

    let mut other_state = RuntimeState::new(other_configuration.slug.clone(), "run-other");
    other_state.set_status(RunStatus::Ready);
    other_state.record_resource(shared.clone().with_action(ActionId::new("other")?))?;
    states.insert(other_state)?;

    let mut current_state = states
        .state(&current_configuration.slug)?
        .ok_or_else(|| std::io::Error::other("current state missing"))?;
    current_state.record_resource(shared.clone().with_action(ActionId::new("current")?))?;
    states.insert(current_state)?;
    handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .started
        .insert("current".to_owned());
    handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .observed_resources
        .insert("current".to_owned(), vec![pre_existing, shared]);

    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());
    let result = engine
        .stop(&current_configuration, CancellationToken::new(), events)
        .await;
    let result = result?;
    assert_eq!(result.preserved_resources, 2);
    assert!(
        handler_state
            .lock()
            .map_err(|_| std::io::Error::other("handler lock failed"))?
            .stop_order
            .is_empty()
    );
    assert!(states.state(&current_configuration.slug)?.is_none());
    Ok(())
}

#[tokio::test]
async fn stop_treats_missing_resources_as_stale_and_is_idempotent() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let configuration = configuration("Blog", &["first"])?;
    configurations.insert(configuration.clone())?;
    let mut state = RuntimeState::new(configuration.slug.clone(), "run-stale");
    state.set_status(RunStatus::Ready);
    state.record_resource(
        resource_for("first", OwnershipStatus::CreatedByEnvironment)
            .ok_or_else(|| std::io::Error::other("resource creation failed"))?,
    )?;
    states.insert(state)?;
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let first = engine
        .stop(&configuration, CancellationToken::new(), events.clone())
        .await?;
    let second = engine
        .stop(&configuration, CancellationToken::new(), events)
        .await?;

    assert_eq!(first.stale_resources, 1);
    assert_eq!(first.cleaned_resources, 1);
    assert_eq!(second.cleaned_resources, 0);
    assert!(
        handler_state
            .lock()
            .map_err(|_| std::io::Error::other("handler lock failed"))?
            .stop_order
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn delete_stops_before_removing_only_the_environment_configuration() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState::default()));
    let other_configuration = configuration("Other", &["other"])?;
    let configuration = configuration("Blog", &["first"])?;
    configurations.insert(configuration.clone())?;
    configurations.insert(other_configuration.clone())?;
    let mut state = RuntimeState::new(configuration.slug.clone(), "run-delete");
    state.set_status(RunStatus::Ready);
    state.record_resource(
        resource_for("first", OwnershipStatus::CreatedByEnvironment)
            .ok_or_else(|| std::io::Error::other("resource creation failed"))?,
    )?;
    states.insert(state)?;
    handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .started
        .insert("first".to_owned());
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    let result = engine
        .delete(&configuration, CancellationToken::new(), events)
        .await?;

    assert!(result.stopped);
    assert!(result.removed);
    assert!(!configurations.contains(&configuration.slug));
    assert!(configurations.contains(&other_configuration.slug));
    assert!(states.state(&configuration.slug)?.is_none());
    assert_eq!(
        handler_state
            .lock()
            .map_err(|_| std::io::Error::other("handler lock failed"))?
            .stop_order,
        vec!["first"]
    );
    Ok(())
}

#[tokio::test]
async fn delete_keeps_configuration_and_state_when_cleanup_fails() -> TestResult {
    let configurations = Arc::new(MemoryConfigStore::default());
    let states = Arc::new(MemoryStateStore::default());
    let handler_state = Arc::new(Mutex::new(HandlerState {
        fail_stop: true,
        ..HandlerState::default()
    }));
    let configuration = configuration("Blog", &["first"])?;
    configurations.insert(configuration.clone())?;
    let mut state = RuntimeState::new(configuration.slug.clone(), "run-delete-failure");
    state.set_status(RunStatus::Ready);
    state.record_resource(
        resource_for("first", OwnershipStatus::CreatedByEnvironment)
            .ok_or_else(|| std::io::Error::other("resource creation failed"))?,
    )?;
    states.insert(state)?;
    handler_state
        .lock()
        .map_err(|_| std::io::Error::other("handler lock failed"))?
        .started
        .insert("first".to_owned());
    let engine = lifecycle_engine(&configurations, &states, &handler_state, false, 1)?;
    let events: Arc<dyn EventSink> = Arc::new(InMemoryEventSink::default());

    assert!(
        engine
            .delete(&configuration, CancellationToken::new(), events)
            .await
            .is_err()
    );
    assert!(configurations.contains(&configuration.slug));
    let state = states
        .state(&configuration.slug)?
        .ok_or_else(|| std::io::Error::other("failed cleanup state was removed"))?;
    assert_eq!(state.status, RunStatus::Partial);
    assert_eq!(state.resources.len(), 1);
    Ok(())
}

#[test]
fn lifecycle_states_reject_invalid_transitions() {
    let Some(slug) = EnvironmentSlug::new("blog").ok() else {
        return;
    };
    let mut state = RuntimeState::new(slug, "run");
    assert!(state.transition_to(RunStatus::Ready).is_err());
    assert!(state.transition_to(RunStatus::Active).is_ok());
    assert!(state.transition_to(RunStatus::Ready).is_ok());
    assert!(state.transition_to(RunStatus::RollingBack).is_err());
}
