pub mod backend;
pub mod errors;
pub mod models;
pub(crate) mod wayland;

use std::{sync::Arc, time::Duration};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, CancellationToken, CompensationResult,
        },
        ports::{
            BoxFuture, DesktopBackend, DesktopOperationStatus, DesktopSnapshot,
            DesktopWorkspaceResolution, ensure_workspace, resolve_workspace_target,
        },
        timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    },
    domain::{
        ActionKind, ActionSpec, CompensationOperation, MutationRecord, OwnershipStatus,
        ResourceIdentity, ResourceKind, ResourceRecord, TilingPreference, WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    platform::CapabilityId,
};

pub use backend::CosmicBackend;
pub use errors::CosmicError;
pub use wayland::CosmicWaylandCoordinator;

#[derive(Clone)]
pub struct WorkspaceHandler {
    backend: Arc<dyn DesktopBackend>,
}

impl WorkspaceHandler {
    pub fn tiling(backend: Arc<dyn DesktopBackend>) -> Self {
        Self { backend }
    }

    fn target_for(&self, action: &ActionSpec) -> Result<WorkspaceTarget> {
        action
            .resolved_workspace_target
            .clone()
            .or_else(|| {
                action
                    .parameters
                    .workspace_id
                    .as_ref()
                    .map(|id| WorkspaceTarget::Existing {
                        reference: crate::domain::WorkspaceReference::Identifier(id.to_string()),
                    })
            })
            .or_else(|| {
                action
                    .desktop_workspace
                    .as_ref()
                    .map(|id| WorkspaceTarget::Existing {
                        reference: crate::domain::WorkspaceReference::Identifier(id.to_string()),
                    })
            })
            .ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Integration,
                    "the action does not contain a resolved desktop workspace target",
                )
            })
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let target = self.target_for(action)?;
        if action
            .resolved_tiling
            .unwrap_or(TilingPreference::Unchanged)
            == TilingPreference::Unchanged
        {
            return Ok(ActionObservation::already_correct());
        }
        let snapshot = self.backend.snapshot().await?;
        let resolution = observed_workspace(&snapshot, &target)?;
        let Some(workspace) = resolution.workspace else {
            return Ok(ActionObservation::requires_change());
        };
        if !tiling_matches(&workspace, action.resolved_tiling.unwrap_or_default())? {
            return Ok(ActionObservation::requires_change().with_resources(vec![
                workspace_record(action, &workspace, OwnershipStatus::ReusedExisting, true)?,
            ]));
        }
        Ok(
            ActionObservation::already_correct().with_resources(vec![workspace_record(
                action,
                &workspace,
                OwnershipStatus::ReusedExisting,
                true,
            )?]),
        )
    }

    async fn apply_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let target = self.target_for(action)?;
        let resolution = ensure_workspace(
            self.backend.as_ref(),
            target,
            cancellation.clone(),
            action_timeout(action),
        )
        .await?;
        let Some(workspace) = resolution.workspace else {
            return Ok(ActionExecutionResult::default());
        };
        let desired = action.resolved_tiling.unwrap_or_default();
        if desired == TilingPreference::Unchanged {
            return Ok(ActionExecutionResult::default());
        }
        let Some(current) = workspace.tiling_enabled else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "tiling state is unavailable for desktop workspace '{}'",
                    workspace.identity
                ),
            ));
        };
        let enabled = desired == TilingPreference::Enabled;
        let created = resolution.status == DesktopOperationStatus::Created;
        let ownership = if created {
            OwnershipStatus::CreatedByCurrentRun
        } else {
            OwnershipStatus::ReusedExisting
        };
        let record = workspace_record(action, &workspace, ownership, !created)?;
        if current == enabled {
            return Ok(ActionExecutionResult {
                changed: false,
                resources: vec![record],
                mutations: Vec::new(),
                outputs: vec![
                    ActionOutput::log("inspected desktop workspaces and windows"),
                    ActionOutput::log(format!(
                        "resolved desktop workspace '{}'",
                        workspace.identity
                    )),
                    ActionOutput::log(format!(
                        "desktop workspace '{}' already has tiling {}",
                        workspace.identity,
                        if enabled { "enabled" } else { "disabled" }
                    )),
                ],
            });
        }
        self.backend
            .set_tiling(&workspace.identity, enabled)
            .await?;
        let refreshed = self.backend.snapshot().await?;
        let Some(updated_workspace) = refreshed.workspace(&workspace.identity) else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "desktop workspace '{}' disappeared after its tiling change",
                    workspace.identity
                ),
            ));
        };
        if updated_workspace.tiling_enabled != Some(enabled) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "desktop workspace '{}' did not confirm tiling {}",
                    workspace.identity,
                    if enabled { "enabled" } else { "disabled" }
                ),
            ));
        }
        let resource = record.resource.clone();
        let mut mutation =
            MutationRecord::new(format!("desktop.workspace.{}.tiling", workspace.identity))
                .map_err(WorkstateError::from)?;
        mutation.action_id = Some(action.id.clone());
        mutation.resource = Some(resource);
        mutation.previous_value = Some(current.to_string());
        mutation.applied_value = Some(enabled.to_string());
        mutation.ownership = OwnershipStatus::CreatedByCurrentRun;
        mutation.compensation = CompensationOperation::Handler;
        mutation.cleanup_policy = action.cleanup_policy;
        Ok(ActionExecutionResult {
            changed: true,
            resources: vec![record],
            mutations: vec![mutation],
            outputs: vec![
                ActionOutput::log("inspected desktop workspaces and windows"),
                ActionOutput::log(format!(
                    "resolved desktop workspace '{}'",
                    workspace.identity
                )),
                ActionOutput::log(format!(
                    "set desktop workspace '{}' tiling to {}",
                    workspace.identity,
                    if enabled { "enabled" } else { "disabled" }
                )),
            ],
        })
    }

    async fn compensate_inner(
        &self,
        result: &ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let mut outputs = Vec::new();
        for mutation in &result.mutations {
            if !mutation.target.starts_with("desktop.workspace.")
                || mutation.compensation == CompensationOperation::None
            {
                continue;
            }
            let Some(previous) = mutation.previous_value.as_deref().and_then(parse_bool) else {
                outputs.push(ActionOutput::log(format!(
                    "preserved desktop mutation '{}' because its previous value is unavailable",
                    mutation.target
                )));
                continue;
            };
            let Some(resource) = &mutation.resource else {
                outputs.push(ActionOutput::log(format!(
                    "preserved desktop mutation '{}' because its workspace identity is unavailable",
                    mutation.target
                )));
                continue;
            };
            let snapshot = self.backend.snapshot().await?;
            let Some(workspace) = snapshot.workspace(&resource.stable_identity) else {
                outputs.push(ActionOutput::log(format!(
                    "preserved desktop workspace '{}' because it no longer exists",
                    resource.stable_identity
                )));
                continue;
            };
            if let Some(applied) = mutation.applied_value.as_deref().and_then(parse_bool)
                && workspace
                    .tiling_enabled
                    .is_some_and(|value| value != applied)
            {
                outputs.push(ActionOutput::log(format!(
                    "preserved desktop workspace '{}' because its tiling was changed after Workstate",
                    workspace.identity
                )));
                continue;
            }
            self.backend
                .restore_tiling(&workspace.identity, previous)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "restored desktop workspace '{}' tiling",
                workspace.identity
            )));
        }
        for resource in &result.resources {
            if resource.resource.kind == ResourceKind::DesktopWorkspace
                && resource.ownership.is_environment_owned()
            {
                self.backend
                    .delete_workspace(&resource.resource.stable_identity)
                    .await?;
                outputs.push(ActionOutput::log(format!(
                    "removed desktop workspace '{}'",
                    resource.resource.stable_identity
                )));
            }
        }
        Ok(CompensationResult { outputs })
    }

    async fn stop_inner(
        &self,
        result: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let mut outputs = Vec::new();
        for resource in result {
            if resource.resource.kind != ResourceKind::DesktopWorkspace
                || !resource.ownership.is_environment_owned()
            {
                continue;
            }
            self.backend
                .delete_workspace(&resource.resource.stable_identity)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "removed desktop workspace '{}'",
                resource.resource.stable_identity
            )));
        }
        Ok(CompensationResult { outputs })
    }
}

impl ActionHandler for WorkspaceHandler {
    fn action_key(&self) -> &str {
        "configure_tiling"
    }

    fn required_capabilities(&self) -> std::collections::BTreeSet<CapabilityId> {
        [CapabilityId::DesktopTiling].into_iter().collect()
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if action.kind != ActionKind::ConfigureTiling {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "workspace handler cannot execute action kind '{}'",
                    action.kind.key()
                ),
            ));
        }
        Ok(())
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_inner(action, cancellation).await })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move { self.apply_inner(action, cancellation).await })
    }

    fn compensate<'a>(
        &'a self,
        _action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.compensate_inner(result, cancellation).await })
    }

    fn stop<'a>(
        &'a self,
        _action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.stop_inner(resources, cancellation).await })
    }
}

pub fn register_handlers(
    registry: &mut ActionHandlerRegistry,
    backend: Arc<dyn DesktopBackend>,
) -> Result<()> {
    registry.register(WorkspaceHandler::tiling(backend))?;
    Ok(())
}

fn observed_workspace(
    snapshot: &DesktopSnapshot,
    target: &WorkspaceTarget,
) -> Result<DesktopWorkspaceResolution> {
    if let WorkspaceTarget::Create { name } = target {
        let matches = snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.name.as_deref() == Some(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [workspace] => Ok(DesktopWorkspaceResolution::existing(workspace.clone())),
            [] => Ok(DesktopWorkspaceResolution::none()),
            _ => Err(WorkstateError::new(
                ErrorCategory::Platform,
                "the requested desktop workspace name is ambiguous",
            )
            .with_context("name", name.clone())
            .with_context("matches", matches.len().to_string())),
        };
    }
    resolve_workspace_target(snapshot, target)
}

fn workspace_record(
    action: &ActionSpec,
    workspace: &crate::application::ports::DesktopWorkspaceSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
) -> Result<ResourceRecord> {
    let identity =
        ResourceIdentity::new(ResourceKind::DesktopWorkspace, workspace.identity.clone())
            .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(action.id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = action.cleanup_policy;
    record.integration_metadata.insert(
        "workspace_name".to_owned(),
        workspace.name.clone().unwrap_or_default(),
    );
    Ok(record)
}

fn tiling_matches(
    workspace: &crate::application::ports::DesktopWorkspaceSnapshot,
    preference: TilingPreference,
) -> Result<bool> {
    match preference {
        TilingPreference::Unchanged => Ok(true),
        TilingPreference::Enabled => workspace.tiling_enabled.ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "tiling state is unavailable for desktop workspace '{}'",
                    workspace.identity
                ),
            )
        }),
        TilingPreference::Disabled => {
            workspace.tiling_enabled.map(|value| !value).ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Integration,
                    format!(
                        "tiling state is unavailable for desktop workspace '{}'",
                        workspace.identity
                    ),
                )
            })
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn action_timeout(action: &ActionSpec) -> Duration {
    action
        .timeout
        .as_ref()
        .map(|timeout| Duration::from_millis(timeout.milliseconds))
        .unwrap_or(DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
}
