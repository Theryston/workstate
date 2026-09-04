use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ActionId, CleanupPolicy, DomainError, ResourceIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    PreExisting,
    CreatedByCurrentRun,
    CreatedByEnvironment,
    ReusedExisting,
    Shared,
    Unknown,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRecord {
    #[serde(default)]
    pub action_id: Option<ActionId>,
    pub target: String,
    #[serde(default)]
    pub previous_value: Option<String>,
    #[serde(default)]
    pub applied_value: Option<String>,
    pub restored: bool,
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
            previous_value: None,
            applied_value: None,
            restored: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Ready,
    Partial,
    RollingBack,
    RollbackFailed,
    Stopped,
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
