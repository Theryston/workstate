use crate::{
    error::{ErrorCategory, Result, WorkstateError},
    platform::DesktopEnvironment,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWorkspaceSnapshot {
    pub identity: String,
    pub name: Option<String>,
    pub tiling_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopWindowSnapshot {
    pub identity: String,
    pub application: Option<String>,
    pub project_path: Option<String>,
    pub workspace_identity: Option<String>,
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

pub trait DesktopBackend: Send + Sync {
    fn snapshot(&self) -> Result<DesktopSnapshot>;

    fn set_tiling(&self, _workspace_identity: &str, _enabled: bool) -> Result<()> {
        Err(WorkstateError::new(
            ErrorCategory::Platform,
            "desktop tiling mutation is not configured",
        ))
    }

    fn restore_tiling(&self, workspace_identity: &str, previous: bool) -> Result<()> {
        self.set_tiling(workspace_identity, previous)
    }
}

pub trait DesktopEnvironmentDetector: Send + Sync {
    fn detect(&self) -> Result<DesktopEnvironment>;
}
