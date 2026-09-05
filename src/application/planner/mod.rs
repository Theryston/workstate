use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{sync::watch, task::JoinSet};

use crate::{
    application::ports::{
        BoxFuture, DesktopBackend, resolve_workspace_target,
        resolve_workspace_target_with_reservations,
    },
    domain::{
        ActionGraph, ActionId, ActionKind, ActionSpec, EnvironmentConfig, MutationRecord,
        ReadinessCheck, ResourceKind, ResourceRecord, RuntimeState, WorkspaceId,
        WorkspaceReference, WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    integrations::IntegrationRegistry,
    platform::CapabilityId,
};

pub mod plan;

pub use plan::{ExecutionPlan, PlanClassification, PlanEntry, PlanStrategy};

#[derive(Clone, Debug)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    signal: watch::Sender<bool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        let (signal, _) = watch::channel(false);
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                signal,
            }),
        }
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::AcqRel) {
            self.state.signal.send_replace(true);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(cancellation_error(None, "operation"));
        }
        Ok(())
    }

    pub async fn cancelled(&self) {
        let mut signal = self.state.signal.subscribe();
        while !self.is_cancelled() {
            if signal.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationStatus {
    AlreadyCorrect,
    RequiresChange,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionObservation {
    pub status: ObservationStatus,
    pub detail: Option<String>,
    pub resources: Vec<ResourceRecord>,
}

impl ActionObservation {
    pub fn already_correct() -> Self {
        Self {
            status: ObservationStatus::AlreadyCorrect,
            detail: None,
            resources: Vec::new(),
        }
    }

    pub fn requires_change() -> Self {
        Self {
            status: ObservationStatus::RequiresChange,
            detail: None,
            resources: Vec::new(),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: ObservationStatus::Unknown,
            detail: Some(reason.into()),
            resources: Vec::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_resources(mut self, resources: Vec<ResourceRecord>) -> Self {
        self.resources = resources;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutputStream {
    Stdout,
    Stderr,
    Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutput {
    pub stream: ActionOutputStream,
    pub message: String,
}

impl ActionOutput {
    pub fn stdout(message: impl Into<String>) -> Self {
        Self {
            stream: ActionOutputStream::Stdout,
            message: message.into(),
        }
    }

    pub fn stderr(message: impl Into<String>) -> Self {
        Self {
            stream: ActionOutputStream::Stderr,
            message: message.into(),
        }
    }

    pub fn log(message: impl Into<String>) -> Self {
        Self {
            stream: ActionOutputStream::Log,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionExecutionResult {
    pub changed: bool,
    pub resources: Vec<ResourceRecord>,
    pub mutations: Vec<MutationRecord>,
    pub outputs: Vec<ActionOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessCheckResult {
    pub passed: bool,
    pub detail: Option<String>,
}

impl ReadinessCheckResult {
    pub fn passed() -> Self {
        Self {
            passed: true,
            detail: None,
        }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self {
            passed: false,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadinessReport {
    pub checks_run: usize,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompensationResult {
    pub outputs: Vec<ActionOutput>,
}

pub trait ActionOutputSink: Send + Sync {
    fn emit<'a>(&'a self, output: ActionOutput) -> BoxFuture<'a, Result<()>>;
}

pub trait ActionHandler: Send + Sync {
    fn action_key(&self) -> &str;

    fn required_capabilities(&self) -> BTreeSet<CapabilityId> {
        BTreeSet::new()
    }

    fn validate(&self, _action: &ActionSpec) -> Result<()> {
        Ok(())
    }

    fn execution_timeout(
        &self,
        action: &ActionSpec,
        default_timeout: Duration,
    ) -> Option<Duration> {
        action
            .timeout
            .as_ref()
            .map(|timeout| Duration::from_millis(timeout.milliseconds))
            .or(Some(default_timeout))
    }

    fn requires_workspace_target_for_observation(&self, _action: &ActionSpec) -> bool {
        true
    }

    fn observe_with_resources<'a>(
        &'a self,
        action: &'a ActionSpec,
        _previous_resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        self.observe(action, cancellation)
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>>;

    fn observe_for_cleanup<'a>(
        &'a self,
        action: &'a ActionSpec,
        _resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        self.observe(action, cancellation)
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>>;

    fn run_once<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.apply(action, cancellation)
    }

    fn run_once_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        _output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.run_once(action, cancellation)
    }

    fn start_background<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.apply(action, cancellation)
    }

    fn start_background_with_output<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
        _output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        self.start_background(action, cancellation)
    }

    fn wait_for_readiness<'a>(
        &'a self,
        action: &'a ActionSpec,
        runner: &'a dyn ReadinessCheckRunner,
        default_timeout: Duration,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ReadinessReport>> {
        Box::pin(run_readiness_checks(
            action,
            runner,
            default_timeout,
            cancellation,
        ))
    }

    fn compensate<'a>(
        &'a self,
        _action: &'a ActionSpec,
        _result: &'a ActionExecutionResult,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async { Ok(CompensationResult::default()) })
    }

    fn stop<'a>(
        &'a self,
        _action: &'a ActionSpec,
        _resources: &'a [ResourceRecord],
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async { Ok(CompensationResult::default()) })
    }
}

pub trait ReadinessCheckRunner: Send + Sync {
    fn check<'a>(
        &'a self,
        action_id: &'a ActionId,
        check: &'a ReadinessCheck,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ReadinessCheckResult>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopReadinessCheckRunner;

impl ReadinessCheckRunner for NoopReadinessCheckRunner {
    fn check<'a>(
        &'a self,
        action_id: &'a ActionId,
        check: &'a ReadinessCheck,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ReadinessCheckResult>> {
        Box::pin(async move {
            cancellation.check()?;
            match check {
                ReadinessCheck::None => Ok(ReadinessCheckResult::passed()),
                ReadinessCheck::Delay { milliseconds } => {
                    tokio::select! {
                        _ = cancellation.cancelled() => {
                            Err(cancellation_error(Some(action_id), "readiness check"))
                        }
                        _ = tokio::time::sleep(Duration::from_millis(*milliseconds)) => {
                            Ok(ReadinessCheckResult::passed())
                        }
                    }
                }
                _ => Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    format!(
                        "readiness check '{}' requires a concrete integration",
                        readiness_check_label(check)
                    ),
                )
                .with_context("action_id", action_id.to_string())),
            }
        })
    }
}

#[derive(Clone, Default)]
pub struct ActionHandlerRegistry {
    handlers: BTreeMap<String, Arc<dyn ActionHandler>>,
}

impl ActionHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<H>(&mut self, handler: H) -> Result<()>
    where
        H: ActionHandler + 'static,
    {
        self.register_shared(Arc::new(handler))
    }

    pub fn register_shared(&mut self, handler: Arc<dyn ActionHandler>) -> Result<()> {
        let action_key = handler.action_key().to_owned();
        if action_key.is_empty() {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "an action handler must have a non-empty action key",
            ));
        }
        if self.handlers.contains_key(&action_key) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("action handler '{action_key}' is already registered"),
            ));
        }
        self.handlers.insert(action_key, handler);
        Ok(())
    }

    pub fn handler_for(&self, action: &ActionKind) -> Option<Arc<dyn ActionHandler>> {
        self.handlers.get(&action.key()).cloned()
    }

    pub fn handler_by_key(&self, action_key: &str) -> Option<Arc<dyn ActionHandler>> {
        self.handlers.get(action_key).cloned()
    }

    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

pub struct Planner<'a> {
    integrations: &'a IntegrationRegistry,
    handlers: &'a ActionHandlerRegistry,
}

impl<'a> Planner<'a> {
    pub fn new(integrations: &'a IntegrationRegistry, handlers: &'a ActionHandlerRegistry) -> Self {
        Self {
            integrations,
            handlers,
        }
    }

    pub fn build(&self, configuration: &crate::domain::EnvironmentConfig) -> Result<ExecutionPlan> {
        configuration.validate().map_err(WorkstateError::from)?;
        let workspace_ids = configuration
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<BTreeSet<_>>();
        let graph = ActionGraph::validate(&configuration.actions, &workspace_ids)
            .map_err(WorkstateError::from)?;
        let mut entries = BTreeMap::new();

        for source_action in &configuration.actions {
            let mut action = source_action.clone();
            enrich_workspace_context(&mut action, configuration);
            let descriptor = self.integrations.handler_for(&action.kind);
            let handler = self.handlers.handler_for(&action.kind);
            let mut required_capabilities = descriptor
                .map(|value| value.required_capabilities.clone())
                .unwrap_or_default();
            if let Some(handler) = &handler {
                required_capabilities.extend(handler.required_capabilities());
            }
            let missing_capabilities = required_capabilities
                .iter()
                .copied()
                .filter(|capability| {
                    self.integrations
                        .capability(*capability)
                        .is_none_or(|availability| !availability.available)
                })
                .collect::<Vec<_>>();
            let handler_key = action.kind.key();
            let unavailable_reason = format!("no handler is registered for '{handler_key}'");
            let strategy = handler
                .as_ref()
                .map(|_| PlanStrategy::Handler {
                    action_key: handler_key.clone(),
                })
                .unwrap_or_else(|| PlanStrategy::NotAvailable {
                    reason: unavailable_reason.clone(),
                });

            let (classification, detail) = match handler.as_ref() {
                None => (PlanClassification::Invalid, Some(unavailable_reason)),
                Some(handler) => match handler.validate(&action) {
                    Err(error) => (PlanClassification::Invalid, Some(error.to_string())),
                    Ok(()) if !missing_capabilities.is_empty() => (
                        PlanClassification::BlockedByMissingCapability,
                        Some(
                            missing_capabilities
                                .iter()
                                .map(|capability| capability.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    ),
                    Ok(()) => (
                        PlanClassification::Unknown,
                        Some("observation pending".to_owned()),
                    ),
                },
            };

            let entry = PlanEntry::from_action(
                action.clone(),
                required_capabilities,
                strategy,
                classification,
                detail,
                missing_capabilities,
            );
            entries.insert(action.id.clone(), entry);
        }

        Ok(ExecutionPlan::new(
            configuration.slug.clone(),
            &graph,
            entries,
        ))
    }

    pub async fn resolve_workspace_targets(
        &self,
        plan: &mut ExecutionPlan,
        configuration: &EnvironmentConfig,
        desktop: &dyn DesktopBackend,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.resolve_workspace_targets_with_state(plan, configuration, desktop, cancellation, None)
            .await
    }

    pub async fn resolve_workspace_targets_with_state(
        &self,
        plan: &mut ExecutionPlan,
        configuration: &EnvironmentConfig,
        desktop: &dyn DesktopBackend,
        cancellation: CancellationToken,
        previous_state: Option<&RuntimeState>,
    ) -> Result<()> {
        self.resolve_workspace_targets_internal(
            plan,
            configuration,
            desktop,
            cancellation,
            previous_state,
            WorkspaceResolutionPhase::Execution,
        )
        .await
    }

    pub async fn resolve_workspace_targets_for_observation_with_state(
        &self,
        plan: &mut ExecutionPlan,
        configuration: &EnvironmentConfig,
        desktop: &dyn DesktopBackend,
        cancellation: CancellationToken,
        previous_state: Option<&RuntimeState>,
    ) -> Result<()> {
        self.resolve_workspace_targets_internal(
            plan,
            configuration,
            desktop,
            cancellation,
            previous_state,
            WorkspaceResolutionPhase::Observation,
        )
        .await
    }

    async fn resolve_workspace_targets_internal(
        &self,
        plan: &mut ExecutionPlan,
        configuration: &EnvironmentConfig,
        desktop: &dyn DesktopBackend,
        cancellation: CancellationToken,
        previous_state: Option<&RuntimeState>,
        phase: WorkspaceResolutionPhase,
    ) -> Result<()> {
        let referenced_workspace_ids = plan
            .entries()
            .filter(|entry| match phase {
                WorkspaceResolutionPhase::Observation => {
                    entry.classification == PlanClassification::Unknown
                        && self
                            .handlers
                            .handler_for(&entry.action.kind)
                            .is_none_or(|handler| {
                                handler.requires_workspace_target_for_observation(&entry.action)
                            })
                }
                WorkspaceResolutionPhase::Execution => matches!(
                    entry.classification,
                    PlanClassification::Unknown | PlanClassification::RequiresChange
                ),
            })
            .filter_map(|entry| workspace_id_for_action(&entry.action).cloned())
            .collect::<BTreeSet<_>>();
        if referenced_workspace_ids.is_empty() {
            return Ok(());
        }

        cancellation.check()?;
        let snapshot = desktop.snapshot().await?;
        let mut reserved_next_empty = BTreeSet::new();
        let mut resolved_targets = BTreeMap::<WorkspaceId, WorkspaceTarget>::new();

        for workspace in &configuration.workspaces {
            if !referenced_workspace_ids.contains(&workspace.id) {
                continue;
            }
            let target = match &workspace.target {
                WorkspaceTarget::Current | WorkspaceTarget::Existing { .. } => {
                    let resolution = resolve_workspace_target(&snapshot, &workspace.target)?;
                    anchor_workspace_target(&workspace.target, resolution)?
                }
                WorkspaceTarget::NextEmpty => {
                    let persisted_target = match previous_state {
                        Some(state) => persisted_workspace_target(
                            &workspace.id,
                            configuration,
                            state,
                            &snapshot,
                        )?,
                        None => None,
                    };
                    let anchored = match persisted_target {
                        Some(target) => target,
                        None => {
                            let resolution = resolve_workspace_target_with_reservations(
                                &snapshot,
                                &workspace.target,
                                &reserved_next_empty,
                            )?;
                            anchor_workspace_target(&workspace.target, resolution)?
                        }
                    };
                    if let WorkspaceTarget::Existing {
                        reference: WorkspaceReference::Identifier(identity),
                    } = &anchored
                    {
                        reserved_next_empty.insert(identity.clone());
                    }
                    anchored
                }
                WorkspaceTarget::Create { .. } | WorkspaceTarget::None => workspace.target.clone(),
            };
            resolved_targets.insert(workspace.id.clone(), target);
        }

        let ordered_action_ids = plan.ordered_action_ids().to_vec();
        for action_id in ordered_action_ids {
            let Some(entry) = plan.entry_mut(&action_id) else {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("workspace resolution returned an unknown action '{action_id}'"),
                ));
            };
            let Some(workspace_id) = workspace_id_for_action(&entry.action) else {
                continue;
            };
            if let Some(target) = resolved_targets.get(workspace_id) {
                entry.action.resolved_workspace_target = Some(target.clone());
            }
        }

        Ok(())
    }

    pub async fn observe(
        &self,
        plan: &mut ExecutionPlan,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.observe_with_timeout_and_state(
            plan,
            cancellation,
            crate::application::timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
            None,
        )
        .await
    }

    pub async fn observe_with_timeout(
        &self,
        plan: &mut ExecutionPlan,
        cancellation: CancellationToken,
        default_timeout: Duration,
    ) -> Result<()> {
        self.observe_with_timeout_and_state(plan, cancellation, default_timeout, None)
            .await
    }

    pub async fn observe_with_timeout_and_state(
        &self,
        plan: &mut ExecutionPlan,
        cancellation: CancellationToken,
        default_timeout: Duration,
        previous_state: Option<&RuntimeState>,
    ) -> Result<()> {
        if default_timeout.is_zero() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "observation timeout must be greater than zero",
            ));
        }
        cancellation.check()?;
        let previous_resources = previous_state
            .map(|state| {
                state
                    .resources
                    .iter()
                    .filter_map(|record| {
                        record
                            .action_id
                            .clone()
                            .map(|action_id| (action_id, record.clone()))
                    })
                    .fold(
                        BTreeMap::<ActionId, Vec<ResourceRecord>>::new(),
                        |mut resources, (action_id, record)| {
                            resources.entry(action_id).or_default().push(record);
                            resources
                        },
                    )
            })
            .unwrap_or_default();
        let mut jobs = JoinSet::new();
        for entry in plan
            .entries()
            .filter(|entry| entry.classification == PlanClassification::Unknown)
        {
            let Some(handler) = self.handlers.handler_for(&entry.action.kind) else {
                continue;
            };
            let action = entry.action.clone();
            let action_id = entry.action_id.clone();
            let resources = previous_resources
                .get(&action_id)
                .cloned()
                .unwrap_or_default();
            let token = cancellation.clone();
            let timeout = entry.timeout.unwrap_or(default_timeout);
            jobs.spawn(async move {
                let observation = run_with_timeout(
                    handler.observe_with_resources(&action, &resources, token.clone()),
                    timeout,
                    token,
                    Some(&action_id),
                    "observation",
                )
                .await;
                (action_id, observation)
            });
        }

        while let Some(joined) = jobs.join_next().await {
            let (action_id, observation) = match joined {
                Ok(value) => value,
                Err(error) => {
                    jobs.abort_all();
                    while jobs.join_next().await.is_some() {}
                    return Err(WorkstateError::new(
                        ErrorCategory::Runtime,
                        "an observation task failed before returning a result",
                    )
                    .with_context("join_error", error.to_string()));
                }
            };
            let observation = match observation {
                Ok(value) => value,
                Err(error) => {
                    jobs.abort_all();
                    while jobs.join_next().await.is_some() {}
                    return Err(error.with_context("action_id", action_id.to_string()));
                }
            };
            let Some(entry) = plan.entry_mut(&action_id) else {
                jobs.abort_all();
                while jobs.join_next().await.is_some() {}
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("observation returned an unknown action '{action_id}'"),
                ));
            };
            entry.classification = match observation.status {
                ObservationStatus::AlreadyCorrect => {
                    entry.apply_strategy = PlanStrategy::NotRequired;
                    entry.compensation_strategy = PlanStrategy::NotRequired;
                    PlanClassification::AlreadyCorrect
                }
                ObservationStatus::RequiresChange => PlanClassification::RequiresChange,
                ObservationStatus::Unknown => PlanClassification::Unknown,
            };
            entry.classification_detail = observation.detail;
            entry.observed_resources = observation.resources;
        }

        Ok(())
    }
}

pub(crate) fn enrich_workspace_context(
    action: &mut ActionSpec,
    configuration: &crate::domain::EnvironmentConfig,
) {
    action.resolved_environment = Some(configuration.slug.clone());
    let workspace_id = action
        .desktop_workspace
        .as_ref()
        .or(action.parameters.workspace_id.as_ref());
    let Some(workspace_id) = workspace_id else {
        return;
    };
    let Some(workspace) = configuration
        .workspaces
        .iter()
        .find(|workspace| &workspace.id == workspace_id)
    else {
        return;
    };
    action.resolved_workspace_target = Some(workspace.target.clone());
    action.resolved_tiling = Some(workspace.tiling);
}

fn workspace_id_for_action(action: &ActionSpec) -> Option<&WorkspaceId> {
    action
        .desktop_workspace
        .as_ref()
        .or(action.parameters.workspace_id.as_ref())
}

fn anchor_workspace_target(
    target: &WorkspaceTarget,
    resolution: crate::application::ports::DesktopWorkspaceResolution,
) -> Result<WorkspaceTarget> {
    let Some(workspace) = resolution.workspace else {
        return Ok(target.clone());
    };
    Ok(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier(workspace.identity),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceResolutionPhase {
    Observation,
    Execution,
}

fn persisted_workspace_target(
    workspace_id: &WorkspaceId,
    configuration: &EnvironmentConfig,
    state: &RuntimeState,
    snapshot: &crate::application::ports::DesktopSnapshot,
) -> Result<Option<WorkspaceTarget>> {
    let action_ids = configuration
        .actions
        .iter()
        .filter(|action| workspace_id_for_action(action) == Some(workspace_id))
        .map(|action| action.id.clone())
        .collect::<BTreeSet<_>>();
    if action_ids.is_empty() {
        return Ok(None);
    }

    let identities = state
        .resources
        .iter()
        .filter(|record| {
            record
                .action_id
                .as_ref()
                .is_some_and(|action_id| action_ids.contains(action_id))
        })
        .filter_map(workspace_identity_from_record)
        .filter(|identity| snapshot.workspace(identity).is_some())
        .collect::<BTreeSet<_>>();

    match identities.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [identity] => Ok(Some(WorkspaceTarget::Existing {
            reference: WorkspaceReference::Identifier((*identity).clone()),
        })),
        identities => Err(WorkstateError::new(
            ErrorCategory::Persistence,
            "the persisted desktop workspace binding is ambiguous",
        )
        .with_context("workspace_id", workspace_id.to_string())
        .with_context("workspace_identities", identities.join(", "))),
    }
}

fn workspace_identity_from_record(record: &ResourceRecord) -> Option<String> {
    match record.resource.kind {
        ResourceKind::DesktopWorkspace => Some(record.resource.stable_identity.clone()),
        ResourceKind::DesktopWindow => record
            .integration_metadata
            .get("workspace_identity")
            .cloned(),
        _ => None,
    }
}

pub(crate) async fn run_readiness_checks(
    action: &ActionSpec,
    runner: &dyn ReadinessCheckRunner,
    default_timeout: Duration,
    cancellation: CancellationToken,
) -> Result<ReadinessReport> {
    let mut checks_run = 0usize;
    let mut detail = None;
    for check in &action.readiness_checks {
        cancellation.check()?;
        let timeout = readiness_timeout(check, default_timeout);
        let result = run_with_timeout(
            runner.check(&action.id, check, cancellation.clone()),
            timeout,
            cancellation.clone(),
            Some(&action.id),
            "readiness check",
        )
        .await?;
        checks_run += 1;
        if !result.passed {
            let message = result
                .detail
                .unwrap_or_else(|| "readiness check did not pass".to_owned());
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                format!(
                    "action '{}' failed its readiness check: {message}",
                    action.id
                ),
            )
            .with_context("action_id", action.id.to_string()));
        }
        detail = result.detail;
    }
    Ok(ReadinessReport { checks_run, detail })
}

pub(crate) async fn run_with_timeout<F, T>(
    future: F,
    timeout: Duration,
    cancellation: CancellationToken,
    action_id: Option<&ActionId>,
    phase: &str,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    run_with_optional_timeout(future, Some(timeout), cancellation, action_id, phase).await
}

pub(crate) async fn run_with_optional_timeout<F, T>(
    future: F,
    timeout: Option<Duration>,
    cancellation: CancellationToken,
    action_id: Option<&ActionId>,
    phase: &str,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    match timeout {
        Some(timeout) => tokio::select! {
            _ = cancellation.cancelled() => Err(cancellation_error(action_id, phase)),
            result = tokio::time::timeout(timeout, future) => match result {
                Ok(value) => value,
                Err(_) => {
                    let mut error = WorkstateError::new(
                        ErrorCategory::Runtime,
                        format!("{phase} timed out"),
                    )
                    .with_context("timeout_milliseconds", timeout.as_millis().to_string());
                    if let Some(action_id) = action_id {
                        error = error.with_context("action_id", action_id.to_string());
                    }
                    Err(error)
                }
            },
        },
        None => tokio::select! {
            _ = cancellation.cancelled() => Err(cancellation_error(action_id, phase)),
            result = future => result,
        },
    }
}

pub(crate) fn readiness_timeout(check: &ReadinessCheck, default: Duration) -> Duration {
    match check {
        ReadinessCheck::Tcp { timeout, .. }
        | ReadinessCheck::Http { timeout, .. }
        | ReadinessCheck::Command { timeout, .. }
        | ReadinessCheck::Container { timeout, .. }
        | ReadinessCheck::Compose { timeout, .. } => Duration::from_millis(timeout.milliseconds),
        ReadinessCheck::Delay { milliseconds } => Duration::from_millis(*milliseconds),
        ReadinessCheck::None => default,
    }
}

fn readiness_check_label(check: &ReadinessCheck) -> &'static str {
    match check {
        ReadinessCheck::None => "none",
        ReadinessCheck::Tcp { .. } => "tcp",
        ReadinessCheck::Http { .. } => "http",
        ReadinessCheck::Command { .. } => "command",
        ReadinessCheck::Delay { .. } => "delay",
        ReadinessCheck::Container { .. } => "container",
        ReadinessCheck::Compose { .. } => "compose",
    }
}

pub(crate) fn cancellation_error(action_id: Option<&ActionId>, phase: &str) -> WorkstateError {
    let mut error = WorkstateError::new(
        ErrorCategory::Runtime,
        format!("operation was cancelled during {phase}"),
    )
    .with_context("cancelled", "true");
    if let Some(action_id) = action_id {
        error = error.with_context("action_id", action_id.to_string());
    }
    error
}

pub(crate) fn is_cancellation_error(error: &WorkstateError) -> bool {
    error
        .context
        .get("cancelled")
        .is_some_and(|value| value == "true")
}

#[cfg(test)]
mod tests {
    use crate::{
        application::planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, CancellationToken, ObservationStatus, Planner,
        },
        application::ports::{
            BoxFuture, DesktopBackend, DesktopSnapshot, DesktopWindowSnapshot,
            DesktopWorkspaceSnapshot,
        },
        domain::{
            ActionKind, ActionParameters, ActionSpec, EnvironmentConfig, ResourceIdentity,
            ResourceKind, ResourceRecord, RuntimeState, WorkspaceId, WorkspaceReference,
            WorkspaceSpec, WorkspaceTarget,
        },
        integrations::IntegrationRegistry,
    };

    use super::{NoopReadinessCheckRunner, ReadinessCheckRunner};

    struct FakeHandler {
        key: &'static str,
        status: ObservationStatus,
    }

    impl ActionHandler for FakeHandler {
        fn action_key(&self) -> &str {
            self.key
        }

        fn observe<'a>(
            &'a self,
            _action: &'a ActionSpec,
            _cancellation: CancellationToken,
        ) -> crate::application::ports::BoxFuture<'a, crate::error::Result<ActionObservation>>
        {
            let status = self.status;
            Box::pin(async move {
                Ok(ActionObservation {
                    status,
                    detail: Some("fake observation".to_owned()),
                    resources: Vec::new(),
                })
            })
        }

        fn apply<'a>(
            &'a self,
            action: &'a ActionSpec,
            _cancellation: CancellationToken,
        ) -> crate::application::ports::BoxFuture<'a, crate::error::Result<ActionExecutionResult>>
        {
            Box::pin(async move {
                Ok(ActionExecutionResult {
                    changed: true,
                    resources: Vec::new(),
                    mutations: Vec::new(),
                    outputs: vec![ActionOutput::log(action.id.to_string())],
                })
            })
        }
    }

    struct StaticDesktop {
        snapshot: DesktopSnapshot,
    }

    impl DesktopBackend for StaticDesktop {
        fn snapshot<'a>(&'a self) -> BoxFuture<'a, crate::error::Result<DesktopSnapshot>> {
            let snapshot = self.snapshot.clone();
            Box::pin(async move { Ok(snapshot) })
        }
    }

    #[tokio::test]
    async fn planner_classifies_handler_observations_without_mutating_configuration() {
        let Some(configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "open_application",
                    status: ObservationStatus::AlreadyCorrect,
                })
                .is_ok()
        );
        let mut registry = IntegrationRegistry::new();
        assert!(
            registry
                .set_capability_availability(
                    crate::platform::CapabilityId::DesktopWindows,
                    true,
                    None,
                )
                .is_ok()
        );
        let planner = Planner::new(&registry, &handlers);
        let mut configuration = configuration;
        let Some(mut action) = ActionSpec::new("editor", ActionKind::OpenApplication).ok() else {
            return;
        };
        action.parameters = ActionParameters {
            application: Some("zed".to_owned()),
            ..ActionParameters::default()
        };
        configuration.actions.push(action);
        let plan = planner.build(&configuration);
        assert!(plan.is_ok());
        let Some(mut plan) = plan.ok() else {
            return;
        };
        assert_eq!(plan.expected_mutation_count(), 0);
        assert!(
            planner
                .observe(&mut plan, CancellationToken::new())
                .await
                .is_ok()
        );
        assert_eq!(
            plan.entries().next().map(|entry| entry.classification),
            Some(super::plan::PlanClassification::AlreadyCorrect)
        );
        assert_eq!(configuration.actions.len(), 1);
    }

    #[tokio::test]
    async fn shared_next_empty_workspace_is_anchored_for_all_dependent_actions() {
        let Some(mut configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let Some(workspace) = WorkspaceSpec::new("shared", WorkspaceTarget::NextEmpty).ok() else {
            return;
        };
        configuration.workspaces.push(workspace);

        let mut first = match ActionSpec::new("first", ActionKind::OpenApplication) {
            Ok(action) => action,
            Err(_) => return,
        };
        first.parameters.application = Some("zed".to_owned());
        first.desktop_workspace = WorkspaceId::new("shared").ok();

        let mut second = match ActionSpec::new("second", ActionKind::OpenApplication) {
            Ok(action) => action,
            Err(_) => return,
        };
        second.parameters.application = Some("zed".to_owned());
        second.desktop_workspace = WorkspaceId::new("shared").ok();

        let mut tiling = match ActionSpec::new("tiling", ActionKind::ConfigureTiling) {
            Ok(action) => action,
            Err(_) => return,
        };
        tiling.desktop_workspace = WorkspaceId::new("shared").ok();
        tiling.depends_on = vec![
            match crate::domain::ActionId::new("first") {
                Ok(id) => id,
                Err(_) => return,
            },
            match crate::domain::ActionId::new("second") {
                Ok(id) => id,
                Err(_) => return,
            },
        ];
        configuration.actions = vec![first, second, tiling];

        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "open_application",
                    status: ObservationStatus::RequiresChange,
                })
                .is_ok()
        );
        assert!(
            handlers
                .register(FakeHandler {
                    key: "configure_tiling",
                    status: ObservationStatus::RequiresChange,
                })
                .is_ok()
        );
        let mut integrations = IntegrationRegistry::new();
        assert!(
            integrations
                .set_capability_availability(
                    crate::platform::CapabilityId::DesktopWindows,
                    true,
                    None,
                )
                .is_ok()
        );
        assert!(
            integrations
                .set_capability_availability(
                    crate::platform::CapabilityId::DesktopTiling,
                    true,
                    None,
                )
                .is_ok()
        );

        let planner = Planner::new(&integrations, &handlers);
        let mut plan = match planner.build(&configuration) {
            Ok(plan) => plan,
            Err(_) => return,
        };
        let desktop = StaticDesktop {
            snapshot: DesktopSnapshot {
                workspaces: vec![
                    DesktopWorkspaceSnapshot {
                        identity: "main".to_owned(),
                        name: Some("Main".to_owned()),
                        position: Some(0),
                        focused: true,
                        tiling_enabled: Some(false),
                    },
                    DesktopWorkspaceSnapshot {
                        identity: "empty-one".to_owned(),
                        name: Some("Empty One".to_owned()),
                        position: Some(1),
                        focused: false,
                        tiling_enabled: Some(false),
                    },
                    DesktopWorkspaceSnapshot {
                        identity: "empty-two".to_owned(),
                        name: Some("Empty Two".to_owned()),
                        position: Some(2),
                        focused: false,
                        tiling_enabled: Some(false),
                    },
                ],
                windows: vec![DesktopWindowSnapshot {
                    identity: "terminal".to_owned(),
                    application: Some("terminal".to_owned()),
                    title: Some("Shell".to_owned()),
                    project_path: None,
                    workspace_identity: Some("main".to_owned()),
                    focused: true,
                }],
            },
        };

        assert!(
            planner
                .resolve_workspace_targets(
                    &mut plan,
                    &configuration,
                    &desktop,
                    CancellationToken::new(),
                )
                .await
                .is_ok()
        );

        let expected = Some(WorkspaceTarget::Existing {
            reference: WorkspaceReference::Identifier("empty-one".to_owned()),
        });
        assert!(
            plan.entries()
                .all(|entry| entry.action.resolved_workspace_target == expected)
        );
    }

    #[tokio::test]
    async fn persisted_next_empty_workspace_is_reused_across_runs() {
        let Some(mut configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let Some(workspace) = WorkspaceSpec::new("shared", WorkspaceTarget::NextEmpty).ok() else {
            return;
        };
        configuration.workspaces.push(workspace);
        let Some(mut action) = ActionSpec::new("editor", ActionKind::OpenApplication).ok() else {
            return;
        };
        action.parameters.application = Some("zed".to_owned());
        action.desktop_workspace = WorkspaceId::new("shared").ok();
        configuration.actions.push(action);

        let mut handlers = ActionHandlerRegistry::new();
        assert!(
            handlers
                .register(FakeHandler {
                    key: "open_application",
                    status: ObservationStatus::RequiresChange,
                })
                .is_ok()
        );
        let mut integrations = IntegrationRegistry::new();
        assert!(
            integrations
                .set_capability_availability(
                    crate::platform::CapabilityId::DesktopWindows,
                    true,
                    None,
                )
                .is_ok()
        );
        let planner = Planner::new(&integrations, &handlers);
        let mut plan = match planner.build(&configuration) {
            Ok(plan) => plan,
            Err(_) => return,
        };

        let mut state = RuntimeState::new(configuration.slug.clone(), "run-1");
        let Some(action_id) = crate::domain::ActionId::new("editor").ok() else {
            return;
        };
        let Some(identity) = ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-editor").ok()
        else {
            return;
        };
        let mut resource = ResourceRecord::new(
            identity,
            crate::domain::OwnershipStatus::CreatedByEnvironment,
        )
        .with_action(action_id);
        resource.observed_before = false;
        resource.integration_metadata.insert(
            "workspace_identity".to_owned(),
            "bound-workspace".to_owned(),
        );
        assert!(state.record_resource(resource).is_ok());

        let desktop = StaticDesktop {
            snapshot: DesktopSnapshot {
                workspaces: vec![
                    DesktopWorkspaceSnapshot {
                        identity: "main".to_owned(),
                        name: Some("Main".to_owned()),
                        position: Some(0),
                        focused: true,
                        tiling_enabled: Some(false),
                    },
                    DesktopWorkspaceSnapshot {
                        identity: "bound-workspace".to_owned(),
                        name: Some("Bound".to_owned()),
                        position: Some(1),
                        focused: false,
                        tiling_enabled: Some(true),
                    },
                    DesktopWorkspaceSnapshot {
                        identity: "other-empty".to_owned(),
                        name: Some("Other Empty".to_owned()),
                        position: Some(2),
                        focused: false,
                        tiling_enabled: Some(false),
                    },
                ],
                windows: vec![DesktopWindowSnapshot {
                    identity: "terminal".to_owned(),
                    application: Some("terminal".to_owned()),
                    title: Some("Shell".to_owned()),
                    project_path: None,
                    workspace_identity: Some("main".to_owned()),
                    focused: true,
                }],
            },
        };

        assert!(
            planner
                .resolve_workspace_targets_with_state(
                    &mut plan,
                    &configuration,
                    &desktop,
                    CancellationToken::new(),
                    Some(&state),
                )
                .await
                .is_ok()
        );

        assert_eq!(
            plan.entries()
                .next()
                .and_then(|entry| entry.action.resolved_workspace_target.clone()),
            Some(WorkspaceTarget::Existing {
                reference: WorkspaceReference::Identifier("bound-workspace".to_owned()),
            })
        );
    }

    #[tokio::test]
    async fn default_readiness_runner_only_executes_non_external_checks() {
        let Some(action) = ActionSpec::new("command", ActionKind::RunCommand).ok() else {
            return;
        };
        let runner = NoopReadinessCheckRunner;
        let result = runner
            .check(
                &action.id,
                &crate::domain::ReadinessCheck::None,
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_ok());
    }
}
