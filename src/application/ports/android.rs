use std::time::Duration;

use super::process::BoxFuture;
use crate::{
    application::planner::CancellationToken,
    domain::{
        ActionId, CleanupPolicy, EmulatorSpec, EnvironmentSlug, MutationRecord, ResourceRecord,
        WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidVirtualDevice {
    pub name: String,
}

impl AndroidVirtualDevice {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Android Virtual Device name must be non-empty and contain no control characters",
            ));
        }
        Ok(Self { name })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AndroidDeviceState {
    Device,
    Offline,
    Unauthorized,
    NoPermissions,
    Unknown(String),
}

impl AndroidDeviceState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Device)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidDeviceSnapshot {
    pub serial: String,
    pub avd: Option<String>,
    pub state: AndroidDeviceState,
    pub boot_completed: bool,
}

impl AndroidDeviceSnapshot {
    pub fn is_ready(&self) -> bool {
        self.state.is_connected() && self.boot_completed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorRuntimeSnapshot {
    pub avd: String,
    pub serial: String,
    pub state: AndroidDeviceState,
    pub boot_completed: bool,
    pub process_identity: Option<String>,
    pub window_identity: Option<String>,
    pub workspace_identity: Option<String>,
}

impl EmulatorRuntimeSnapshot {
    pub fn is_ready(&self) -> bool {
        self.state.is_connected() && self.boot_completed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmulatorObservation {
    Missing,
    Present(EmulatorRuntimeSnapshot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmulatorOperationStatus {
    Available,
    AlreadyRunning,
    Started,
    Booting,
    Ready,
    Missing,
    Ambiguous,
    Incompatible,
    TimedOut,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorActionContext {
    pub action_id: ActionId,
    pub environment: EnvironmentSlug,
    pub cleanup_policy: CleanupPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorRequest {
    pub context: EmulatorActionContext,
    pub specification: EmulatorSpec,
    pub workspace_target: Option<WorkspaceTarget>,
    pub timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorEnsureOutcome {
    pub status: EmulatorOperationStatus,
    pub runtime: Option<EmulatorRuntimeSnapshot>,
    pub resources: Vec<ResourceRecord>,
    pub mutations: Vec<MutationRecord>,
    pub outputs: Vec<String>,
}

impl EmulatorEnsureOutcome {
    pub fn new(status: EmulatorOperationStatus) -> Self {
        Self {
            status,
            runtime: None,
            resources: Vec::new(),
            mutations: Vec::new(),
            outputs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorCleanupOutcome {
    pub status: EmulatorOperationStatus,
    pub detail: Option<String>,
    pub outputs: Vec<String>,
}

impl EmulatorCleanupOutcome {
    pub fn new(status: EmulatorOperationStatus) -> Self {
        Self {
            status,
            detail: None,
            outputs: Vec::new(),
        }
    }
}

pub trait EmulatorBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;

    fn list_avds(&self) -> BoxFuture<'_, Result<Vec<AndroidVirtualDevice>>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Android Virtual Device listing is not configured",
            ))
        })
    }

    fn observe(
        &self,
        _request: EmulatorRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorObservation>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Android emulator observation is not configured",
            ))
        })
    }

    fn ensure(
        &self,
        _request: EmulatorRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorEnsureOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Android emulator lifecycle is not configured",
            ))
        })
    }

    fn stop_owned(
        &self,
        _request: EmulatorRequest,
        _resources: Vec<ResourceRecord>,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorCleanupOutcome>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Integration,
                "Android emulator cleanup is not configured",
            ))
        })
    }
}
