use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, CancellationToken, CompensationResult,
        },
        ports::{
            ApplicationCatalog, BoxFuture, DesktopBackend, DesktopOperationStatus, DesktopSnapshot,
            DesktopWindowSnapshot, FileSystem, ensure_workspace,
        },
        timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    },
    domain::{
        ActionKind, ActionSpec, CompensationOperation, MutationRecord, OwnershipStatus,
        ResourceIdentity, ResourceKind, ResourceRecord, WorkspaceReference, WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::filesystem::PathResolver,
    platform::CapabilityId,
};

#[derive(Clone)]
pub struct ApplicationActionHandler {
    catalog: Arc<dyn ApplicationCatalog>,
    desktop: Arc<dyn DesktopBackend>,
    file_system: Arc<dyn FileSystem>,
    launch_lock: Arc<tokio::sync::Mutex<()>>,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl ApplicationActionHandler {
    pub fn new(
        catalog: Arc<dyn ApplicationCatalog>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
    ) -> Self {
        Self {
            catalog,
            desktop,
            file_system,
            launch_lock: Arc::new(tokio::sync::Mutex::new(())),
            poll_interval: Duration::from_millis(25),
            poll_timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
        }
    }

    pub fn with_timing(mut self, poll_interval: Duration, poll_timeout: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.poll_timeout = poll_timeout;
        self
    }

    fn application_id<'a>(&self, action: &'a ActionSpec) -> Result<&'a str> {
        action.parameters.application.as_deref().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!(
                    "application action '{}' is missing its application",
                    action.id
                ),
            )
        })
    }

    fn action_matches(&self, action: &ActionSpec) -> bool {
        action.kind == ActionKind::OpenApplication
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
                        reference: WorkspaceReference::Identifier(id.to_string()),
                    })
            })
            .or_else(|| {
                action
                    .desktop_workspace
                    .as_ref()
                    .map(|id| WorkspaceTarget::Existing {
                        reference: WorkspaceReference::Identifier(id.to_string()),
                    })
            })
    }

    fn resolve_working_directory(&self, configured: Option<&str>) -> Result<PathBuf> {
        let home = self.file_system.home_directory().map_err(|error| {
            error.with_context("operation", "resolve application working directory")
        })?;
        let resolver = PathResolver::new(home.clone(), self.file_system.as_ref())?;
        match configured {
            Some(value) => resolver.canonicalize_for_execution(value).map_err(|error| {
                error.with_context("operation", "resolve application working directory")
            }),
            None => {
                let exists = self.file_system.exists(&home)?;
                if !exists || !self.file_system.is_directory(&home)? {
                    return Err(WorkstateError::new(
                        ErrorCategory::Process,
                        "the default application working directory is not a directory",
                    )
                    .with_context("working_directory", home.display().to_string()));
                }
                self.file_system.canonicalize(&home).map_err(|error| {
                    error.with_context("operation", "resolve application working directory")
                })
            }
        }
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        cancellation.check()?;
        let application_id = self.application_id(action)?;
        let snapshot = self.desktop.snapshot().await?;
        let windows = matching_windows(&snapshot, application_id);
        if windows.is_empty() {
            return Ok(ActionObservation::requires_change()
                .with_detail("the application window is not open"));
        }
        let resources = windows
            .iter()
            .map(|window| {
                window_record(
                    action,
                    window,
                    OwnershipStatus::ReusedExisting,
                    true,
                    application_id,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ActionObservation::already_correct().with_resources(resources))
    }

    async fn apply_inner(
        &self,
        action: &ActionSpec,
        cancellation: CancellationToken,
    ) -> Result<ActionExecutionResult> {
        cancellation.check()?;
        let application_id = self.application_id(action)?;
        let launch = self.catalog.launch_spec(application_id)?;
        let working_directory =
            self.resolve_working_directory(action.working_directory.as_deref())?;
        let request = launch.process_request(
            action.parameters.application_arguments.clone(),
            Some(working_directory),
        );

        let _launch_guard = self.launch_lock.lock().await;
        let before = self.desktop.snapshot().await?;
        let existing = matching_windows(&before, application_id);
        if !existing.is_empty() {
            return self.reused_result(action, application_id, &existing);
        }

        let launch_outcome = self.desktop.open_application(request).await?;
        let process_identity = match launch_outcome.status {
            DesktopOperationStatus::Created | DesktopOperationStatus::Changed => {
                launch_outcome.identity
            }
            DesktopOperationStatus::AlreadyPresent
            | DesktopOperationStatus::Reused
            | DesktopOperationStatus::Unchanged
            | DesktopOperationStatus::Unavailable
            | DesktopOperationStatus::Ambiguous => None,
        };
        let before_ids = before
            .windows
            .iter()
            .map(|window| window.identity.clone())
            .collect::<BTreeSet<_>>();
        let observed = self
            .wait_for_new_windows(application_id, &before_ids, cancellation.clone())
            .await;
        let mut windows = match observed {
            Ok(windows) => windows,
            Err(error) => {
                return Err(self.cleanup_launched_process(error, process_identity).await);
            }
        };

        let target = self.target_for(action);
        let placement = self
            .place_windows(action, &mut windows, target.clone(), cancellation.clone())
            .await;
        let mutations = match placement {
            Ok(mutations) => mutations,
            Err(error) => {
                let cleanup = self.close_windows(&windows, CancellationToken::new()).await;
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => {
                        Err(error.with_context("launched_window_cleanup", cleanup_error.render()))
                    }
                };
            }
        };
        let resources = windows
            .iter()
            .map(|window| {
                window_record(
                    action,
                    window,
                    OwnershipStatus::CreatedByCurrentRun,
                    false,
                    application_id,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut outputs = vec![ActionOutput::log(format!(
            "inspected desktop windows before opening application '{application_id}'"
        ))];
        outputs.push(ActionOutput::log(format!(
            "launched application '{application_id}'"
        )));
        outputs.push(ActionOutput::log(format!(
            "waited for application '{application_id}' to become observable"
        )));
        if target.is_some_and(|target| !matches!(target, WorkspaceTarget::None)) {
            outputs.push(ActionOutput::log(format!(
                "resolved the desktop workspace for application '{application_id}'"
            )));
        }

        Ok(ActionExecutionResult {
            changed: true,
            resources,
            mutations,
            outputs,
        })
    }

    fn reused_result(
        &self,
        action: &ActionSpec,
        application_id: &str,
        windows: &[DesktopWindowSnapshot],
    ) -> Result<ActionExecutionResult> {
        let resources = windows
            .iter()
            .map(|window| {
                window_record(
                    action,
                    window,
                    OwnershipStatus::ReusedExisting,
                    true,
                    application_id,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ActionExecutionResult {
            changed: false,
            resources,
            mutations: Vec::new(),
            outputs: vec![ActionOutput::log(format!(
                "reused the already-open application '{application_id}'"
            ))],
        })
    }

    async fn wait_for_new_windows(
        &self,
        application_id: &str,
        before_ids: &BTreeSet<String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<DesktopWindowSnapshot>> {
        let wait = async {
            loop {
                cancellation.check()?;
                let snapshot = self.desktop.snapshot().await?;
                let windows = matching_windows(&snapshot, application_id)
                    .into_iter()
                    .filter(|window| !before_ids.contains(&window.identity))
                    .collect::<Vec<_>>();
                if !windows.is_empty() {
                    return Ok(windows);
                }
                tokio::time::sleep(self.poll_interval).await;
            }
        };
        tokio::select! {
            _ = cancellation.cancelled() => Err(WorkstateError::new(
                ErrorCategory::Runtime,
                format!("opening application '{application_id}' was cancelled"),
            )),
            result = tokio::time::timeout(self.poll_timeout, wait) => match result {
                Ok(result) => result,
                Err(_) => Err(WorkstateError::new(
                    ErrorCategory::Runtime,
                    format!("application '{application_id}' did not become observable before the timeout"),
                )
                .with_context("application_id", application_id.to_owned())
                .with_context("timeout_milliseconds", self.poll_timeout.as_millis().to_string())),
            },
        }
    }

    async fn place_windows(
        &self,
        action: &ActionSpec,
        windows: &mut [DesktopWindowSnapshot],
        target: Option<WorkspaceTarget>,
        cancellation: CancellationToken,
    ) -> Result<Vec<MutationRecord>> {
        let Some(target) = target else {
            return Ok(Vec::new());
        };
        if matches!(target, WorkspaceTarget::None) {
            return Ok(Vec::new());
        }
        let resolution = ensure_workspace(
            self.desktop.as_ref(),
            target,
            cancellation.clone(),
            action_timeout(action),
        )
        .await?;
        let Some(workspace) = resolution.workspace else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the application workspace target could not be resolved",
            ));
        };
        let mut mutations = Vec::new();
        for window in windows {
            if window.workspace_identity.as_deref() == Some(workspace.identity.as_str()) {
                continue;
            }
            let previous = window.workspace_identity.clone();
            self.desktop
                .move_window(&window.identity, &workspace.identity)
                .await
                .map_err(|error| error.with_context("window_identity", window.identity.clone()))?;
            let refreshed = self.desktop.snapshot().await?;
            let Some(updated) = refreshed.window(&window.identity) else {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "the application window disappeared after workspace placement",
                )
                .with_context("window_identity", window.identity.clone()));
            };
            if updated.workspace_identity.as_deref() != Some(workspace.identity.as_str()) {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "the desktop backend did not confirm application workspace placement",
                )
                .with_context("window_identity", window.identity.clone())
                .with_context("workspace_identity", workspace.identity.clone()));
            }
            window.workspace_identity = Some(workspace.identity.clone());
            if let Some(previous) = previous {
                let resource =
                    ResourceIdentity::new(ResourceKind::DesktopWindow, window.identity.clone())
                        .map_err(WorkstateError::from)?;
                let mut mutation =
                    MutationRecord::new(format!("desktop.window.{}.workspace", window.identity))
                        .map_err(WorkstateError::from)?;
                mutation.action_id = Some(action.id.clone());
                mutation.resource = Some(resource);
                mutation.previous_value = Some(previous);
                mutation.applied_value = Some(workspace.identity.clone());
                mutation.ownership = OwnershipStatus::CreatedByCurrentRun;
                mutation.compensation = CompensationOperation::Handler;
                mutation.cleanup_policy = action.cleanup_policy;
                mutations.push(mutation);
            }
        }
        Ok(mutations)
    }

    async fn cleanup_launched_process(
        &self,
        error: WorkstateError,
        process_identity: Option<String>,
    ) -> WorkstateError {
        let Some(process_identity) = process_identity else {
            return error;
        };
        match self.desktop.stop_application(&process_identity).await {
            Ok(_) => error,
            Err(cleanup_error) => {
                error.with_context("launched_process_cleanup", cleanup_error.render())
            }
        }
    }

    async fn close_windows(
        &self,
        windows: &[DesktopWindowSnapshot],
        cancellation: CancellationToken,
    ) -> Result<()> {
        for window in windows {
            cancellation.check()?;
            if self
                .desktop
                .snapshot()
                .await?
                .window(&window.identity)
                .is_none()
            {
                continue;
            }
            self.desktop.close_window(&window.identity).await?;
            wait_for_window_absence(
                self.desktop.as_ref(),
                &window.identity,
                cancellation.clone(),
                DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
            )
            .await?;
        }
        Ok(())
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
                continue;
            };
            let Some(previous) = mutation.previous_value.as_deref() else {
                continue;
            };
            let snapshot = self.desktop.snapshot().await?;
            let Some(window) = snapshot.window(&resource.stable_identity) else {
                continue;
            };
            if mutation
                .applied_value
                .as_deref()
                .is_some_and(|applied| window.workspace_identity.as_deref() != Some(applied))
            {
                outputs.push(ActionOutput::log(format!(
                    "preserved application window '{}' because its workspace changed after Workstate",
                    resource.stable_identity
                )));
                continue;
            }
            if snapshot.workspace(previous).is_none() {
                outputs.push(ActionOutput::log(format!(
                    "preserved application window '{}' because its previous workspace no longer exists",
                    resource.stable_identity
                )));
                continue;
            }
            self.desktop
                .move_window(&resource.stable_identity, previous)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "restored application window '{}' to desktop workspace '{}'",
                resource.stable_identity, previous
            )));
        }

        let snapshot = self.desktop.snapshot().await?;
        let windows = result
            .resources
            .iter()
            .filter(|resource| {
                resource.resource.kind == ResourceKind::DesktopWindow
                    && resource.ownership.is_environment_owned()
            })
            .filter_map(|resource| snapshot.window(&resource.resource.stable_identity).cloned())
            .collect::<Vec<_>>();
        if !windows.is_empty() {
            self.close_windows(&windows, cancellation).await?;
            outputs.extend(windows.into_iter().map(|window| {
                ActionOutput::log(format!(
                    "closed owned application window '{}'",
                    window.identity
                ))
            }));
        }
        Ok(CompensationResult { outputs })
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
            let expected_application = resource.integration_metadata.get("application_id");
            if expected_application
                .is_some_and(|expected| window.application.as_deref() != Some(expected.as_str()))
            {
                return Ok(ActionObservation::unknown(format!(
                    "persisted application window '{}' now identifies another application",
                    resource.resource.stable_identity
                )));
            }
            observed.push(resource.clone());
        }
        Ok(ActionObservation::already_correct().with_resources(observed))
    }

    async fn stop_inner(
        &self,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        cancellation.check()?;
        let snapshot = self.desktop.snapshot().await?;
        let windows = resources
            .iter()
            .filter(|resource| {
                resource.resource.kind == ResourceKind::DesktopWindow
                    && resource.ownership.is_environment_owned()
            })
            .filter_map(|resource| snapshot.window(&resource.resource.stable_identity).cloned())
            .collect::<Vec<_>>();
        self.close_windows(&windows, cancellation).await?;
        Ok(CompensationResult {
            outputs: windows
                .into_iter()
                .map(|window| {
                    ActionOutput::log(format!(
                        "closed owned application window '{}'",
                        window.identity
                    ))
                })
                .collect(),
        })
    }
}

impl ActionHandler for ApplicationActionHandler {
    fn action_key(&self) -> &str {
        "open_application"
    }

    fn required_capabilities(&self) -> BTreeSet<CapabilityId> {
        [CapabilityId::DesktopWindows].into_iter().collect()
    }

    fn requires_workspace_target_for_observation(&self, _action: &ActionSpec) -> bool {
        false
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if !self.action_matches(action) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "application handler received an incompatible action",
            ));
        }
        action.validate().map_err(WorkstateError::from)?;
        let application_id = self.application_id(action)?;
        self.catalog
            .launch_spec(application_id)
            .map_err(|error| error.with_context("application_id", application_id.to_owned()))?;
        self.resolve_working_directory(action.working_directory.as_deref())?;
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
    catalog: Arc<dyn ApplicationCatalog>,
    desktop: Arc<dyn DesktopBackend>,
    file_system: Arc<dyn FileSystem>,
) -> Result<()> {
    registry.register(ApplicationActionHandler::new(catalog, desktop, file_system))
}

fn matching_windows(
    snapshot: &DesktopSnapshot,
    application_id: &str,
) -> Vec<DesktopWindowSnapshot> {
    snapshot
        .windows
        .iter()
        .filter(|window| window.application.as_deref() == Some(application_id))
        .cloned()
        .collect()
}

fn window_record(
    action: &ActionSpec,
    window: &DesktopWindowSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
    application_id: &str,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::DesktopWindow, window.identity.clone())
        .map_err(WorkstateError::from)?;
    let mut record = ResourceRecord::new(identity, ownership).with_action(action.id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = action.cleanup_policy;
    record
        .integration_metadata
        .insert("application_id".to_owned(), application_id.to_owned());
    if let Some(application) = &window.application {
        record
            .integration_metadata
            .insert("application".to_owned(), application.clone());
    }
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

async fn wait_for_window_absence(
    desktop: &dyn DesktopBackend,
    window_identity: &str,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<()> {
    let wait = async {
        loop {
            cancellation.check()?;
            if desktop.snapshot().await?.window(window_identity).is_none() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::time::timeout(timeout, wait).await.map_err(|_| {
        WorkstateError::new(
            ErrorCategory::Runtime,
            "the application window did not close before the timeout",
        )
        .with_context("window_identity", window_identity.to_owned())
    })?
}

fn action_timeout(action: &ActionSpec) -> Duration {
    action
        .timeout
        .as_ref()
        .map(|timeout| Duration::from_millis(timeout.milliseconds))
        .unwrap_or(DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
}
