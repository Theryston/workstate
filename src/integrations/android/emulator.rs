use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};

use crate::{
    application::{
        planner::{
            ActionExecutionResult, ActionHandler, ActionHandlerRegistry, ActionObservation,
            ActionOutput, CancellationToken, CompensationResult,
        },
        ports::{
            AndroidDeviceSnapshot, BackgroundProcess, BoxFuture, DesktopBackend,
            EmulatorActionContext, EmulatorBackend, EmulatorCleanupOutcome, EmulatorEnsureOutcome,
            EmulatorObservation, EmulatorOperationStatus, EmulatorRequest, EmulatorRuntimeSnapshot,
            ProcessRequest, ProcessRunner, ensure_workspace, resolve_workspace_target,
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

use super::{adb::AdbClient, checks, errors::AndroidError, models};

#[derive(Clone)]
pub struct AndroidBackend {
    runner: Arc<dyn ProcessRunner>,
    desktop: Arc<dyn DesktopBackend>,
    emulator_executable: PathBuf,
    adb: AdbClient,
    launch_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AndroidBackend {
    pub fn new(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        emulator_executable: PathBuf,
        adb_executable: PathBuf,
    ) -> Result<Self> {
        if emulator_executable.as_os_str().is_empty()
            || emulator_executable
                .to_string_lossy()
                .chars()
                .any(char::is_control)
        {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "emulator executable path must be non-empty and contain no control characters",
            ));
        }
        let adb = AdbClient::new(Arc::clone(&runner), adb_executable)?;
        Ok(Self {
            runner,
            desktop,
            emulator_executable,
            adb,
            launch_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn emulator_executable(&self) -> &PathBuf {
        &self.emulator_executable
    }

    pub fn adb(&self) -> &AdbClient {
        &self.adb
    }

    async fn list_avds_inner(
        &self,
    ) -> Result<Vec<crate::application::ports::AndroidVirtualDevice>> {
        let output = self
            .runner
            .run(ProcessRequest {
                program: self.emulator_executable.to_string_lossy().into_owned(),
                arguments: vec!["-list-avds".to_owned()],
                working_directory: None,
                environment: Vec::new(),
            })
            .await
            .map_err(|source| source.with_context("operation", "list-android-avds"))?;
        if !output.succeeded() {
            return Err(super::errors::command_failed("list-avds", &output));
        }
        models::parse_avd_list(&output.stdout)
    }

    async fn validate_avd(&self, avd: &str) -> Result<()> {
        let available = self.list_avds_inner().await?;
        if available.iter().any(|candidate| candidate.name == avd) {
            return Ok(());
        }
        Err(AndroidError::MissingAvd {
            avd: avd.to_owned(),
        }
        .into_workstate()
        .with_context(
            "available_avds",
            available
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ))
    }

    async fn observe_inner(
        &self,
        request: &EmulatorRequest,
        cancellation: &CancellationToken,
    ) -> Result<EmulatorObservation> {
        self.validate_avd(&request.specification.avd).await?;
        cancellation.check()?;
        let devices = self.adb.observe_emulators(cancellation).await?;
        let device = matching_device(&devices, &request.specification.avd)?;
        let Some(device) = device else {
            return Ok(EmulatorObservation::Missing);
        };
        let (window_identity, workspace_identity) = self
            .observe_window(
                &request.specification.avd,
                &device.serial,
                request.workspace_target.as_ref(),
            )
            .await?;
        Ok(EmulatorObservation::Present(EmulatorRuntimeSnapshot {
            avd: request.specification.avd.clone(),
            serial: device.serial,
            state: device.state,
            boot_completed: device.boot_completed,
            process_identity: None,
            window_identity,
            workspace_identity,
        }))
    }

    async fn ensure_inner(
        &self,
        request: &EmulatorRequest,
        cancellation: CancellationToken,
    ) -> Result<EmulatorEnsureOutcome> {
        cancellation.check()?;
        self.validate_avd(&request.specification.avd).await?;
        let initial_devices = self.adb.observe_emulators(&cancellation).await?;
        let (mut device, mut process) = if let Some(device) =
            matching_device(&initial_devices, &request.specification.avd)?
        {
            (device, None)
        } else {
            let _launch_guard = self.launch_lock.lock().await;
            let refreshed_devices = self.adb.observe_emulators(&cancellation).await?;
            if let Some(device) = matching_device(&refreshed_devices, &request.specification.avd)? {
                (device, None)
            } else {
                let process = self
                    .runner
                    .start_background(self.emulator_request(request))
                    .await
                    .map_err(|source| source.with_context("operation", "start-emulator"))?;
                let before_serials = models::serials(&refreshed_devices);
                let wait = checks::wait_for_device(
                    &self.adb,
                    &request.specification.avd,
                    &before_serials,
                    request.timeout,
                    request.poll_interval,
                    cancellation.clone(),
                )
                .await;
                match wait {
                    Ok(device) => (device, Some(process)),
                    Err(error) => {
                        return Err(self.cleanup_launched_process(error, process).await);
                    }
                }
            }
        };

        let launched = process.is_some();
        if !launched {
            device = match checks::wait_for_device(
                &self.adb,
                &request.specification.avd,
                &BTreeSet::new(),
                request.timeout,
                request.poll_interval,
                cancellation.clone(),
            )
            .await
            {
                Ok(device) => device,
                Err(error) => return Err(error),
            };
        }

        let result = self
            .finish_ensure(request, &device, process.as_ref(), cancellation)
            .await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) if launched => {
                let Some(process) = process.take() else {
                    return Err(error);
                };
                Err(self.cleanup_launched_process(error, process).await)
            }
            Err(error) => Err(error),
        }
    }

    async fn finish_ensure(
        &self,
        request: &EmulatorRequest,
        device: &AndroidDeviceSnapshot,
        process: Option<&BackgroundProcess>,
        cancellation: CancellationToken,
    ) -> Result<EmulatorEnsureOutcome> {
        let mut runtime = EmulatorRuntimeSnapshot {
            avd: request.specification.avd.clone(),
            serial: device.serial.clone(),
            state: device.state.clone(),
            boot_completed: device.boot_completed,
            process_identity: process.map(|value| value.identity.clone()),
            window_identity: None,
            workspace_identity: None,
        };
        let mut mutations = Vec::new();
        let mut outputs = vec!["inspected Android Emulator processes and adb devices".to_owned()];
        if process.is_some() {
            outputs.push(format!(
                "started Android Virtual Device '{}'",
                request.specification.avd
            ));
            outputs.push(format!(
                "Android Emulator '{}' is connected and boot-complete",
                request.specification.avd
            ));
        } else {
            outputs.push(format!(
                "reused the running Android Virtual Device '{}'",
                request.specification.avd
            ));
        }

        let needs_window = request
            .workspace_target
            .as_ref()
            .is_some_and(|target| !matches!(target, WorkspaceTarget::None));
        let window = if needs_window {
            Some(
                checks::wait_for_window(
                    self.desktop.as_ref(),
                    &request.specification.avd,
                    &device.serial,
                    request.timeout,
                    request.poll_interval,
                    cancellation.clone(),
                )
                .await?,
            )
        } else {
            None
        };

        if let Some(window) = window.as_ref() {
            runtime.window_identity = Some(window.identity.clone());
            runtime.workspace_identity = window.workspace_identity.clone();
        }

        if let Some(target) = request.workspace_target.clone()
            && !matches!(target, WorkspaceTarget::None)
        {
            let Some(window) = window.as_ref() else {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "the Android Emulator window was not observable for workspace placement",
                ));
            };
            let resolution = ensure_workspace(
                self.desktop.as_ref(),
                target,
                cancellation.clone(),
                request.timeout,
            )
            .await?;
            let Some(workspace) = resolution.workspace else {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "the Android Emulator workspace target could not be resolved",
                ));
            };
            if window.workspace_identity.as_deref() != Some(workspace.identity.as_str()) {
                let previous = window.workspace_identity.clone();
                self.desktop
                    .move_window(&window.identity, &workspace.identity)
                    .await
                    .map_err(|error| {
                        error.with_context("window_identity", window.identity.clone())
                    })?;
                let refreshed = self.desktop.snapshot().await?;
                let Some(updated_window) = refreshed.window(&window.identity) else {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "the Android Emulator window disappeared after workspace placement",
                    ));
                };
                if updated_window.workspace_identity.as_deref() != Some(workspace.identity.as_str())
                {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "the desktop backend did not confirm Android Emulator workspace placement",
                    )
                    .with_context("window_identity", window.identity.clone())
                    .with_context("workspace_identity", workspace.identity.clone()));
                }
                runtime.workspace_identity = Some(workspace.identity.clone());
                let resource =
                    ResourceIdentity::new(ResourceKind::DesktopWindow, window.identity.clone())
                        .map_err(WorkstateError::from)?;
                let mut mutation =
                    MutationRecord::new(format!("desktop.window.{}.workspace", window.identity))
                        .map_err(WorkstateError::from)?;
                mutation.action_id = Some(request.context.action_id.clone());
                mutation.resource = Some(resource);
                mutation.previous_value = previous;
                mutation.applied_value = Some(workspace.identity.clone());
                mutation.ownership = OwnershipStatus::CreatedByCurrentRun;
                mutation.compensation = CompensationOperation::Handler;
                mutation.cleanup_policy = request.context.cleanup_policy;
                mutations.push(mutation);
                outputs.push(format!(
                    "moved Android Emulator '{}' to desktop workspace '{}'",
                    request.specification.avd, workspace.identity
                ));
            }
        }

        let owned = process.is_some();
        let record = emulator_record(request, &runtime, owned)?;
        let status = if owned {
            EmulatorOperationStatus::Started
        } else {
            EmulatorOperationStatus::AlreadyRunning
        };
        Ok(EmulatorEnsureOutcome {
            status,
            runtime: Some(runtime),
            resources: vec![record],
            mutations,
            outputs,
        })
    }

    async fn cleanup_launched_process(
        &self,
        error: WorkstateError,
        process: BackgroundProcess,
    ) -> WorkstateError {
        match self.runner.stop_background(process).await {
            Ok(()) => error,
            Err(cleanup_error) => {
                error.with_context("launched_emulator_cleanup", cleanup_error.render())
            }
        }
    }

    async fn observe_window(
        &self,
        avd: &str,
        serial: &str,
        target: Option<&WorkspaceTarget>,
    ) -> Result<(Option<String>, Option<String>)> {
        let Some(target) = target else {
            return Ok((None, None));
        };
        if matches!(target, WorkspaceTarget::None) {
            return Ok((None, None));
        }
        let snapshot = self.desktop.snapshot().await?;
        let window = checks::find_matching_window(&snapshot, avd, serial)?;
        let Some(window) = window else {
            return Ok((None, None));
        };
        Ok((Some(window.identity), window.workspace_identity))
    }

    fn emulator_request(&self, request: &EmulatorRequest) -> ProcessRequest {
        let mut arguments = vec!["-avd".to_owned(), request.specification.avd.clone()];
        arguments.extend(request.specification.arguments.clone());
        ProcessRequest {
            program: self.emulator_executable.to_string_lossy().into_owned(),
            arguments,
            working_directory: None,
            environment: Vec::new(),
        }
    }

    async fn stop_owned_inner(
        &self,
        request: &EmulatorRequest,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<EmulatorCleanupOutcome> {
        cancellation.check()?;
        let candidates = resources
            .iter()
            .filter(|resource| {
                resource.resource.kind == ResourceKind::AndroidEmulator
                    && resource.is_cleanup_candidate()
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(EmulatorCleanupOutcome {
                status: EmulatorOperationStatus::AlreadyRunning,
                detail: None,
                outputs: Vec::new(),
            });
        }

        let devices = self.adb.observe_emulators(&cancellation).await?;
        let mut stopped = BTreeSet::new();
        let mut outputs = Vec::new();
        for resource in candidates {
            let serial = resource
                .integration_metadata
                .get("serial")
                .map(String::as_str)
                .unwrap_or(resource.resource.stable_identity.as_str());
            if !stopped.insert(serial.to_owned()) {
                continue;
            }
            let Some(device) = devices.iter().find(|device| device.serial == serial) else {
                outputs.push(format!("Android Emulator '{serial}' was already stopped"));
                continue;
            };
            if let Some(expected_avd) = resource.integration_metadata.get("avd")
                && device
                    .avd
                    .as_deref()
                    .is_some_and(|actual| actual != expected_avd)
            {
                return Err(AndroidError::OwnershipUnavailable {
                    serial: serial.to_owned(),
                }
                .into_workstate());
            }
            let process_identity = resource
                .integration_metadata
                .get("process_identity")
                .cloned()
                .ok_or_else(|| {
                    AndroidError::OwnershipUnavailable {
                        serial: serial.to_owned(),
                    }
                    .into_workstate()
                })?;
            let process = BackgroundProcess::new(process_identity)?;
            self.runner.stop_background(process).await?;
            checks::wait_for_device_absence(
                &self.adb,
                serial,
                request.timeout,
                request.poll_interval,
                cancellation.clone(),
            )
            .await?;
            outputs.push(format!("stopped owned Android Emulator '{serial}'"));
        }
        Ok(EmulatorCleanupOutcome {
            status: EmulatorOperationStatus::Ready,
            detail: None,
            outputs,
        })
    }
}

impl EmulatorBackend for AndroidBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(!self.emulator_executable.as_os_str().is_empty()
            && !self.adb.executable().as_os_str().is_empty())
    }

    fn list_avds(
        &self,
    ) -> BoxFuture<'_, Result<Vec<crate::application::ports::AndroidVirtualDevice>>> {
        Box::pin(async move { self.list_avds_inner().await })
    }

    fn observe(
        &self,
        request: EmulatorRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorObservation>> {
        Box::pin(async move {
            cancellation.check()?;
            self.observe_inner(&request, &cancellation).await
        })
    }

    fn ensure(
        &self,
        request: EmulatorRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorEnsureOutcome>> {
        Box::pin(async move { self.ensure_inner(&request, cancellation).await })
    }

    fn stop_owned(
        &self,
        request: EmulatorRequest,
        resources: Vec<ResourceRecord>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorCleanupOutcome>> {
        Box::pin(async move {
            self.stop_owned_inner(&request, &resources, cancellation)
                .await
        })
    }
}

#[derive(Clone)]
pub struct AndroidEmulatorActionHandler {
    backend: Arc<dyn EmulatorBackend>,
    desktop: Arc<dyn DesktopBackend>,
    poll_interval: Duration,
}

impl AndroidEmulatorActionHandler {
    pub fn new(backend: Arc<dyn EmulatorBackend>, desktop: Arc<dyn DesktopBackend>) -> Self {
        Self {
            backend,
            desktop,
            poll_interval: checks::DEFAULT_POLL_INTERVAL,
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    fn request_for(&self, action: &ActionSpec) -> Result<EmulatorRequest> {
        let specification = action.parameters.emulator.clone().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Domain,
                format!("Android Emulator action '{}' is missing its AVD", action.id),
            )
        })?;
        let environment = action.resolved_environment.clone().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Runtime,
                format!(
                    "Android Emulator action '{}' was executed without an environment context",
                    action.id
                ),
            )
        })?;
        Ok(EmulatorRequest {
            context: EmulatorActionContext {
                action_id: action.id.clone(),
                environment,
                cleanup_policy: action.cleanup_policy,
            },
            specification,
            workspace_target: action.resolved_workspace_target.clone(),
            timeout: action_timeout(action),
            poll_interval: self.poll_interval,
        })
    }

    async fn observe_inner(
        &self,
        action: &ActionSpec,
        previous_resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<ActionObservation> {
        let request = self.request_for(action)?;
        let observation = self.backend.observe(request, cancellation).await?;
        match observation {
            EmulatorObservation::Missing => Ok(ActionObservation::requires_change()
                .with_detail("the configured Android Virtual Device is not running")),
            EmulatorObservation::Present(mut runtime) => {
                if let Some(previous) = previous_resources.iter().find(|resource| {
                    resource.resource.kind == ResourceKind::AndroidEmulator
                        && resource.resource.stable_identity == runtime.serial
                }) {
                    runtime.process_identity = previous
                        .integration_metadata
                        .get("process_identity")
                        .cloned();
                    runtime.window_identity = runtime.window_identity.or_else(|| {
                        previous
                            .integration_metadata
                            .get("window_identity")
                            .cloned()
                    });
                    runtime.workspace_identity = runtime.workspace_identity.or_else(|| {
                        previous
                            .integration_metadata
                            .get("workspace_identity")
                            .cloned()
                    });
                }
                let record = emulator_record_from_runtime(
                    action,
                    &runtime,
                    OwnershipStatus::ReusedExisting,
                    true,
                )?;
                if !runtime.is_ready() {
                    return Ok(ActionObservation::requires_change()
                        .with_detail("the Android Emulator is connected but Android has not finished booting")
                        .with_resources(vec![record]));
                }
                if !self.workspace_is_satisfied(action, &runtime).await? {
                    return Ok(ActionObservation::requires_change()
                        .with_detail(
                            "the Android Emulator window is not in the requested workspace",
                        )
                        .with_resources(vec![record]));
                }
                Ok(ActionObservation::already_correct().with_resources(vec![record]))
            }
        }
    }

    async fn workspace_is_satisfied(
        &self,
        action: &ActionSpec,
        runtime: &EmulatorRuntimeSnapshot,
    ) -> Result<bool> {
        let Some(target) = action.resolved_workspace_target.as_ref() else {
            return Ok(true);
        };
        if matches!(target, WorkspaceTarget::None) {
            return Ok(true);
        }
        let Some(workspace_identity) = runtime.workspace_identity.as_deref() else {
            return Ok(false);
        };
        let snapshot = self.desktop.snapshot().await?;
        let resolution = resolve_workspace_target(&snapshot, target)?;
        Ok(resolution
            .workspace
            .is_some_and(|workspace| workspace.identity == workspace_identity))
    }

    async fn compensate_inner(
        &self,
        action: &ActionSpec,
        result: &ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        let request = self.request_for(action)?;
        let mut outputs = Vec::new();
        let cleanup = self
            .backend
            .stop_owned(request, result.resources.clone(), cancellation.clone())
            .await?;
        outputs.extend(cleanup.outputs.into_iter().map(ActionOutput::log));
        outputs.extend(
            self.restore_mutations(&result.mutations, cancellation)
                .await?,
        );
        Ok(CompensationResult { outputs })
    }

    async fn restore_mutations(
        &self,
        mutations: &[MutationRecord],
        cancellation: CancellationToken,
    ) -> Result<Vec<ActionOutput>> {
        let mut outputs = Vec::new();
        for mutation in mutations {
            if mutation.compensation == CompensationOperation::None
                || !mutation.target.starts_with("desktop.window.")
            {
                continue;
            }
            let Some(resource) = mutation.resource.as_ref() else {
                outputs.push(ActionOutput::log(format!(
                    "preserved emulator window mutation '{}' because its window identity is unavailable",
                    mutation.target
                )));
                continue;
            };
            cancellation.check()?;
            let snapshot = self.desktop.snapshot().await?;
            let Some(window) = snapshot.window(&resource.stable_identity) else {
                outputs.push(ActionOutput::log(format!(
                    "preserved emulator window '{}' because it no longer exists",
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
                    "preserved emulator window '{}' because its workspace changed after Workstate",
                    resource.stable_identity
                )));
                continue;
            }
            let Some(previous) = mutation.previous_value.as_deref() else {
                outputs.push(ActionOutput::log(format!(
                    "preserved emulator window '{}' because its previous workspace is unavailable",
                    resource.stable_identity
                )));
                continue;
            };
            if snapshot.workspace(previous).is_none() {
                outputs.push(ActionOutput::log(format!(
                    "preserved emulator window '{}' because its previous workspace no longer exists",
                    resource.stable_identity
                )));
                continue;
            }
            self.desktop
                .move_window(&resource.stable_identity, previous)
                .await?;
            outputs.push(ActionOutput::log(format!(
                "restored emulator window '{}' to desktop workspace '{}'",
                resource.stable_identity, previous
            )));
        }
        Ok(outputs)
    }

    async fn stop_inner(
        &self,
        action: &ActionSpec,
        resources: &[ResourceRecord],
        cancellation: CancellationToken,
    ) -> Result<CompensationResult> {
        let request = self.request_for(action)?;
        let cleanup = self
            .backend
            .stop_owned(request, resources.to_vec(), cancellation)
            .await?;
        Ok(CompensationResult {
            outputs: cleanup.outputs.into_iter().map(ActionOutput::log).collect(),
        })
    }
}

impl ActionHandler for AndroidEmulatorActionHandler {
    fn action_key(&self) -> &str {
        "start_android_emulator"
    }

    fn required_capabilities(&self) -> BTreeSet<CapabilityId> {
        [CapabilityId::AndroidEmulator, CapabilityId::Adb]
            .into_iter()
            .collect()
    }

    fn validate(&self, action: &ActionSpec) -> Result<()> {
        if action.kind != ActionKind::StartAndroidEmulator {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "the Android Emulator handler received an incompatible action",
            ));
        }
        action.validate().map_err(WorkstateError::from)
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
        action: &'a ActionSpec,
        resources: &'a [ResourceRecord],
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionObservation>> {
        Box::pin(async move {
            cancellation.check()?;
            let request = self.request_for(action)?;
            let observation = self.backend.observe(request, cancellation.clone()).await?;
            let EmulatorObservation::Present(runtime) = observation else {
                return Ok(ActionObservation::already_correct());
            };
            let mut observed = Vec::new();
            for resource in resources {
                if resource.resource.kind != ResourceKind::AndroidEmulator {
                    continue;
                }
                let serial = resource
                    .integration_metadata
                    .get("serial")
                    .map(String::as_str)
                    .unwrap_or(resource.resource.stable_identity.as_str());
                if runtime.serial != serial {
                    continue;
                }
                if let Some(expected_avd) = resource.integration_metadata.get("avd")
                    && runtime.avd != *expected_avd
                {
                    return Ok(ActionObservation::unknown(format!(
                        "persisted Android Emulator serial '{}' now identifies a different AVD",
                        serial
                    )));
                }
                observed.push(resource.clone());
            }
            Ok(ActionObservation::already_correct().with_resources(observed))
        })
    }

    fn apply<'a>(
        &'a self,
        action: &'a ActionSpec,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ActionExecutionResult>> {
        Box::pin(async move {
            let request = self.request_for(action)?;
            let outcome = self.backend.ensure(request, cancellation).await?;
            Ok(ActionExecutionResult {
                changed: outcome.status != EmulatorOperationStatus::AlreadyRunning
                    || !outcome.mutations.is_empty(),
                resources: outcome.resources,
                mutations: outcome.mutations,
                outputs: outcome.outputs.into_iter().map(ActionOutput::log).collect(),
            })
        })
    }

    fn compensate<'a>(
        &'a self,
        action: &'a ActionSpec,
        result: &'a ActionExecutionResult,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<CompensationResult>> {
        Box::pin(async move { self.compensate_inner(action, result, cancellation).await })
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
    backend: Arc<dyn EmulatorBackend>,
    desktop: Arc<dyn DesktopBackend>,
) -> Result<()> {
    registry.register(AndroidEmulatorActionHandler::new(backend, desktop))?;
    Ok(())
}

fn matching_device(
    devices: &[AndroidDeviceSnapshot],
    avd: &str,
) -> Result<Option<AndroidDeviceSnapshot>> {
    let matches = devices
        .iter()
        .filter(|device| device.avd.as_deref() == Some(avd))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [device] => Ok(Some(device.clone())),
        _ => Err(AndroidError::AmbiguousAvd {
            avd: avd.to_owned(),
            serials: matches.into_iter().map(|device| device.serial).collect(),
        }
        .into_workstate()),
    }
}

fn emulator_record(
    request: &EmulatorRequest,
    runtime: &EmulatorRuntimeSnapshot,
    owned: bool,
) -> Result<ResourceRecord> {
    let record = emulator_record_from_context(
        &request.context,
        runtime,
        if owned {
            OwnershipStatus::CreatedByCurrentRun
        } else {
            OwnershipStatus::ReusedExisting
        },
        !owned,
    )?;
    Ok(record)
}

fn emulator_record_from_runtime(
    action: &ActionSpec,
    runtime: &EmulatorRuntimeSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
) -> Result<ResourceRecord> {
    let environment = action.resolved_environment.clone().ok_or_else(|| {
        WorkstateError::new(
            ErrorCategory::Runtime,
            format!(
                "Android Emulator action '{}' has no environment context",
                action.id
            ),
        )
    })?;
    emulator_record_from_context(
        &EmulatorActionContext {
            action_id: action.id.clone(),
            environment,
            cleanup_policy: action.cleanup_policy,
        },
        runtime,
        ownership,
        observed_before,
    )
}

fn emulator_record_from_context(
    context: &EmulatorActionContext,
    runtime: &EmulatorRuntimeSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::AndroidEmulator, runtime.serial.clone())
        .map_err(WorkstateError::from)?;
    let mut record =
        ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("avd".to_owned(), runtime.avd.clone());
    record
        .integration_metadata
        .insert("serial".to_owned(), runtime.serial.clone());
    if let Some(process_identity) = &runtime.process_identity {
        record
            .integration_metadata
            .insert("process_identity".to_owned(), process_identity.clone());
    }
    if let Some(window_identity) = &runtime.window_identity {
        record
            .integration_metadata
            .insert("window_identity".to_owned(), window_identity.clone());
    }
    if let Some(workspace_identity) = &runtime.workspace_identity {
        record
            .integration_metadata
            .insert("workspace_identity".to_owned(), workspace_identity.clone());
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
