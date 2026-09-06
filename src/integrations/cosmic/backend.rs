use std::sync::Arc;

use crate::{
    application::ports::{
        BackgroundProcess, BoxFuture, DesktopBackend, DesktopOperationOutcome, DesktopSnapshot,
        ProcessRequest, ProcessRunner,
    },
    application::timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{errors::CosmicError, wayland::CosmicWaylandCoordinator};

#[derive(Clone)]
pub struct CosmicBackend {
    process_runner: Arc<dyn ProcessRunner>,
    wayland: Arc<CosmicWaylandCoordinator>,
}

impl CosmicBackend {
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self::with_wayland(runner, Arc::new(CosmicWaylandCoordinator::new()))
    }

    pub fn with_wayland(
        process_runner: Arc<dyn ProcessRunner>,
        wayland: Arc<CosmicWaylandCoordinator>,
    ) -> Self {
        Self {
            process_runner,
            wayland,
        }
    }

    pub async fn observe(&self) -> Result<DesktopSnapshot> {
        self.wayland
            .observe(DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
            .await
            .map_err(CosmicError::into_workstate)
    }

    async fn set_tiling_native(
        &self,
        workspace_identity: &str,
        enabled: bool,
    ) -> Result<DesktopOperationOutcome> {
        self.wayland
            .set_tiling(
                workspace_identity,
                enabled,
                DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
            )
            .await
            .map(map_native_outcome)
            .map_err(CosmicError::into_workstate)
    }

    async fn move_window_native(
        &self,
        window_identity: &str,
        workspace_identity: &str,
    ) -> Result<DesktopOperationOutcome> {
        self.wayland
            .move_window(
                window_identity,
                workspace_identity,
                DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
            )
            .await
            .map(map_native_outcome)
            .map_err(CosmicError::into_workstate)
    }

    async fn close_window_native(&self, window_identity: &str) -> Result<DesktopOperationOutcome> {
        self.wayland
            .close_window(window_identity, DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
            .await
            .map(map_native_outcome)
            .map_err(CosmicError::into_workstate)
    }

    async fn focus_window_native(&self, window_identity: &str) -> Result<DesktopOperationOutcome> {
        self.wayland
            .focus_window(window_identity, DEFAULT_EXTERNAL_OPERATION_TIMEOUT)
            .await
            .map(map_native_outcome)
            .map_err(CosmicError::into_workstate)
    }
}

impl DesktopBackend for CosmicBackend {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>> {
        Box::pin(async move { self.observe().await })
    }

    fn open_application<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            let process = self
                .process_runner
                .start_background(request)
                .await
                .map_err(|source| {
                    CosmicError::CommandFailed {
                        operation: "open-application".to_owned(),
                        detail: source.render(),
                    }
                    .into_workstate()
                })?;
            Ok(DesktopOperationOutcome::created(Some(process.identity)))
        })
    }

    fn stop_application<'a>(
        &'a self,
        process_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            let process = BackgroundProcess::new(process_identity.to_owned())?;
            self.process_runner
                .stop_background(process)
                .await
                .map_err(|source| {
                    CosmicError::CommandFailed {
                        operation: "stop-application".to_owned(),
                        detail: source.render(),
                    }
                    .into_workstate()
                })?;
            Ok(DesktopOperationOutcome::changed(Some(
                process_identity.to_owned(),
            )))
        })
    }

    fn create_workspace<'a>(
        &'a self,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(CosmicError::Unavailable {
                operation: "create-workspace".to_owned(),
                detail: "the native COSMIC workspace protocol does not expose workspace creation"
                    .to_owned(),
            }
            .into_workstate())
        })
    }

    fn delete_workspace<'a>(
        &'a self,
        _workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(CosmicError::Unavailable {
                operation: "delete-workspace".to_owned(),
                detail: "the native COSMIC workspace protocol does not expose workspace deletion"
                    .to_owned(),
            }
            .into_workstate())
        })
    }

    fn move_window<'a>(
        &'a self,
        window_identity: &'a str,
        workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            self.move_window_native(window_identity, workspace_identity)
                .await
        })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move { self.close_window_native(window_identity).await })
    }

    fn focus_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move { self.focus_window_native(window_identity).await })
    }

    fn set_tiling<'a>(
        &'a self,
        workspace_identity: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move { self.set_tiling_native(workspace_identity, enabled).await })
    }
}

fn map_native_outcome(outcome: DesktopOperationOutcome) -> DesktopOperationOutcome {
    match outcome.status {
        crate::application::ports::DesktopOperationStatus::Changed => {
            let mut mapped = DesktopOperationOutcome::changed(outcome.identity);
            mapped.detail = outcome.detail;
            mapped
        }
        crate::application::ports::DesktopOperationStatus::Unchanged => {
            let mut mapped = DesktopOperationOutcome::unchanged(outcome.identity);
            mapped.detail = outcome.detail;
            mapped
        }
        crate::application::ports::DesktopOperationStatus::Created
        | crate::application::ports::DesktopOperationStatus::AlreadyPresent
        | crate::application::ports::DesktopOperationStatus::Reused
        | crate::application::ports::DesktopOperationStatus::Unavailable
        | crate::application::ports::DesktopOperationStatus::Ambiguous => outcome,
    }
}

pub fn unsupported_desktop_error() -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Platform,
        "COSMIC desktop integration is unavailable on this platform",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::application::ports::{DesktopOperationStatus, ProcessOutput};

    #[derive(Clone, Default)]
    struct RecordingProcessRunner {
        started: Arc<Mutex<Vec<ProcessRequest>>>,
        stopped: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingProcessRunner {
        fn started(&self) -> Result<Vec<ProcessRequest>> {
            self.started
                .lock()
                .map(|requests| requests.clone())
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "test lock failed"))
        }

        fn stopped(&self) -> Result<Vec<String>> {
            self.stopped
                .lock()
                .map(|identities| identities.clone())
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "test lock failed"))
        }
    }

    impl ProcessRunner for RecordingProcessRunner {
        fn run<'a>(&'a self, _: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
            Box::pin(async {
                Err(WorkstateError::new(
                    ErrorCategory::Process,
                    "the desktop backend must not use ProcessRunner::run for COSMIC state",
                ))
            })
        }

        fn start_background<'a>(
            &'a self,
            request: ProcessRequest,
        ) -> BoxFuture<'a, Result<BackgroundProcess>> {
            let result = self
                .started
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "test lock failed"))
                .and_then(|mut requests| {
                    requests.push(request);
                    BackgroundProcess::new("application-process")
                });
            Box::pin(async move { result })
        }

        fn stop_background<'a>(&'a self, process: BackgroundProcess) -> BoxFuture<'a, Result<()>> {
            let result = self
                .stopped
                .lock()
                .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "test lock failed"))
                .map(|mut identities| identities.push(process.identity));
            Box::pin(async move { result })
        }
    }

    #[tokio::test]
    async fn application_launch_and_cleanup_stay_on_the_injected_process_port() -> Result<()> {
        let runner = Arc::new(RecordingProcessRunner::default());
        let backend = CosmicBackend::new(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        let request = ProcessRequest {
            program: "zed".to_owned(),
            arguments: vec!["--new".to_owned()],
            working_directory: None,
            environment: Vec::new(),
        };

        let launch = backend.open_application(request.clone()).await?;
        assert_eq!(launch.status, DesktopOperationStatus::Created);
        assert_eq!(launch.identity.as_deref(), Some("application-process"));
        assert_eq!(runner.started()?, vec![request]);

        let cleanup = backend.stop_application("application-process").await?;
        assert_eq!(cleanup.status, DesktopOperationStatus::Changed);
        assert_eq!(runner.stopped()?, vec!["application-process".to_owned()]);
        Ok(())
    }

    #[test]
    fn native_outcome_mapping_preserves_only_changed_and_unchanged_semantics() {
        let changed = map_native_outcome(
            DesktopOperationOutcome::changed(Some("window-1".to_owned())).with_detail("confirmed"),
        );
        let unchanged = map_native_outcome(DesktopOperationOutcome::unchanged(Some(
            "workspace-1".to_owned(),
        )));

        assert_eq!(changed.status, DesktopOperationStatus::Changed);
        assert_eq!(changed.identity.as_deref(), Some("window-1"));
        assert_eq!(changed.detail.as_deref(), Some("confirmed"));
        assert_eq!(unchanged.status, DesktopOperationStatus::Unchanged);
        assert_eq!(unchanged.identity.as_deref(), Some("workspace-1"));
    }

    #[test]
    fn native_outcome_mapping_preserves_non_mutation_statuses() {
        for status in [
            DesktopOperationStatus::Created,
            DesktopOperationStatus::AlreadyPresent,
            DesktopOperationStatus::Reused,
            DesktopOperationStatus::Unavailable,
            DesktopOperationStatus::Ambiguous,
        ] {
            let outcome = DesktopOperationOutcome {
                status,
                identity: Some("resource-1".to_owned()),
                detail: Some("preserved".to_owned()),
            };

            assert_eq!(map_native_outcome(outcome.clone()), outcome);
        }
    }
}
