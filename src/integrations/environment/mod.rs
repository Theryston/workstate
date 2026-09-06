use std::sync::{Arc, Mutex};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, ActionOutputSink, CancellationToken, CompensationResult,
        },
        ports::{BoxFuture, EnvironmentLifecycleBackend},
    },
    domain::{
        ActionKind, ActionSpec, EnvironmentSlug, OwnershipStatus, ResourceIdentity, ResourceKind,
        ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone)]
pub struct StartOtherEnvironmentActionHandler {
    backend: Arc<dyn EnvironmentLifecycleBackend>,
}

impl StartOtherEnvironmentActionHandler {
    pub fn new(backend: Arc<dyn EnvironmentLifecycleBackend>) -> Self {
        Self { backend }
    }

    fn target_for<'a>(&self, action: &'a ActionSpec) -> Result<&'a EnvironmentSlug> {
        action.parameters.other_environment.as_ref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!(
                    "Start Other Environment action '{}' is missing its environment",
                    action.id
                ),
            )
        })
    }

    fn validate_target_identity<'a>(&self, action: &'a ActionSpec) -> Result<&'a EnvironmentSlug> {
        let target = self.target_for(action)?;
        if action
            .resolved_environment
            .as_ref()
            .is_some_and(|environment| environment == target)
        {
            return Err(WorkstateError::new(
                ErrorCategory::Domain,
                format!(
                    "Start Other Environment action '{}' cannot start its own environment",
                    action.id
                ),
            )
            .with_context("environment", target.to_string()));
        }
        Ok(target)
    }

    fn validate_target<'a>(&self, action: &'a ActionSpec) -> Result<&'a EnvironmentSlug> {
        let target = self.validate_target_identity(action)?;
        if !self.backend.exists(target)? {
            return Err(WorkstateError::new(
                ErrorCategory::Persistence,
                format!("target environment '{target}' was not found"),
            )
            .with_context("suggested_command", format!("workstate new {target}"))
            .with_context("action_id", action.id.to_string()));
        }
        Ok(target)
    }

    async fn observe_for_cleanup_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let target = self.validate_target_identity(action)?;
        if !self.backend.is_active(target)? {
            return Ok(ActionObservation::requires_change());
        }
        Ok(
            ActionObservation::already_correct().with_resources(vec![environment_record(
                action,
                target,
                OwnershipStatus::ReusedExisting,
                true,
            )?]),
        )
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let target = self.validate_target(action)?;
        if !self.backend.is_active(target)? {
            return Ok(ActionObservation::requires_change()
                .with_detail(format!("environment '{target}' is stopped")));
        }
        Ok(ActionObservation::already_correct()
            .with_detail(format!("environment '{target}' is already active"))
            .with_resources(vec![environment_record(
                action,
                target,
                OwnershipStatus::ReusedExisting,
                true,
            )?]))
    }

    async fn start_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> Result<(bool, ResourceRecord)> {
        cancellation.check()?;
        let target = self.validate_target(action)?.clone();
        output
            .emit(ActionOutput::log(format!(
                "Starting other environment '{target}'"
            )))
            .await?;
        let changed = self
            .backend
            .start(&target, cancellation, output.clone())
            .await?;
        output
            .emit(ActionOutput::log(if changed {
                format!("Other environment '{target}' is ready")
            } else {
                format!("Other environment '{target}' was already active")
            }))
            .await?;
        let ownership = if changed {
            OwnershipStatus::CreatedByCurrentRun
        } else {
            OwnershipStatus::ReusedExisting
        };
        Ok((
            changed,
            environment_record(action, &target, ownership, !changed)?,
        ))
    }

    async fn apply_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        let output = Arc::new(BufferedActionOutputSink::default());
        let (changed, resource) = self
            .start_inner(action, cancellation, output.clone())
            .await?;
        Ok(ActionExecutionResult {
            changed,
            resources: vec![resource],
            mutations: Vec::new(),
            outputs: output.snapshot()?,
        })
    }

    async fn run_once_with_output_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> Result<ActionExecutionResult> {
        let (changed, resource) = self.start_inner(action, cancellation, output).await?;
        Ok(ActionExecutionResult {
            changed,
            resources: vec![resource],
            mutations: Vec::new(),
            outputs: Vec::new(),
        })
    }

    async fn stop_inner(
        &self,
        action: &ActionSpec,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let target = self.validate_target_identity(action)?.clone();
        let tracked = resources.iter().any(|record| {
            record.resource.kind == ResourceKind::Environment
                && record.resource.stable_identity == target.as_str()
        });
        if !tracked {
            return Ok(CompensationResult::default());
        }
        let output = Arc::new(BufferedActionOutputSink::default());
        output
            .emit(ActionOutput::log(format!(
                "Stopping other environment '{target}'"
            )))
            .await?;
        self.backend
            .stop(&target, cancellation, output.clone())
            .await?;
        output
            .emit(ActionOutput::log(format!(
                "Other environment '{target}' stopped"
            )))
            .await?;
        Ok(CompensationResult {
            outputs: output.snapshot()?,
        })
    }
}

impl ActionHandler for StartOtherEnvironmentActionHandler {
    fn action_key(&self) -> &str {
        "start_other_environment"
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if action.kind != ActionKind::StartOtherEnvironment {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Start Other Environment handler received an incompatible action",
            ));
        }
        self.validate_target(action).map(|_| ())
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_inner(action, cancellation).await })
    }

    fn observe_for_cleanup<'a>(
        &'a self,
        action: &'a ActionSpec,
        _resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_for_cleanup_inner(action, cancellation).await })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move { self.apply_inner(action, cancellation).await })
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

    fn compensate<'a>(
        &'a self,
        action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move {
            self.stop_inner(action, &result.resources, cancellation)
                .await
        })
    }

    fn stop<'a>(
        &'a self,
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.stop_inner(action, resources, cancellation).await })
    }
}

#[derive(Default)]
struct BufferedActionOutputSink {
    outputs: Mutex<Vec<ActionOutput>>,
}

impl BufferedActionOutputSink {
    fn snapshot(&self) -> Result<Vec<ActionOutput>> {
        self.outputs
            .lock()
            .map(|outputs| outputs.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output buffer lock failed"))
    }
}

impl ActionOutputSink for BufferedActionOutputSink {
    fn emit<'a>(&'a self, output: ActionOutput) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.outputs
                .lock()
                .map(|mut outputs| outputs.push(output))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "output buffer lock failed")
                })
        })
    }
}

fn environment_record(
    action: &ActionSpec,
    environment: &EnvironmentSlug,
    ownership: OwnershipStatus,
    observed_before: bool,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::Environment, environment.to_string())
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(action.id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = action.cleanup_policy;
    record
        .integration_metadata
        .insert("environment_slug".to_owned(), environment.to_string());
    Ok(record)
}

pub fn register_handlers(
    registry: &mut ActionHandlerRegistry,
    backend: Arc<dyn EnvironmentLifecycleBackend>,
) -> Result<()> {
    registry.register(StartOtherEnvironmentActionHandler::new(backend))
}
