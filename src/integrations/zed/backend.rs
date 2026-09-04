use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    application::{
        planner::CancellationToken,
        ports::{
            BoxFuture, Clock, DesktopBackend, EditorBackend, EditorOpenOutcome,
            EditorOperationStatus, EditorWindowSnapshot, FileSystem, ProcessRequest, ProcessRunner,
            SystemClock,
        },
    },
    error::Result,
};

use super::errors::ZedError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZedCommand {
    program: String,
    new_window_flag: Option<String>,
}

impl ZedCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            new_window_flag: Some("-n".to_owned()),
        }
    }

    pub fn without_new_window_flag(mut self) -> Self {
        self.new_window_flag = None;
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self, project_path: &Path) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(flag) = &self.new_window_flag {
            arguments.push(flag.clone());
        }
        arguments.push(project_path.display().to_string());
        arguments
    }
}

impl Default for ZedCommand {
    fn default() -> Self {
        Self::new("zed")
    }
}

#[derive(Clone)]
pub struct ZedBackend {
    runner: Arc<dyn ProcessRunner>,
    desktop: Arc<dyn DesktopBackend>,
    file_system: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    command: ZedCommand,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl ZedBackend {
    pub fn new(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
    ) -> Self {
        Self::with_clock(runner, desktop, file_system, Arc::new(SystemClock))
    }

    pub fn with_clock(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            runner,
            desktop,
            file_system,
            clock,
            command: ZedCommand::default(),
            poll_interval: Duration::from_millis(25),
            poll_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_command(mut self, command: ZedCommand) -> Self {
        self.command = command;
        self
    }

    pub fn with_timing(mut self, poll_interval: Duration, poll_timeout: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.poll_timeout = poll_timeout;
        self
    }

    pub fn command(&self) -> &ZedCommand {
        &self.command
    }

    pub fn resolve_project_path(&self, project_path: &Path) -> Result<PathBuf> {
        if !project_path.is_absolute() {
            return Err(ZedError::InvalidProjectPath {
                detail: "the configured project path must be absolute".to_owned(),
            }
            .into_workstate());
        }
        let exists = self
            .file_system
            .exists(project_path)
            .map_err(|source| operation_error("inspect-project-path", source))?;
        if !exists {
            return Err(ZedError::InvalidProjectPath {
                detail: format!(
                    "the configured project path does not exist: {}",
                    project_path.display()
                ),
            }
            .into_workstate());
        }
        let directory = self
            .file_system
            .is_directory(project_path)
            .map_err(|source| operation_error("inspect-project-path", source))?;
        if !directory {
            return Err(ZedError::InvalidProjectPath {
                detail: format!(
                    "the configured project path is not a directory: {}",
                    project_path.display()
                ),
            }
            .into_workstate());
        }
        self.file_system
            .canonicalize(project_path)
            .map_err(|source| operation_error("resolve-project-path", source))
    }

    async fn observe_zed_projects(&self) -> Result<Vec<EditorWindowSnapshot>> {
        let snapshot = self.desktop.snapshot().await?;
        Ok(snapshot
            .windows
            .into_iter()
            .filter_map(|window| {
                let application = window.application?;
                if !is_zed_application(&application) {
                    return None;
                }
                Some(EditorWindowSnapshot {
                    identity: window.identity,
                    application,
                    title: window.title,
                    project_path: window.project_path.map(PathBuf::from),
                    workspace_identity: window.workspace_identity,
                })
            })
            .collect())
    }

    async fn open_project_inner(
        &self,
        project_path: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<EditorOpenOutcome> {
        cancellation.check()?;
        let project_path = self.resolve_project_path(&project_path)?;
        let before = self.observe_zed_projects().await?;
        let matches = matching_projects(&before, &project_path);
        if matches.len() > 1 {
            return Err(ZedError::AmbiguousProject {
                project: project_path.display().to_string(),
                matches: matches.len(),
            }
            .into_workstate());
        }
        if let Some(window) = matches.into_iter().next() {
            return Ok(EditorOpenOutcome {
                status: EditorOperationStatus::Reused,
                window,
                owned: false,
                process_identity: None,
            });
        }

        let process = self
            .runner
            .start_background(ProcessRequest {
                program: self.command.program().to_owned(),
                arguments: self.command.arguments(&project_path),
                working_directory: Some(project_path.clone()),
                environment: Vec::new(),
            })
            .await
            .map_err(|source| operation_error("launch-project", source))?;
        let before_ids = before
            .iter()
            .map(|window| window.identity.clone())
            .collect::<BTreeSet<_>>();
        let project = project_path.clone();
        let launched_at = self.clock.monotonic_now();
        let wait = async {
            loop {
                cancellation.check()?;
                if self.clock.elapsed_since(launched_at) >= self.poll_timeout {
                    return Err(ZedError::WindowTimeout {
                        project: project.display().to_string(),
                    }
                    .into_workstate());
                }
                let observed = self.observe_zed_projects().await?;
                let matches = matching_projects(&observed, &project_path);
                if matches.len() > 1 {
                    return Err(ZedError::AmbiguousProject {
                        project: project.display().to_string(),
                        matches: matches.len(),
                    }
                    .into_workstate());
                }
                if let Some(window) = matches.into_iter().next() {
                    let owned = !before_ids.contains(&window.identity);
                    return Ok(EditorOpenOutcome {
                        status: if owned {
                            EditorOperationStatus::Launched
                        } else {
                            EditorOperationStatus::Reused
                        },
                        window,
                        owned,
                        process_identity: owned.then_some(process.identity.clone()),
                    });
                }
                let new_windows = observed
                    .iter()
                    .filter(|window| !before_ids.contains(&window.identity))
                    .cloned()
                    .collect::<Vec<_>>();
                if new_windows.len() == 1 {
                    let window = new_windows.into_iter().next().ok_or_else(|| {
                        ZedError::OperationFailed {
                            operation: "observe-launched-project".to_owned(),
                            detail: "the newly launched Zed window disappeared before it could be recorded".to_owned(),
                        }
                        .into_workstate()
                    })?;
                    return Ok(EditorOpenOutcome {
                        status: EditorOperationStatus::Launched,
                        window,
                        owned: true,
                        process_identity: Some(process.identity.clone()),
                    });
                }
                if new_windows.len() > 1 {
                    return Err(ZedError::AmbiguousProject {
                        project: project.display().to_string(),
                        matches: new_windows.len(),
                    }
                    .into_workstate());
                }
                tokio::time::sleep(self.poll_interval).await;
            }
        };
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(ZedError::OperationFailed {
                operation: "wait-for-project-window".to_owned(),
                detail: "the operation was cancelled".to_owned(),
            }.into_workstate()),
            result = tokio::time::timeout(self.poll_timeout, wait) => match result {
                Ok(result) => result,
                Err(_) => Err(ZedError::WindowTimeout {
                    project: project_path.display().to_string(),
                }.into_workstate()),
            },
        };
        if let Err(error) = result {
            let cleanup = self.runner.stop_background(process).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => {
                    Err(error.with_context("launched_process_cleanup", cleanup_error.render()))
                }
            };
        }
        result
    }
}

impl EditorBackend for ZedBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn observe_projects<'a>(&'a self) -> BoxFuture<'a, Result<Vec<EditorWindowSnapshot>>> {
        Box::pin(async move { self.observe_zed_projects().await })
    }

    fn open_project<'a>(
        &'a self,
        project_path: PathBuf,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<EditorOpenOutcome>> {
        Box::pin(async move { self.open_project_inner(project_path, cancellation).await })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<crate::application::ports::DesktopOperationOutcome>> {
        Box::pin(async move { self.desktop.close_window(window_identity).await })
    }
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

pub fn is_zed_application(application: &str) -> bool {
    let normalized = application
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(normalized.as_str(), "zed" | "devzedzed" | "devzed")
}

fn operation_error(
    operation: &str,
    source: crate::error::WorkstateError,
) -> crate::error::WorkstateError {
    ZedError::OperationFailed {
        operation: operation.to_owned(),
        detail: source.render(),
    }
    .into_workstate()
}
