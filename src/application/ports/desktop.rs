use std::{collections::BTreeSet, time::Duration};

use crate::{
    application::{
        planner::CancellationToken,
        ports::process::{BoxFuture, ProcessRequest},
    },
    domain::{WorkspaceReference, WorkspaceTarget},
    error::{ErrorCategory, Result, WorkstateError},
    platform::DesktopEnvironment,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWorkspaceSnapshot {
    pub identity: String,
    pub name: Option<String>,
    pub position: Option<u32>,
    pub focused: bool,
    pub tiling_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWindowSnapshot {
    pub identity: String,
    pub application: Option<String>,
    pub title: Option<String>,
    pub project_path: Option<String>,
    pub workspace_identity: Option<String>,
    pub focused: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopSnapshot {
    pub workspaces: Vec<DesktopWorkspaceSnapshot>,
    pub windows: Vec<DesktopWindowSnapshot>,
}

impl DesktopSnapshot {
    pub fn workspace(&self, identity: &str) -> Option<&DesktopWorkspaceSnapshot> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.identity == identity)
    }

    pub fn window(&self, identity: &str) -> Option<&DesktopWindowSnapshot> {
        self.windows
            .iter()
            .find(|window| window.identity == identity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOperationStatus {
    Created,
    AlreadyPresent,
    Reused,
    Changed,
    Unchanged,
    Unavailable,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopOperationOutcome {
    pub status: DesktopOperationStatus,
    pub identity: Option<String>,
    pub detail: Option<String>,
}

impl DesktopOperationOutcome {
    pub fn created(identity: Option<String>) -> Self {
        Self {
            status: DesktopOperationStatus::Created,
            identity,
            detail: None,
        }
    }

    pub fn reused(identity: Option<String>) -> Self {
        Self {
            status: DesktopOperationStatus::Reused,
            identity,
            detail: None,
        }
    }

    pub fn changed(identity: Option<String>) -> Self {
        Self {
            status: DesktopOperationStatus::Changed,
            identity,
            detail: None,
        }
    }

    pub fn unchanged(identity: Option<String>) -> Self {
        Self {
            status: DesktopOperationStatus::Unchanged,
            identity,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWorkspaceResolution {
    pub workspace: Option<DesktopWorkspaceSnapshot>,
    pub status: DesktopOperationStatus,
    pub detail: Option<String>,
}

impl DesktopWorkspaceResolution {
    pub fn none() -> Self {
        Self {
            workspace: None,
            status: DesktopOperationStatus::Unchanged,
            detail: None,
        }
    }

    pub fn existing(workspace: DesktopWorkspaceSnapshot) -> Self {
        Self {
            workspace: Some(workspace),
            status: DesktopOperationStatus::Reused,
            detail: None,
        }
    }

    pub fn created(workspace: DesktopWorkspaceSnapshot) -> Self {
        Self {
            workspace: Some(workspace),
            status: DesktopOperationStatus::Created,
            detail: None,
        }
    }
}

pub trait DesktopBackend: Send + Sync {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>>;

    fn open_application<'a>(
        &'a self,
        _request: ProcessRequest,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop application opening is not configured",
            ))
        })
    }

    fn create_workspace<'a>(
        &'a self,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop workspace creation is not configured",
            ))
        })
    }

    fn delete_workspace<'a>(
        &'a self,
        _workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop workspace deletion is not configured",
            ))
        })
    }

    fn move_window<'a>(
        &'a self,
        _window_identity: &'a str,
        _workspace_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop window movement is not configured",
            ))
        })
    }

    fn close_window<'a>(
        &'a self,
        _window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop window closing is not configured",
            ))
        })
    }

    fn focus_window<'a>(
        &'a self,
        _window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop window focusing is not configured",
            ))
        })
    }

    fn set_tiling<'a>(
        &'a self,
        _workspace_identity: &'a str,
        _enabled: bool,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Platform,
                "desktop tiling mutation is not configured",
            ))
        })
    }

    fn restore_tiling<'a>(
        &'a self,
        workspace_identity: &'a str,
        previous: bool,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        self.set_tiling(workspace_identity, previous)
    }
}

pub trait DesktopEnvironmentDetector: Send + Sync {
    fn detect(&self) -> Result<DesktopEnvironment>;
}

pub fn resolve_workspace_target(
    snapshot: &DesktopSnapshot,
    target: &WorkspaceTarget,
) -> Result<DesktopWorkspaceResolution> {
    match target {
        WorkspaceTarget::None => Ok(DesktopWorkspaceResolution::none()),
        WorkspaceTarget::Current => {
            let focused_windows = snapshot
                .windows
                .iter()
                .filter(|window| window.focused)
                .collect::<Vec<_>>();
            match focused_windows.as_slice() {
                [window] => {
                    let Some(identity) = window.workspace_identity.as_deref() else {
                        return Err(WorkstateError::new(
                            ErrorCategory::Platform,
                            "the focused window does not expose a desktop workspace",
                        ));
                    };
                    return snapshot
                        .workspace(identity)
                        .cloned()
                        .map(DesktopWorkspaceResolution::existing)
                        .ok_or_else(|| {
                            WorkstateError::new(
                                ErrorCategory::Platform,
                                "the focused window references an unknown desktop workspace",
                            )
                            .with_context("workspace_identity", identity)
                        });
                }
                [] => {}
                _ => {
                    return Err(WorkstateError::new(
                        ErrorCategory::Platform,
                        "the current desktop workspace is ambiguous",
                    )
                    .with_context("focused_windows", focused_windows.len().to_string()));
                }
            }

            let focused_workspaces = snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.focused)
                .cloned()
                .collect::<Vec<_>>();
            match focused_workspaces.as_slice() {
                [workspace] => Ok(DesktopWorkspaceResolution::existing(workspace.clone())),
                [] => Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "the focused desktop workspace could not be determined",
                )),
                _ => Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "the current desktop workspace is ambiguous",
                )
                .with_context("focused_workspaces", focused_workspaces.len().to_string())),
            }
        }
        WorkspaceTarget::Existing { reference } => {
            let matches = snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace_matches_reference(workspace, reference))
                .cloned()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [workspace] => Ok(DesktopWorkspaceResolution::existing(workspace.clone())),
                [] => Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "the requested desktop workspace was not found",
                )
                .with_context("reference", workspace_reference_label(reference))),
                _ => Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "the requested desktop workspace is ambiguous",
                )
                .with_context("reference", workspace_reference_label(reference))
                .with_context("matches", matches.len().to_string())),
            }
        }
        WorkspaceTarget::NextEmpty => {
            if snapshot
                .windows
                .iter()
                .any(|window| window.workspace_identity.is_none())
            {
                return Err(WorkstateError::new(
                    ErrorCategory::Platform,
                    "an empty desktop workspace cannot be determined because a window has an unknown workspace",
                ));
            }
            let mut workspaces = snapshot.workspaces.clone();
            workspaces.sort_by(|left, right| {
                left.position
                    .unwrap_or(u32::MAX)
                    .cmp(&right.position.unwrap_or(u32::MAX))
                    .then_with(|| left.identity.cmp(&right.identity))
            });
            let occupied = snapshot
                .windows
                .iter()
                .filter_map(|window| window.workspace_identity.as_deref())
                .collect::<BTreeSet<_>>();
            workspaces
                .into_iter()
                .find(|workspace| !occupied.contains(workspace.identity.as_str()))
                .map(DesktopWorkspaceResolution::existing)
                .ok_or_else(|| {
                    WorkstateError::new(
                        ErrorCategory::Platform,
                        "no empty desktop workspace is currently available",
                    )
                })
        }
        WorkspaceTarget::Create { .. } => Err(WorkstateError::new(
            ErrorCategory::Platform,
            "the requested desktop workspace must be created before it can be resolved",
        )),
    }
}

pub fn ensure_workspace<'a>(
    backend: &'a dyn DesktopBackend,
    target: WorkspaceTarget,
    cancellation: CancellationToken,
    timeout: Duration,
) -> BoxFuture<'a, Result<DesktopWorkspaceResolution>> {
    Box::pin(async move {
        if timeout.is_zero() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "desktop workspace resolution timeout must be greater than zero",
            ));
        }
        cancellation.check()?;
        let snapshot = backend.snapshot().await?;
        if let WorkspaceTarget::Create { name } = &target {
            let named = snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.name.as_deref() == Some(name.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            match named.as_slice() {
                [workspace] => return Ok(DesktopWorkspaceResolution::existing(workspace.clone())),
                [] => {}
                _ => {
                    return Err(WorkstateError::new(
                        ErrorCategory::Platform,
                        "the requested desktop workspace name is ambiguous",
                    )
                    .with_context("name", name.clone())
                    .with_context("matches", named.len().to_string()));
                }
            }
            let creation = backend.create_workspace(name).await?;
            let created_by_run = match creation.status {
                DesktopOperationStatus::Created => true,
                DesktopOperationStatus::AlreadyPresent
                | DesktopOperationStatus::Reused
                | DesktopOperationStatus::Unchanged => false,
                DesktopOperationStatus::Changed => false,
                DesktopOperationStatus::Unavailable | DesktopOperationStatus::Ambiguous => {
                    return Err(WorkstateError::new(
                        ErrorCategory::Platform,
                        "the desktop backend did not confirm workspace creation",
                    )
                    .with_context("workspace_name", name.clone())
                    .with_context("operation_status", format!("{:?}", creation.status)));
                }
            };
            let wait = async {
                loop {
                    cancellation.check()?;
                    let refreshed = backend.snapshot().await?;
                    let matches = refreshed
                        .workspaces
                        .iter()
                        .filter(|workspace| workspace.name.as_deref() == Some(name.as_str()))
                        .cloned()
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [workspace] => {
                            return Ok(if created_by_run {
                                DesktopWorkspaceResolution::created(workspace.clone())
                            } else {
                                DesktopWorkspaceResolution::existing(workspace.clone())
                            });
                        }
                        [] => tokio::time::sleep(Duration::from_millis(25)).await,
                        _ => {
                            return Err(WorkstateError::new(
                                ErrorCategory::Platform,
                                "the created desktop workspace is ambiguous",
                            )
                            .with_context("name", name.clone())
                            .with_context("matches", matches.len().to_string()));
                        }
                    }
                }
            };
            return tokio::time::timeout(timeout, wait).await.map_err(|_| {
                WorkstateError::new(
                    ErrorCategory::Runtime,
                    "the created desktop workspace did not become observable before the timeout",
                )
            })?;
        }
        match resolve_workspace_target(&snapshot, &target) {
            Ok(resolution) => Ok(resolution),
            Err(initial_error) => {
                let refreshed = backend.snapshot().await?;
                resolve_workspace_target(&refreshed, &target).map_err(|retry_error| {
                    retry_error.with_context("initial_resolution_error", initial_error.render())
                })
            }
        }
    })
}

fn workspace_matches_reference(
    workspace: &DesktopWorkspaceSnapshot,
    reference: &WorkspaceReference,
) -> bool {
    match reference {
        WorkspaceReference::Name(name) => workspace.name.as_deref() == Some(name.as_str()),
        WorkspaceReference::Identifier(identity) => workspace.identity == *identity,
    }
}

fn workspace_reference_label(reference: &WorkspaceReference) -> String {
    match reference {
        WorkspaceReference::Name(name) => format!("name:{name}"),
        WorkspaceReference::Identifier(identity) => format!("identifier:{identity}"),
    }
}
