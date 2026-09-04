use std::sync::Arc;

use crate::{
    application::ports::{
        BoxFuture, DesktopBackend, DesktopOperationOutcome, DesktopSnapshot, ProcessOutput,
        ProcessRequest, ProcessRunner,
    },
    error::{ErrorCategory, Result, WorkstateError},
    platform::desktop::cosmic::{CosmicCommand, CosmicOperation},
};

use super::{errors::CosmicError, models};

#[derive(Clone)]
pub struct CosmicBackend {
    runner: Arc<dyn ProcessRunner>,
    command: CosmicCommand,
}

impl CosmicBackend {
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            runner,
            command: CosmicCommand::default(),
        }
    }

    pub fn with_command(mut self, command: CosmicCommand) -> Self {
        self.command = command;
        self
    }

    pub fn command(&self) -> &CosmicCommand {
        &self.command
    }

    async fn run_operation(&self, operation: CosmicOperation) -> Result<ProcessOutput> {
        let operation_name = operation_name(&operation);
        let request = ProcessRequest {
            program: self.command.program().to_owned(),
            arguments: self.command.arguments(&operation),
            working_directory: None,
            environment: Vec::new(),
        };
        let output = self.runner.run(request).await.map_err(|source| {
            CosmicError::CommandFailed {
                operation: operation_name.clone(),
                detail: source.render(),
            }
            .into_workstate()
        })?;
        if !output.succeeded() {
            return Err(CosmicError::CommandFailed {
                operation: operation_name,
                detail: process_failure_detail(&output),
            }
            .into_workstate());
        }
        Ok(output)
    }

    async fn run_mutation(&self, operation: CosmicOperation) -> Result<DesktopOperationOutcome> {
        let identity = operation_identity(&operation);
        let operation_name = operation_name(&operation);
        let output = self.run_operation(operation).await?;
        if !output.stdout.is_empty() {
            serde_json::from_slice::<serde_json::Value>(&output.stdout).map_err(|source| {
                CosmicError::MalformedOutput {
                    operation: operation_name,
                    detail: source.to_string(),
                }
                .into_workstate()
            })?;
        }
        Ok(DesktopOperationOutcome::changed(identity))
    }

    pub async fn observe(&self) -> Result<DesktopSnapshot> {
        let workspace_future = self.run_operation(CosmicOperation::GetWorkspaces);
        let window_future = self.run_operation(CosmicOperation::GetWindows);
        let (workspace_output, window_output) = tokio::join!(workspace_future, window_future);
        let workspace_output = workspace_output?;
        let window_output = window_output?;
        models::decode_snapshot(&workspace_output.stdout, &window_output.stdout)
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
                .runner
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

    fn create_workspace<'a>(
        &'a self,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(CosmicError::Unavailable {
                operation: "create-workspace".to_owned(),
                detail: "the current COSMIC command contract does not expose workspace creation"
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
                detail: "the current COSMIC command contract does not expose workspace deletion"
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
            self.run_mutation(CosmicOperation::MoveWindow {
                window: window_identity.to_owned(),
                workspace: workspace_identity.to_owned(),
            })
            .await
        })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            self.run_mutation(CosmicOperation::CloseWindow {
                window: window_identity.to_owned(),
            })
            .await
        })
    }

    fn focus_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            self.run_mutation(CosmicOperation::FocusWindow {
                window: window_identity.to_owned(),
            })
            .await
        })
    }

    fn set_tiling<'a>(
        &'a self,
        workspace_identity: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            self.run_mutation(CosmicOperation::SetTiling {
                workspace: workspace_identity.to_owned(),
                enabled,
            })
            .await
        })
    }
}

fn operation_name(operation: &CosmicOperation) -> String {
    match operation {
        CosmicOperation::GetWorkspaces => "get-workspaces".to_owned(),
        CosmicOperation::GetWindows => "get-toplevels".to_owned(),
        CosmicOperation::SetTiling { .. } => "set-tiling".to_owned(),
        CosmicOperation::MoveWindow { .. } => "move-window".to_owned(),
        CosmicOperation::CloseWindow { .. } => "close-window".to_owned(),
        CosmicOperation::FocusWindow { .. } => "focus-window".to_owned(),
    }
}

fn operation_identity(operation: &CosmicOperation) -> Option<String> {
    match operation {
        CosmicOperation::SetTiling { workspace, .. } => Some(workspace.clone()),
        CosmicOperation::MoveWindow { window, .. }
        | CosmicOperation::CloseWindow { window }
        | CosmicOperation::FocusWindow { window } => Some(window.clone()),
        CosmicOperation::GetWorkspaces | CosmicOperation::GetWindows => None,
    }
}

fn process_failure_detail(output: &ProcessOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    output
        .exit_code()
        .map(|code| format!("process exited with status {code}"))
        .unwrap_or_else(|| "process terminated without an exit status".to_owned())
}

pub fn unsupported_desktop_error() -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Platform,
        "COSMIC desktop integration is unavailable on this platform",
    )
}
