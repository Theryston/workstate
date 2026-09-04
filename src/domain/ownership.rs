use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ActionId, CleanupPolicy, DomainError, ResourceIdentity};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    PreExisting,
    CreatedByCurrentRun,
    CreatedByEnvironment,
    ReusedExisting,
    Shared,
    #[default]
    Unknown,
}

impl OwnershipStatus {
    pub fn is_environment_owned(self) -> bool {
        matches!(self, Self::CreatedByCurrentRun | Self::CreatedByEnvironment)
    }

    pub fn is_preserved(self) -> bool {
        !self.is_environment_owned()
    }

    pub fn merge(self, other: Self) -> Self {
        use OwnershipStatus::{
            CreatedByCurrentRun, CreatedByEnvironment, PreExisting, ReusedExisting, Shared, Unknown,
        };

        match (self, other) {
            (Shared, _) | (_, Shared) => Shared,
            (Unknown, _) | (_, Unknown) => Unknown,
            (PreExisting, _) | (_, PreExisting) => PreExisting,
            (CreatedByEnvironment, _) | (_, CreatedByEnvironment) => CreatedByEnvironment,
            (ReusedExisting, _) | (_, ReusedExisting) => ReusedExisting,
            (CreatedByCurrentRun, CreatedByCurrentRun) => CreatedByCurrentRun,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRecord {
    #[serde(default)]
    pub action_id: Option<ActionId>,
    pub resource: ResourceIdentity,
    pub observed_before: bool,
    pub ownership: OwnershipStatus,
    #[serde(default)]
    pub cleanup_policy: CleanupPolicy,
    #[serde(default)]
    pub integration_metadata: BTreeMap<String, String>,
}

impl ResourceRecord {
    pub fn new(resource: ResourceIdentity, ownership: OwnershipStatus) -> Self {
        Self {
            action_id: None,
            resource,
            observed_before: false,
            ownership,
            cleanup_policy: CleanupPolicy::default(),
            integration_metadata: BTreeMap::new(),
        }
    }

    pub fn with_action(mut self, action_id: ActionId) -> Self {
        self.action_id = Some(action_id);
        self
    }

    pub fn is_cleanup_candidate(&self) -> bool {
        self.cleanup_policy == CleanupPolicy::OwnedOnly && self.ownership.is_environment_owned()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompensationOperation {
    None,
    RestoreValue,
    #[default]
    Handler,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorationStatus {
    #[default]
    Pending,
    Restored,
    NotRequired,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRecord {
    #[serde(default)]
    pub action_id: Option<ActionId>,
    pub target: String,
    #[serde(default)]
    pub resource: Option<ResourceIdentity>,
    #[serde(default)]
    pub previous_value: Option<String>,
    #[serde(default)]
    pub applied_value: Option<String>,
    #[serde(default)]
    pub ownership: OwnershipStatus,
    #[serde(default)]
    pub compensation: CompensationOperation,
    #[serde(default)]
    pub cleanup_policy: CleanupPolicy,
    pub restored: bool,
    #[serde(default)]
    pub restoration_status: RestorationStatus,
}

impl MutationRecord {
    pub fn new(target: impl Into<String>) -> Result<Self, DomainError> {
        let target = target.into();
        if target.is_empty() || target.contains('\0') {
            return Err(DomainError::InvalidIdentifier {
                kind: "mutation target".to_owned(),
                value: target,
            });
        }

        Ok(Self {
            action_id: None,
            target,
            resource: None,
            previous_value: None,
            applied_value: None,
            ownership: OwnershipStatus::CreatedByCurrentRun,
            compensation: CompensationOperation::Handler,
            cleanup_policy: CleanupPolicy::OwnedOnly,
            restored: false,
            restoration_status: RestorationStatus::Pending,
        })
    }

    pub fn mark_restored(&mut self) {
        self.restored = true;
        self.restoration_status = RestorationStatus::Restored;
    }

    pub fn mark_not_required(&mut self) {
        self.restored = true;
        self.restoration_status = RestorationStatus::NotRequired;
    }

    pub fn mark_restore_failed(&mut self) {
        self.restored = false;
        self.restoration_status = RestorationStatus::Failed;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Stopped,
    Planning,
    Active,
    Ready,
    Partial,
    RollingBack,
    RollbackFailed,
    Stopping,
    Deleting,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Planning => "planning",
            Self::Active => "active",
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::RollingBack => "rolling_back",
            Self::RollbackFailed => "rollback_failed",
            Self::Stopping => "stopping",
            Self::Deleting => "deleting",
        }
    }

    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Stopped)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Stopped => matches!(next, Self::Planning | Self::Stopping | Self::Deleting),
            Self::Planning => matches!(
                next,
                Self::Active | Self::Partial | Self::RollingBack | Self::Stopping | Self::Stopped
            ),
            Self::Active => matches!(
                next,
                Self::Planning
                    | Self::Ready
                    | Self::Partial
                    | Self::RollingBack
                    | Self::Stopping
                    | Self::Deleting
            ),
            Self::Ready => matches!(next, Self::Planning | Self::Stopping | Self::Deleting),
            Self::Partial => matches!(
                next,
                Self::Planning
                    | Self::RollingBack
                    | Self::Stopping
                    | Self::Deleting
                    | Self::Stopped
            ),
            Self::RollingBack => matches!(
                next,
                Self::Planning | Self::Stopping | Self::Stopped | Self::RollbackFailed
            ),
            Self::RollbackFailed => {
                matches!(next, Self::Planning | Self::Stopping | Self::Deleting)
            }
            Self::Stopping => matches!(
                next,
                Self::Stopped | Self::Partial | Self::RollbackFailed | Self::Deleting
            ),
            Self::Deleting => matches!(
                next,
                Self::Planning
                    | Self::Stopping
                    | Self::Stopped
                    | Self::Partial
                    | Self::RollbackFailed
            ),
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    #[default]
    NotRequired,
    Pending,
    InProgress,
    Complete,
    Failed {
        errors: Vec<String>,
    },
}
