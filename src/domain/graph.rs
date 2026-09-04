use std::collections::{BTreeMap, BTreeSet};

use super::{ActionId, ActionSpec, DomainError, WorkspaceId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionGraph {
    actions: BTreeMap<ActionId, ActionSpec>,
    ordered_action_ids: Vec<ActionId>,
    configuration_indices: BTreeMap<ActionId, usize>,
    dependents: BTreeMap<ActionId, Vec<ActionId>>,
}

impl ActionGraph {
    pub fn validate(
        actions: &[ActionSpec],
        workspace_ids: &BTreeSet<WorkspaceId>,
    ) -> Result<Self, DomainError> {
        let mut action_map = BTreeMap::new();
        let mut configuration_indices = BTreeMap::new();

        for (index, action) in actions.iter().enumerate() {
            action.validate()?;
            if action_map
                .insert(action.id.clone(), action.clone())
                .is_some()
            {
                return Err(DomainError::DuplicateActionId {
                    id: action.id.to_string(),
                });
            }
            configuration_indices.insert(action.id.clone(), index);
        }

        for action in actions {
            if let Some(workspace_id) = &action.desktop_workspace
                && !workspace_ids.contains(workspace_id)
            {
                return Err(DomainError::MissingWorkspaceReference {
                    action_id: action.id.to_string(),
                    workspace_id: workspace_id.to_string(),
                });
            }

            if let Some(workspace_id) = &action.parameters.workspace_id
                && !workspace_ids.contains(workspace_id)
            {
                return Err(DomainError::MissingWorkspaceReference {
                    action_id: action.id.to_string(),
                    workspace_id: workspace_id.to_string(),
                });
            }

            let mut dependencies = BTreeSet::new();
            for dependency_id in &action.depends_on {
                if dependency_id == &action.id {
                    return Err(DomainError::SelfDependency {
                        action_id: action.id.to_string(),
                    });
                }

                if !dependencies.insert(dependency_id.clone()) {
                    return Err(DomainError::DuplicateDependency {
                        action_id: action.id.to_string(),
                        dependency_id: dependency_id.to_string(),
                    });
                }

                if !action_map.contains_key(dependency_id) {
                    return Err(DomainError::MissingDependency {
                        action_id: action.id.to_string(),
                        dependency_id: dependency_id.to_string(),
                    });
                }
            }
        }

        let mut indegree = BTreeMap::new();
        let mut dependents: BTreeMap<ActionId, Vec<ActionId>> = BTreeMap::new();

        for action in actions {
            indegree.insert(action.id.clone(), action.depends_on.len());
            for dependency_id in &action.depends_on {
                dependents
                    .entry(dependency_id.clone())
                    .or_default()
                    .push(action.id.clone());
            }
        }

        let mut ready = BTreeSet::new();
        for (action_id, count) in &indegree {
            if *count == 0 {
                let Some(index) = configuration_indices.get(action_id) else {
                    return Err(DomainError::GraphInvariant {
                        message: format!("missing configuration index for {action_id}"),
                    });
                };
                ready.insert((*index, action_id.clone()));
            }
        }

        let mut ordered_action_ids = Vec::with_capacity(actions.len());
        while let Some((index, action_id)) = ready.first().cloned() {
            ready.remove(&(index, action_id.clone()));
            ordered_action_ids.push(action_id.clone());

            if let Some(dependent_ids) = dependents.get(&action_id) {
                for dependent_id in dependent_ids {
                    let count = match indegree.get_mut(dependent_id) {
                        Some(count) => count,
                        None => {
                            return Err(DomainError::GraphInvariant {
                                message: format!("missing indegree entry for {dependent_id}"),
                            });
                        }
                    };
                    *count -= 1;
                    if *count == 0 {
                        let Some(index) = configuration_indices.get(dependent_id) else {
                            return Err(DomainError::GraphInvariant {
                                message: format!("missing configuration index for {dependent_id}"),
                            });
                        };
                        ready.insert((*index, dependent_id.clone()));
                    }
                }
            }
        }

        if ordered_action_ids.len() != actions.len() {
            let remaining = indegree
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(action_id, _)| action_id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DomainError::DependencyCycle { actions: remaining });
        }

        Ok(Self {
            actions: action_map,
            ordered_action_ids,
            configuration_indices,
            dependents,
        })
    }

    pub fn ordered_action_ids(&self) -> &[ActionId] {
        &self.ordered_action_ids
    }

    pub fn action(&self, action_id: &ActionId) -> Option<&ActionSpec> {
        self.actions.get(action_id)
    }

    pub fn configuration_index(&self, action_id: &ActionId) -> Option<usize> {
        self.configuration_indices.get(action_id).copied()
    }

    pub fn dependencies_of(&self, action_id: &ActionId) -> Option<&[ActionId]> {
        self.actions
            .get(action_id)
            .map(|action| action.depends_on.as_slice())
    }

    pub fn dependents_of(&self, action_id: &ActionId) -> &[ActionId] {
        self.dependents
            .get(action_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn actions(&self) -> impl Iterator<Item = &ActionSpec> {
        self.ordered_action_ids
            .iter()
            .filter_map(|action_id| self.actions.get(action_id))
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::ActionGraph;
    use crate::domain::{
        ActionId, ActionKind, ActionParameters, ActionSpec, CommandSpec, ExecutionMode, WorkspaceId,
    };

    fn command_action(id: &str) -> Option<ActionSpec> {
        let mut action = ActionSpec::new(id, ActionKind::RunCommand).ok()?;
        action.parameters = ActionParameters {
            command: Some(CommandSpec::new("printf")),
            ..ActionParameters::default()
        };
        action.execution_mode = Some(ExecutionMode::RunOnce);
        Some(action)
    }

    #[test]
    fn orders_ready_actions_deterministically() {
        let Some(first) = command_action("first") else {
            return;
        };
        let Some(mut second) = command_action("second") else {
            return;
        };
        let Some(mut third) = command_action("third") else {
            return;
        };
        let Some(first_id_for_third) = ActionId::new("first").ok() else {
            return;
        };
        let Some(first_id_for_second) = ActionId::new("first").ok() else {
            return;
        };
        third.depends_on.push(first_id_for_third);
        second.depends_on.push(first_id_for_second);

        let graph = ActionGraph::validate(&[first, second, third], &BTreeSet::new());
        assert!(graph.is_ok());
        let Some(graph) = graph.ok() else {
            return;
        };
        let order = graph
            .ordered_action_ids()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["first", "second", "third"]);
    }

    #[test]
    fn uses_configuration_order_before_action_id_for_equal_priority_actions() {
        let Some(last_configured) = command_action("z-last") else {
            return;
        };
        let Some(first_configured) = command_action("a-first") else {
            return;
        };

        let graph = ActionGraph::validate(&[last_configured, first_configured], &BTreeSet::new());
        assert!(graph.is_ok());
        let Some(graph) = graph.ok() else {
            return;
        };

        let order = graph
            .ordered_action_ids()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["z-last", "a-first"]);
    }

    #[test]
    fn rejects_missing_dependencies_and_cycles() {
        let Some(mut missing) = command_action("missing") else {
            return;
        };
        let Some(unknown_id) = ActionId::new("unknown").ok() else {
            return;
        };
        missing.depends_on.push(unknown_id);
        let missing_result = ActionGraph::validate(&[missing], &BTreeSet::new());
        assert!(missing_result.is_err());

        let Some(mut first) = command_action("first") else {
            return;
        };
        let Some(mut second) = command_action("second") else {
            return;
        };
        let Some(second_id) = ActionId::new("second").ok() else {
            return;
        };
        let Some(first_id) = ActionId::new("first").ok() else {
            return;
        };
        first.depends_on.push(second_id);
        second.depends_on.push(first_id);
        let cycle_result = ActionGraph::validate(&[first, second], &BTreeSet::new());
        assert!(cycle_result.is_err());
    }

    #[test]
    fn rejects_an_action_that_references_an_unknown_workspace() {
        let Some(mut action) = command_action("api") else {
            return;
        };
        let Some(workspace_id) = WorkspaceId::new("missing").ok() else {
            return;
        };
        action.desktop_workspace = Some(workspace_id);

        let result = ActionGraph::validate(&[action], &BTreeSet::new());

        assert!(result.is_err());
    }
}
