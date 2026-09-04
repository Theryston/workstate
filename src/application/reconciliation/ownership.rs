use std::collections::{BTreeMap, BTreeSet};

use crate::{
    application::ports::persistence::{ConfigStore, StateStore},
    domain::{
        CleanupPolicy, EnvironmentSlug, MutationRecord, OwnershipStatus, ResourceIdentity,
        ResourceRecord, RuntimeState,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupDecision {
    Clean,
    PreserveByPolicy,
    PreservePreExisting,
    PreserveShared,
    PreserveUnknown,
}

impl CleanupDecision {
    pub const fn is_cleanup_allowed(self) -> bool {
        matches!(self, Self::Clean)
    }

    pub const fn is_safe_preservation(self) -> bool {
        !matches!(self, Self::PreserveUnknown)
    }
}

#[derive(Debug, Clone)]
pub struct OwnershipRegistry {
    environment: EnvironmentSlug,
    shared_resources: BTreeSet<ResourceIdentity>,
    shared_mutation_targets: BTreeSet<String>,
    uncertain_environments: BTreeSet<EnvironmentSlug>,
}

impl OwnershipRegistry {
    pub fn load(
        environment: &EnvironmentSlug,
        config_store: &dyn ConfigStore,
        state_store: &dyn StateStore,
    ) -> Result<Self> {
        let mut shared_resources = BTreeSet::new();
        let mut shared_mutation_targets = BTreeSet::new();
        let mut uncertain_environments = BTreeSet::new();

        for candidate in config_store.list()? {
            if candidate == *environment {
                continue;
            }
            match state_store.load(&candidate) {
                Ok(Some(state)) if state.status.is_active() => {
                    shared_resources
                        .extend(state.resources.into_iter().map(|record| record.resource));
                    shared_mutation_targets.extend(
                        state
                            .mutations
                            .into_iter()
                            .filter(|mutation| !mutation.restored)
                            .map(|mutation| mutation.target),
                    );
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(_) => {
                    uncertain_environments.insert(candidate);
                }
            }
        }

        Ok(Self {
            environment: environment.clone(),
            shared_resources,
            shared_mutation_targets,
            uncertain_environments,
        })
    }

    pub fn environment(&self) -> &EnvironmentSlug {
        &self.environment
    }

    pub fn shared_resources(&self) -> &BTreeSet<ResourceIdentity> {
        &self.shared_resources
    }

    pub fn uncertain_environments(&self) -> &BTreeSet<EnvironmentSlug> {
        &self.uncertain_environments
    }

    pub fn shared_mutation_targets(&self) -> &BTreeSet<String> {
        &self.shared_mutation_targets
    }

    pub fn classify(
        &self,
        mut record: ResourceRecord,
        current_state: &RuntimeState,
    ) -> ResourceRecord {
        let identity = record.resource.clone();
        let ownership = if self.shared_resources.contains(&identity) {
            OwnershipStatus::Shared
        } else if let Some(previous) = current_state.resource(&identity) {
            previous.ownership
        } else if !self.uncertain_environments.is_empty()
            && record.ownership == OwnershipStatus::Unknown
        {
            OwnershipStatus::Unknown
        } else {
            record.ownership
        };
        record.ownership = ownership;
        record
    }

    pub fn classify_mutation(
        &self,
        mut mutation: MutationRecord,
        current_state: &RuntimeState,
    ) -> MutationRecord {
        if self.shared_mutation_targets.contains(&mutation.target)
            || mutation
                .resource
                .as_ref()
                .is_some_and(|resource| self.shared_resources.contains(resource))
        {
            mutation.ownership = OwnershipStatus::Shared;
        } else if let Some(previous) = current_state
            .mutations
            .iter()
            .find(|candidate| candidate.target == mutation.target)
        {
            mutation.ownership = previous.ownership;
        }
        mutation
    }

    pub fn cleanup_decision(&self, record: &ResourceRecord) -> CleanupDecision {
        if record.cleanup_policy == CleanupPolicy::Preserve {
            return CleanupDecision::PreserveByPolicy;
        }
        if self.shared_resources.contains(&record.resource) {
            return CleanupDecision::PreserveShared;
        }
        if !self.uncertain_environments.is_empty() && record.ownership.is_environment_owned() {
            return CleanupDecision::PreserveUnknown;
        }
        if record.is_cleanup_candidate() {
            return CleanupDecision::Clean;
        }
        if record.ownership.is_preserved() {
            if record.ownership == OwnershipStatus::Unknown {
                return CleanupDecision::PreserveUnknown;
            }
            return CleanupDecision::PreservePreExisting;
        }
        CleanupDecision::PreserveUnknown
    }

    pub fn cleanup_resources(
        &self,
        records: &[ResourceRecord],
    ) -> (
        Vec<ResourceRecord>,
        Vec<(ResourceIdentity, CleanupDecision)>,
    ) {
        let mut clean = Vec::new();
        let mut preserved = Vec::new();
        for record in records {
            let decision = self.cleanup_decision(record);
            if decision.is_cleanup_allowed() {
                clean.push(record.clone());
            } else {
                preserved.push((record.resource.clone(), decision));
            }
        }
        (clean, preserved)
    }

    pub fn diagnostics(&self) -> BTreeMap<String, String> {
        let mut diagnostics = BTreeMap::new();
        if !self.uncertain_environments.is_empty() {
            diagnostics.insert(
                "uncertain_environments".to_owned(),
                self.uncertain_environments
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        diagnostics
    }

    pub fn ensure_safe_for_cleanup(&self) -> Result<()> {
        if self.uncertain_environments.is_empty() {
            return Ok(());
        }
        Err(WorkstateError::new(
            ErrorCategory::Persistence,
            "ownership could not be determined for every active environment",
        )
        .with_context(
            "uncertain_environments",
            self.uncertain_environments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ))
    }
}
