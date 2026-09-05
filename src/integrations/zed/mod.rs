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

pub use backend::{ProjectEditorKind, ZedBackend, ZedCommand, is_zed_application};
pub use errors::ZedError;

#[derive(Clone)]
pub struct ProjectEditorHandler {
    editor: Arc<ZedBackend>,
    desktop: Arc<dyn DesktopBackend>,
    action_kind: ActionKind,
    action_key: String,
}

impl ProjectEditorHandler {
    pub fn new(editor: Arc<ZedBackend>, desktop: Arc<dyn DesktopBackend>) -> Self {
        Self::for_editor(editor, desktop)
    }

    pub fn for_editor(editor: Arc<ZedBackend>, desktop: Arc<dyn DesktopBackend>) -> Self {
        let editor_kind = editor.editor_kind();
        let action_kind = match editor_kind {
            ProjectEditorKind::Zed => ActionKind::OpenProject,
            ProjectEditorKind::VsCode => ActionKind::OpenProjectWithVsCode,
            ProjectEditorKind::Cursor => ActionKind::OpenProjectWithCursor,
        };
        let action_key = action_kind.key();
        Self {
            editor,
            desktop,
            action_kind,
            action_key,
        }
    }

    fn editor_kind(&self) -> ProjectEditorKind {
        self.editor.editor_kind()
    }

    fn editor_name(&self) -> &'static str {
        self.editor_kind().display_name()
    }

    async fn workspace_is_satisfied(
        &self,
        action: &ActionSpec,
        window: &EditorWindowSnapshot,
    ) -> Result<bool> {
        let Some(target) = self.target_for(action) else {
            return Ok(true);
        };
        if matches!(&target, WorkspaceTarget::None) {
            return Ok(true);
        }
        let snapshot = self.desktop.snapshot().await?;
        let resolution = observed_destination(&snapshot, &target)?;
        Ok(resolution.workspace.is_some_and(|workspace| {
            window.workspace_identity.as_deref() == Some(workspace.identity.as_str())
        }))
    }

    fn target_for(&self, action: &ActionSpec) -> Option<WorkspaceTarget> {
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
                format!(
                    "the {} action does not contain a project path",
                    self.editor_name()
                ),
            )
        })?;
        let project_path = self
            .editor
            .resolve_project_path(std::path::Path::new(project_path))?;
        let projects = self.editor.observe_projects().await?;
        let window = match self
            .editor
            .matching_project_window(&projects, &project_path)?
        {
            Some(window) => window,
            None => {
                let persisted_matches = matching_persisted_projects(
                    &projects,
                    &project_path,
                    previous_resources,
                    self.editor.as_ref(),
                );
                match persisted_matches.as_slice() {
                    [window] => window.clone(),
                    [] => return Ok(ActionObservation::requires_change()),
                    _ => {
                        return Err(WorkstateError::new(
                            ErrorCategory::Integration,
                            format!(
                                "more than one {} window owns the configured project identity",
                                self.editor_name()
                            ),
                        )
                        .with_context("project_path", project_path.display().to_string())
                        .with_context("matches", persisted_matches.len().to_string()));
                    }
                }
            }
        };
        let record = window_record(
            action,
            &window,
            OwnershipStatus::ReusedExisting,
            true,
            Some(&project_path),
        )?;
        if !self.workspace_is_satisfied(action, &window).await? {
            return Ok(ActionObservation::requires_change()
                .with_detail(format!(
                    "the {} project window is not in the requested workspace",
                    self.editor_name()
                ))
                .with_resources(vec![record]));
        }
        Ok(ActionObservation::already_correct().with_resources(vec![record]))
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
                && !self.editor_kind().matches_application(application)
            {
                return Ok(ActionObservation::unknown(format!(
                    "persisted {} window '{}' now identifies application '{}'",
                    self.editor_name(),
                    resource.resource.stable_identity,
                    application
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
                format!(
                    "the {} action does not contain a project path",
                    self.editor_name()
                ),
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
        let mut outputs = vec![ActionOutput::log(format!(
            "inspected {} windows before opening project",
            self.editor_name()
        ))];
        outputs.push(ActionOutput::log(if outcome.owned {
            format!(
                "launched {} for '{}'",
                self.editor_name(),
                project_path.display()
            )
        } else {
            format!(
                "reused {} for '{}'",
                self.editor_name(),
                project_path.display()
            )
        }));
        if outcome.owned {
            outputs.push(ActionOutput::log(format!(
                "waited for the launched {} project window to become observable",
                self.editor_name()
            )));
        }

        if let Some(target) = target {
            let target_label = workspace_target_label(&target);
            outputs.push(ActionOutput::log(format!(
                "resolved {} destination workspace '{target_label}'",
                self.editor_name()
            )));
            match move_window_with_retry(
                self.desktop.as_ref(),
                &window.identity,
                target,
                cancellation.clone(),
                action_timeout(action),
                self.editor_name(),
            )
            .await
            {
                Ok(Some((previous, destination))) => {
                    changed = true;
                    outputs.push(ActionOutput::log(format!(
                        "moved {} window to desktop workspace '{destination}'",
                        self.editor_name()
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
                    "closed owned {} window '{}'",
                    self.editor_name(),
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
                self.editor_name(),
            )
            .await?;
            outputs.push(ActionOutput::log(format!(
                "closed owned {} window '{}'",
                self.editor_name(),
                resource.resource.stable_identity
            )));
        }
        Ok(CompensationResult { outputs })
    }
}

impl ActionHandler for ProjectEditorHandler {
    fn action_key(&self) -> &str {
        &self.action_key
    }

    fn required_capabilities(&self) -> std::collections::BTreeSet<CapabilityId> {
        let editor_capability = match self.editor_kind() {
            ProjectEditorKind::Zed => CapabilityId::Zed,
            ProjectEditorKind::VsCode => CapabilityId::VsCode,
            ProjectEditorKind::Cursor => CapabilityId::Cursor,
        };
        [CapabilityId::DesktopWindows, editor_capability]
            .into_iter()
            .collect()
    }

    fn requires_workspace_target_for_observation(&self, _action: &ActionSpec) -> bool {
        true
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if action.kind != self.action_kind {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "the {} project handler received an incompatible action",
                    self.editor_name()
                ),
            ));
        }
        let application = action.parameters.application.as_deref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "the {} action does not contain an application identity",
                    self.editor_name()
                ),
            )
        })?;
        if !self.editor_kind().matches_application(application) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "the open-project action application is not {}",
                    self.editor_name()
                ),
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
    registry.register(ProjectEditorHandler::new(editor, desktop))?;
    Ok(())
}

pub fn register_editor_handler(
    registry: &mut ActionHandlerRegistry,
    editor: Arc<ZedBackend>,
    desktop: Arc<dyn DesktopBackend>,
) -> Result<()> {
    registry.register(ProjectEditorHandler::for_editor(editor, desktop))?;
    Ok(())
}

pub type ZedProjectHandler = ProjectEditorHandler;

fn matching_persisted_projects(
    windows: &[EditorWindowSnapshot],
    project_path: &Path,
    previous_resources: &[ResourceRecord],
    editor: &ZedBackend,
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
                .is_none_or(|_| editor.project_path_matches(window, project_path))
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
    editor_name: &str,
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
                format!("the {editor_name} window disappeared before it could be moved"),
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
                        format!("the {editor_name} window disappeared after it was moved"),
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
                    format!(
                        "the desktop backend did not confirm the {editor_name} window movement"
                    ),
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
        format!("the {editor_name} window could not be moved after a safe retry"),
    ))
}

async fn wait_for_window_absence(
    desktop: &dyn DesktopBackend,
    window_identity: &str,
    cancellation: CancellationToken,
    timeout: Duration,
    editor_name: &str,
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
                editor: editor_name.to_owned(),
                window: window_identity.to_owned(),
            }
            .into_workstate());
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!(
                        "operation was cancelled while verifying the {editor_name} window close"
                    ),
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
