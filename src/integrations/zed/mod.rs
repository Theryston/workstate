pub mod backend;
pub mod errors;

use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, CancellationToken, CompensationResult,
        },
        ports::{
            BoxFuture, DesktopBackend, DesktopSnapshot, DesktopWorkspaceResolution, EditorBackend,
            EditorWindowSnapshot, ensure_workspace, resolve_workspace_target,
        },
        timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    },
    domain::{
        ActionKind, ActionSpec, CompensationOperation, MutationRecord, OwnershipStatus,
        ResourceIdentity, ResourceKind, ResourceRecord, WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    platform::CapabilityId,
};

pub use backend::{ZedBackend, ZedCommand, is_zed_application};
pub use errors::ZedError;

#[derive(Clone)]
pub struct ZedProjectHandler {
    editor: Arc<ZedBackend>,
    desktop: Arc<dyn DesktopBackend>,
}

impl ZedProjectHandler {
    pub fn new(editor: Arc<ZedBackend>, desktop: Arc<dyn DesktopBackend>) -> Self {
        Self { editor, desktop }
    }

    fn target_for(&self, action: &ActionSpec) -> Option<WorkspaceTarget> {
        action.resolved_workspace_target.clone()
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        previous_resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let project_path = action.parameters.project_path.as_deref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "the Zed action does not contain a project path",
            )
        })?;
        let project_path = self
            .editor
            .resolve_project_path(std::path::Path::new(project_path))?;
        let projects = self.editor.observe_projects().await?;
        let matches = matching_projects(&projects, &project_path);
        if matches.len() > 1 {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "more than one Zed window owns the configured project identity",
            )
            .with_context("project_path", project_path.display().to_string())
            .with_context("matches", matches.len().to_string()));
        }
        let window = match matches.as_slice() {
            [window] => (*window).clone(),
            [] => {
                let persisted_matches =
                    matching_persisted_projects(&projects, &project_path, previous_resources);
                match persisted_matches.as_slice() {
                    [window] => (*window).clone(),
                    [] => return Ok(ActionObservation::requires_change()),
                    _ => {
                        return Err(WorkstateError::new(
                            ErrorCategory::Integration,
                            "more than one Zed window owns the configured project identity",
                        )
                        .with_context("project_path", project_path.display().to_string())
                        .with_context("matches", persisted_matches.len().to_string()));
                    }
                }
            }
            _ => {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "more than one Zed window owns the configured project identity",
                )
                .with_context("project_path", project_path.display().to_string())
                .with_context("matches", matches.len().to_string()));
            }
        };
        Ok(
            ActionObservation::already_correct().with_resources(vec![window_record(
                action,
                &window,
                OwnershipStatus::ReusedExisting,
                true,
                Some(&project_path),
            )?]),
        )
    }

    async fn observe_for_cleanup_inner(
        &self,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let snapshot = self.desktop.snapshot().await?;
        let mut observed = Vec::new();
        for resource in resources {
            if resource.resource.kind != ResourceKind::DesktopWindow {
                continue;
            }
            let Some(window) = snapshot.window(&resource.resource.stable_identity) else {
                continue;
            };
            if let Some(application) = window.application.as_deref()
                && !is_zed_application(application)
            {
                return Ok(ActionObservation::unknown(format!(
                    "persisted Zed window '{}' now identifies application '{}'",
                    resource.resource.stable_identity, application
                )));
            }
            observed.push(resource.clone());
        }
        Ok(ActionObservation::already_correct().with_resources(observed))
    }

    async fn apply_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let project_path = action.parameters.project_path.as_deref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "the Zed action does not contain a project path",
            )
        })?;
        let project_path = self
            .editor
            .resolve_project_path(std::path::Path::new(project_path))?;
        let target = match self.target_for(action) {
            Some(WorkspaceTarget::None) => Some(WorkspaceTarget::None),
            Some(target) => Some(self.anchor_workspace_target(target).await?),
            None => None,
        };
        let outcome = self
            .editor
            .open_project(project_path.clone(), cancellation.clone())
            .await?;
        let mut window = outcome.window.clone();
        let ownership = if outcome.owned {
            OwnershipStatus::CreatedByCurrentRun
        } else {
            OwnershipStatus::ReusedExisting
        };
        let mut record = window_record(
            action,
            &window,
            ownership,
            !outcome.owned,
            Some(&project_path),
        )?;
        let mut mutations = Vec::new();
        let mut changed =
            outcome.status == crate::application::ports::EditorOperationStatus::Launched;
        let mut outputs = vec![ActionOutput::log(
            "inspected Zed windows before opening project",
        )];
        outputs.push(ActionOutput::log(if outcome.owned {
            format!("launched Zed for '{}'", project_path.display())
        } else {
            format!("reused Zed for '{}'", project_path.display())
        }));
        if outcome.owned {
            outputs.push(ActionOutput::log(
                "waited for the launched Zed project window to become observable",
            ));
        }

        if let Some(target) = target {
            let target_label = workspace_target_label(&target);
            outputs.push(ActionOutput::log(format!(
                "resolved Zed destination workspace '{target_label}'"
            )));
            match move_window_with_retry(
                self.desktop.as_ref(),
                &window.identity,
                target,
                cancellation.clone(),
                action_timeout(action),
            )
            .await
            {
                Ok(Some((previous, destination))) => {
                    changed = true;
                    outputs.push(ActionOutput::log(format!(
                        "moved Zed window to desktop workspace '{destination}'"
                    )));
                    window.workspace_identity = Some(destination.clone());
                    record
                        .integration_metadata
                        .insert("workspace_identity".to_owned(), destination.clone());
                    if let Some(previous) = previous {
                        let resource = record.resource.clone();
                        let mut mutation = MutationRecord::new(format!(
                            "desktop.window.{}.workspace",
                            window.identity
                        ))
                        .map_err(WorkstateError::from)?;
                        mutation.action_id = Some(action.id.clone());
                        mutation.resource = Some(resource);
                        mutation.previous_value = Some(previous.clone());
                        mutation.applied_value = Some(destination.clone());
                        mutation.ownership = OwnershipStatus::CreatedByCurrentRun;
                        mutation.compensation = CompensationOperation::Handler;
                        mutation.cleanup_policy = action.cleanup_policy;
                        mutations.push(mutation);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    if outcome.owned {
                        let cleanup = self.editor.close_window(&window.identity).await;
                        if let Err(cleanup_error) = cleanup {
                            return Err(error
                                .with_context("launched_window_cleanup", cleanup_error.render()));
                        }
                    }
                    return Err(error);
                }
            }
        }

        Ok(ActionExecutionResult {
            changed,
            resources: vec![record],
            mutations,
            outputs,
        })
    }

    async fn anchor_workspace_target(&self, target: WorkspaceTarget) -> Result<WorkspaceTarget> {
        let snapshot = self.desktop.snapshot().await?;
        let destination = observed_destination(&snapshot, &target)?;
        Ok(destination
            .workspace
            .map_or(target, |workspace| WorkspaceTarget::Existing {
                reference: crate::domain::WorkspaceReference::Identifier(workspace.identity),
            }))
    }

    async fn compensate_inner(
        &self,
        result: &ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let mut outputs = Vec::new();
        for mutation in &result.mutations {
            if mutation.compensation == CompensationOperation::None
                || !mutation.target.starts_with("desktop.window.")
            {
                continue;
            }
            let Some(resource) = &mutation.resource else {
                outputs.push(ActionOutput::log(format!(
                    "preserved window mutation '{}' because its window identity is unavailable",
                    mutation.target
                )));
                continue;
            };
            let snapshot = self.desktop.snapshot().await?;
            let Some(window) = snapshot.window(&resource.stable_identity) else {
                outputs.push(ActionOutput::log(format!(
                    "preserved window '{}' because it no longer exists",
                    resource.stable_identity
                )));
                continue;
            };
            if mutation
                .applied_value
                .as_deref()
                .is_some_and(|applied| window.workspace_identity.as_deref() != Some(applied))
            {
                outputs.push(ActionOutput::log(format!(
                    "preserved window '{}' because its workspace changed after Workstate",
                    resource.stable_identity
                )));
                continue;
            }
            let Some(previous) = mutation.previous_value.as_deref() else {
                outputs.push(ActionOutput::log(format!(
                    "preserved window '{}' because its previous workspace is unavailable",
                    resource.stable_identity
                )));
                continue;
            };
            if snapshot.workspace(previous).is_none() {
                outputs.push(ActionOutput::log(format!(
                    "preserved window '{}' because its previous workspace no longer exists",
                    resource.stable_identity
                )));
                continue;
            }
            self.desktop
                .move_window(&resource.stable_identity, previous)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "restored window '{}' to desktop workspace '{}'",
                resource.stable_identity, previous
            )));
        }

        for resource in &result.resources {
            if resource.resource.kind != ResourceKind::DesktopWindow
                || !resource.ownership.is_environment_owned()
            {
                continue;
            }
            if self
                .desktop
                .snapshot()
                .await?
                .window(&resource.resource.stable_identity)
                .is_some()
            {
                self.editor
                    .close_window(&resource.resource.stable_identity)
                    .await?;
                outputs.push(ActionOutput::log(format!(
                    "closed owned Zed window '{}'",
                    resource.resource.stable_identity
                )));
            }
        }
        Ok(CompensationResult { outputs })
    }

    async fn stop_inner(
        &self,
        action: &ActionSpec,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let mut outputs = Vec::new();
        let snapshot = self.desktop.snapshot().await?;
        for resource in resources {
            if resource.resource.kind != ResourceKind::DesktopWindow
                || !resource.ownership.is_environment_owned()
                || snapshot
                    .window(&resource.resource.stable_identity)
                    .is_none()
            {
                continue;
            }
            self.editor
                .close_window(&resource.resource.stable_identity)
                .await?;
            wait_for_window_absence(
                self.desktop.as_ref(),
                &resource.resource.stable_identity,
                cancellation.clone(),
                action_timeout(action),
            )
            .await?;
            outputs.push(ActionOutput::log(format!(
                "closed owned Zed window '{}'",
                resource.resource.stable_identity
            )));
        }
        Ok(CompensationResult { outputs })
    }
}

impl ActionHandler for ZedProjectHandler {
    fn action_key(&self) -> &str {
        "open_project"
    }

    fn required_capabilities(&self) -> std::collections::BTreeSet<CapabilityId> {
        [CapabilityId::DesktopWindows, CapabilityId::Zed]
            .into_iter()
            .collect()
    }

    fn requires_workspace_target_for_observation(&self, _action: &ActionSpec) -> bool {
        false
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if action.kind != ActionKind::OpenProject {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the Zed project handler received an incompatible action",
            ));
        }
        let application = action.parameters.application.as_deref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "the Zed action does not contain an application identity",
            )
        })?;
        if !is_zed_application(application) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the open-project action application is not Zed",
            )
            .with_context("application", application));
        }
        Ok(())
    }

    fn observe<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move { self.observe_inner(action, &[], cancellation).await })
    }

    fn observe_with_resources<'a>(
        &'a self,
        action: &'a ActionSpec,
        previous_resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            self.observe_inner(action, previous_resources, cancellation)
                .await
        })
    }

    fn observe_for_cleanup<'a>(
        &'a self,
        _action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            self.observe_for_cleanup_inner(resources, cancellation)
                .await
        })
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
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.stop_inner(action, resources, cancellation).await })
    }
}

pub fn register_handlers(
    registry: &mut ActionHandlerRegistry,
    editor: Arc<ZedBackend>,
    desktop: Arc<dyn DesktopBackend>,
) -> Result<()> {
    registry.register(ZedProjectHandler::new(editor, desktop))?;
    Ok(())
}

fn matching_projects(
    windows: &[EditorWindowSnapshot],
    project_path: &Path,
) -> Vec<EditorWindowSnapshot> {
    windows
        .iter()
        .filter(|window| window.project_path.as_deref() == Some(project_path))
        .cloned()
        .collect()
}

fn matching_persisted_projects(
    windows: &[EditorWindowSnapshot],
    project_path: &Path,
    previous_resources: &[ResourceRecord],
) -> Vec<EditorWindowSnapshot> {
    let project_key = project_path.display().to_string();
    let identities = previous_resources
        .iter()
        .filter(|record| record.resource.kind == ResourceKind::DesktopWindow)
        .filter(|record| {
            record
                .integration_metadata
                .get("project_path")
                .is_some_and(|value| value == &project_key)
        })
        .map(|record| record.resource.stable_identity.as_str())
        .collect::<BTreeSet<_>>();
    windows
        .iter()
        .filter(|window| identities.contains(window.identity.as_str()))
        .filter(|window| {
            window
                .project_path
                .as_deref()
                .is_none_or(|observed| observed == project_path)
        })
        .cloned()
        .collect()
}

fn workspace_target_label(target: &WorkspaceTarget) -> String {
    match target {
        WorkspaceTarget::Existing { reference } => match reference {
            crate::domain::WorkspaceReference::Name(name) => format!("name:{name}"),
            crate::domain::WorkspaceReference::Identifier(identity) => identity.clone(),
        },
        WorkspaceTarget::Current => "current".to_owned(),
        WorkspaceTarget::NextEmpty => "next-empty".to_owned(),
        WorkspaceTarget::Create { name } => format!("create:{name}"),
        WorkspaceTarget::None => "none".to_owned(),
    }
}

fn observed_destination(
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

async fn move_window_with_retry(
    desktop: &dyn DesktopBackend,
    window_identity: &str,
    target: WorkspaceTarget,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<Option<(Option<String>, String)>> {
    let mut resolution =
        ensure_workspace(desktop, target.clone(), cancellation.clone(), timeout).await?;
    let Some(mut workspace) = resolution.workspace.take() else {
        return Ok(None);
    };
    let mut snapshot = desktop.snapshot().await?;
    for attempt in 0..2 {
        cancellation.check()?;
        let Some(window) = snapshot.window(window_identity) else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the Zed window disappeared before it could be moved",
            )
            .with_context("window_identity", window_identity));
        };
        if window.workspace_identity.as_deref() == Some(workspace.identity.as_str()) {
            return Ok(None);
        }
        let previous = window.workspace_identity.clone();
        match desktop
            .move_window(window_identity, &workspace.identity)
            .await
        {
            Ok(_) => {
                let refreshed = desktop.snapshot().await?;
                let Some(updated_window) = refreshed.window(window_identity) else {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "the Zed window disappeared after it was moved",
                    )
                    .with_context("window_identity", window_identity));
                };
                if updated_window.workspace_identity.as_deref() == Some(workspace.identity.as_str())
                {
                    return Ok(Some((previous, workspace.identity.clone())));
                }
                if attempt == 0 {
                    snapshot = refreshed;
                    continue;
                }
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "the desktop backend did not confirm the Zed window movement",
                )
                .with_context("window_identity", window_identity)
                .with_context("workspace_identity", workspace.identity.clone()));
            }
            Err(error) if attempt == 0 => {
                resolution =
                    ensure_workspace(desktop, target.clone(), cancellation.clone(), timeout)
                        .await?;
                let Some(refreshed_workspace) = resolution.workspace.take() else {
                    return Ok(None);
                };
                snapshot = desktop.snapshot().await?;
                workspace = refreshed_workspace;
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }
    Err(WorkstateError::new(
        ErrorCategory::Integration,
        "the Zed window could not be moved after a safe retry",
    ))
}

async fn wait_for_window_absence(
    desktop: &dyn DesktopBackend,
    window_identity: &str,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        cancellation.check()?;
        if desktop.snapshot().await?.window(window_identity).is_none() {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(ZedError::WindowCloseTimeout {
                window: window_identity.to_owned(),
            }
            .into_workstate());
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    "operation was cancelled while verifying the Zed window close",
                ));
            }
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(25))) => {}
        }
    }
}

fn window_record(
    action: &ActionSpec,
    window: &EditorWindowSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
    project_path: Option<&Path>,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::DesktopWindow, window.identity.clone())
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(action.id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = action.cleanup_policy;
    if let Some(project_path) = project_path {
        record.integration_metadata.insert(
            "project_path".to_owned(),
            project_path.display().to_string(),
        );
    }
    record
        .integration_metadata
        .insert("application".to_owned(), window.application.clone());
    if let Some(title) = &window.title {
        record
            .integration_metadata
            .insert("title".to_owned(), title.clone());
    }
    if let Some(workspace) = &window.workspace_identity {
        record
            .integration_metadata
            .insert("workspace_identity".to_owned(), workspace.clone());
    }
    Ok(record)
}

fn action_timeout(action: &ActionSpec) -> Duration {
    action
        .timeout
        .as_ref()
        .map(|timeout| Duration::from_millis(timeout.milliseconds))
        .unwrap_or(DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
}
