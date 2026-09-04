use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use crate::{
    domain::{
        ActionGraph, ActionId, ActionKind, ActionSpec, CleanupPolicy, ExecutionMode,
        ReadinessCheck, RetryPolicy, WorkspaceId,
    },
    platform::CapabilityId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanClassification {
    AlreadyCorrect,
    RequiresChange,
    BlockedByMissingCapability,
    Invalid,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStrategy {
    Handler { action_key: String },
    NotAvailable { reason: String },
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub action: ActionSpec,
    pub action_id: ActionId,
    pub action_kind: ActionKind,
    pub dependencies: Vec<ActionId>,
    pub execution_mode: Option<ExecutionMode>,
    pub working_directory: Option<String>,
    pub desktop_workspace: Option<WorkspaceId>,
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub observation_strategy: PlanStrategy,
    pub apply_strategy: PlanStrategy,
    pub readiness_checks: Vec<ReadinessCheck>,
    pub compensation_strategy: PlanStrategy,
    pub cleanup_policy: CleanupPolicy,
    pub timeout: Option<Duration>,
    pub retry_policy: RetryPolicy,
    pub classification: PlanClassification,
    pub classification_detail: Option<String>,
    pub missing_capabilities: Vec<CapabilityId>,
    pub observed_resources: Vec<crate::domain::ResourceRecord>,
}

impl PlanEntry {
    pub(crate) fn from_action(
        action: ActionSpec,
        required_capabilities: BTreeSet<CapabilityId>,
        strategy: PlanStrategy,
        classification: PlanClassification,
        classification_detail: Option<String>,
        missing_capabilities: Vec<CapabilityId>,
    ) -> Self {
        Self {
            action_id: action.id.clone(),
            action_kind: action.kind.clone(),
            dependencies: action.depends_on.clone(),
            execution_mode: action.execution_mode,
            working_directory: action.working_directory.clone(),
            desktop_workspace: action.desktop_workspace.clone(),
            required_capabilities,
            observation_strategy: strategy.clone(),
            apply_strategy: strategy.clone(),
            readiness_checks: action.readiness_checks.clone(),
            compensation_strategy: strategy,
            cleanup_policy: action.cleanup_policy,
            timeout: action
                .timeout
                .as_ref()
                .map(|timeout| Duration::from_millis(timeout.milliseconds)),
            retry_policy: action.retry_policy.clone(),
            classification,
            classification_detail,
            missing_capabilities,
            observed_resources: Vec::new(),
            action,
        }
    }

    pub fn is_runnable(&self) -> bool {
        matches!(
            self.classification,
            PlanClassification::AlreadyCorrect | PlanClassification::RequiresChange
        )
    }

    pub fn requires_change(&self) -> bool {
        self.classification == PlanClassification::RequiresChange
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub environment: crate::domain::EnvironmentSlug,
    entries: BTreeMap<ActionId, PlanEntry>,
    ordered_action_ids: Vec<ActionId>,
}

impl ExecutionPlan {
    pub(crate) fn new(
        environment: crate::domain::EnvironmentSlug,
        graph: &ActionGraph,
        entries: BTreeMap<ActionId, PlanEntry>,
    ) -> Self {
        Self {
            environment,
            ordered_action_ids: graph.ordered_action_ids().to_vec(),
            entries,
        }
    }

    pub fn ordered_action_ids(&self) -> &[ActionId] {
        &self.ordered_action_ids
    }

    pub fn entries(&self) -> impl Iterator<Item = &PlanEntry> {
        self.ordered_action_ids
            .iter()
            .filter_map(|action_id| self.entries.get(action_id))
    }

    pub fn entry(&self, action_id: &ActionId) -> Option<&PlanEntry> {
        self.entries.get(action_id)
    }

    pub(crate) fn entry_mut(&mut self, action_id: &ActionId) -> Option<&mut PlanEntry> {
        self.entries.get_mut(action_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_observed(&self) -> bool {
        self.entries
            .values()
            .all(|entry| !matches!(entry.classification, PlanClassification::Unknown))
    }

    pub fn expected_mutation_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.requires_change())
            .count()
    }
}
