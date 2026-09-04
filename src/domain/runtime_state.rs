use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    ActionId, CleanupStatus, DomainError, EnvironmentSlug, MutationRecord, ResourceIdentity,
    ResourceRecord, RunStatus,
};

pub const CURRENT_RUNTIME_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    #[serde(default = "default_runtime_schema_version")]
    pub schema_version: u32,
    pub environment_slug: EnvironmentSlug,
    pub run_id: String,
    pub status: RunStatus,
    #[serde(default)]
    pub started_at_unix_milliseconds: Option<u64>,
    #[serde(default)]
    pub updated_at_unix_milliseconds: Option<u64>,
    #[serde(default)]
    pub resources: Vec<ResourceRecord>,
    #[serde(default)]
    pub mutations: Vec<MutationRecord>,
    #[serde(default)]
    pub active_tasks: Vec<ActionId>,
    #[serde(default)]
    pub cleanup_status: CleanupStatus,
}

impl RuntimeState {
    pub fn new(environment_slug: EnvironmentSlug, run_id: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            environment_slug,
            run_id: run_id.into(),
            status: RunStatus::Planning,
            started_at_unix_milliseconds: None,
            updated_at_unix_milliseconds: None,
            resources: Vec::new(),
            mutations: Vec::new(),
            active_tasks: Vec::new(),
            cleanup_status: CleanupStatus::default(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != CURRENT_RUNTIME_SCHEMA_VERSION {
            return Err(DomainError::UnsupportedRuntimeSchema {
                actual: self.schema_version,
                expected: CURRENT_RUNTIME_SCHEMA_VERSION,
            });
        }

        if self.run_id.is_empty() || self.run_id.contains('\0') {
            return Err(DomainError::InvalidIdentifier {
                kind: "run".to_owned(),
                value: self.run_id.clone(),
            });
        }

        let mut resources = BTreeSet::new();
        for record in &self.resources {
            if !resources.insert(record.resource.clone()) {
                return Err(DomainError::DuplicateRuntimeResource {
                    identity: record.resource.to_string(),
                });
            }
        }

        let mut mutations = BTreeSet::new();
        for record in &self.mutations {
            if !mutations.insert(record.target.clone()) {
                return Err(DomainError::DuplicateRuntimeMutation {
                    target: record.target.clone(),
                });
            }
        }

        Ok(())
    }

    pub fn record_resource(&mut self, record: ResourceRecord) -> Result<(), DomainError> {
        if self
            .resources
            .iter()
            .any(|item| item.resource == record.resource)
        {
            return Err(DomainError::DuplicateRuntimeResource {
                identity: record.resource.to_string(),
            });
        }

        self.resources.push(record);
        Ok(())
    }

    pub fn upsert_resource(&mut self, mut record: ResourceRecord) -> Result<(), DomainError> {
        if let Some(existing) = self
            .resources
            .iter_mut()
            .find(|item| item.resource == record.resource)
        {
            existing.observed_before |= record.observed_before;
            existing.ownership = existing.ownership.merge(record.ownership);
            if existing.action_id.is_none() {
                existing.action_id = record.action_id.take();
            }
            if existing.cleanup_policy == super::CleanupPolicy::Preserve {
                record.cleanup_policy = super::CleanupPolicy::Preserve;
            }
            existing.cleanup_policy = record.cleanup_policy;
            existing
                .integration_metadata
                .append(&mut record.integration_metadata);
            return Ok(());
        }

        self.resources.push(record);
        Ok(())
    }

    pub fn resource(&self, identity: &ResourceIdentity) -> Option<&ResourceRecord> {
        self.resources
            .iter()
            .find(|record| &record.resource == identity)
    }

    pub fn record_mutation(&mut self, record: MutationRecord) -> Result<(), DomainError> {
        if self
            .mutations
            .iter()
            .any(|item| item.target == record.target)
        {
            return Err(DomainError::DuplicateRuntimeMutation {
                target: record.target,
            });
        }

        self.mutations.push(record);
        Ok(())
    }

    pub fn upsert_mutation(&mut self, record: MutationRecord) -> Result<(), DomainError> {
        if let Some(existing) = self
            .mutations
            .iter_mut()
            .find(|mutation| mutation.target == record.target)
        {
            *existing = record;
            return Ok(());
        }
        self.mutations.push(record);
        Ok(())
    }

    pub fn mutation_mut(&mut self, target: &str) -> Option<&mut MutationRecord> {
        self.mutations
            .iter_mut()
            .find(|mutation| mutation.target == target)
    }

    pub fn set_status(&mut self, status: RunStatus) {
        self.status = status;
    }

    pub fn transition_to(&mut self, status: RunStatus) -> Result<(), DomainError> {
        if self.status == status {
            return Ok(());
        }
        if !self.status.can_transition_to(status) {
            return Err(DomainError::InvalidLifecycleTransition {
                from: self.status.to_string(),
                to: status.to_string(),
            });
        }
        self.status = status;
        Ok(())
    }

    pub fn begin_run(&mut self, run_id: impl Into<String>) -> Result<(), DomainError> {
        let run_id = run_id.into();
        if run_id.is_empty() || run_id.contains('\0') {
            return Err(DomainError::InvalidIdentifier {
                kind: "run".to_owned(),
                value: run_id,
            });
        }
        self.run_id = run_id;
        self.active_tasks.clear();
        self.cleanup_status = CleanupStatus::default();
        self.transition_to(RunStatus::Planning)
    }

    pub fn set_cleanup_status(&mut self, status: CleanupStatus) {
        self.cleanup_status = status;
    }

    pub fn set_started_at(&mut self, timestamp: u64) {
        self.started_at_unix_milliseconds = Some(timestamp);
    }

    pub fn set_updated_at(&mut self, timestamp: u64) {
        self.updated_at_unix_milliseconds = Some(timestamp);
    }

    pub fn add_active_task(&mut self, action_id: ActionId) {
        if !self.active_tasks.contains(&action_id) {
            self.active_tasks.push(action_id);
        }
    }

    pub fn remove_active_task(&mut self, action_id: &ActionId) {
        self.active_tasks.retain(|item| item != action_id);
    }
}

fn default_runtime_schema_version() -> u32 {
    CURRENT_RUNTIME_SCHEMA_VERSION
}
