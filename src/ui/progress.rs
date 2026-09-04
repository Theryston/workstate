use std::{
    collections::{BTreeMap, VecDeque},
    fmt::{self, Display, Formatter},
    time::Duration,
};

use crate::{
    domain::{ActionId, EnvironmentConfig},
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionProgressStatus {
    Pending,
    Running,
    Ready,
    Skipped,
    Failed,
    RollingBack,
    Stopped,
}

impl Display for ActionProgressStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::RollingBack => "rolling back",
            Self::Stopped => "stopped",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEntry {
    pub action_id: ActionId,
    pub label: String,
    pub status: ActionProgressStatus,
    pub elapsed: Duration,
    pub timeout: Option<Duration>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressLog {
    pub action_id: Option<ActionId>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    ActionQueued {
        action_id: ActionId,
    },
    ActionStarted {
        action_id: ActionId,
    },
    ActionReady {
        action_id: ActionId,
    },
    ActionSkipped {
        action_id: ActionId,
        reason: String,
    },
    ActionFailed {
        action_id: ActionId,
        error: String,
    },
    RollbackStarted,
    RollbackAction {
        action_id: ActionId,
    },
    RollbackFinished {
        success: bool,
    },
    Log {
        action_id: Option<ActionId>,
        message: String,
    },
    ClockAdvanced {
        elapsed: Duration,
    },
    Completed {
        success: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressState {
    pub environment_name: String,
    entries: BTreeMap<ActionId, ProgressEntry>,
    logs: VecDeque<ProgressLog>,
    pub elapsed: Duration,
    pub finished: bool,
    pub successful: Option<bool>,
}

impl ProgressState {
    pub fn from_configuration(configuration: &EnvironmentConfig) -> Self {
        let entries = configuration
            .actions
            .iter()
            .map(|action| {
                let label = action
                    .display_label
                    .clone()
                    .unwrap_or_else(|| action.id.to_string());
                (
                    action.id.clone(),
                    ProgressEntry {
                        action_id: action.id.clone(),
                        label,
                        status: ActionProgressStatus::Pending,
                        elapsed: Duration::ZERO,
                        timeout: action
                            .timeout
                            .as_ref()
                            .map(|timeout| Duration::from_millis(timeout.milliseconds)),
                        detail: None,
                    },
                )
            })
            .collect();

        Self {
            environment_name: configuration.name.to_string(),
            entries,
            logs: VecDeque::new(),
            elapsed: Duration::ZERO,
            finished: false,
            successful: None,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &ProgressEntry> {
        self.entries.values()
    }

    pub fn entry(&self, action_id: &ActionId) -> Option<&ProgressEntry> {
        self.entries.get(action_id)
    }

    pub fn logs(&self) -> impl Iterator<Item = &ProgressLog> {
        self.logs.iter()
    }

    pub fn apply(&mut self, event: ProgressEvent) -> Result<()> {
        match event {
            ProgressEvent::ActionQueued { action_id } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Pending;
                    entry.detail = None;
                })?;
            }
            ProgressEvent::ActionStarted { action_id } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Running;
                    entry.detail = None;
                })?;
            }
            ProgressEvent::ActionReady { action_id } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Ready;
                    entry.detail = None;
                })?;
            }
            ProgressEvent::ActionSkipped { action_id, reason } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Skipped;
                    entry.detail = Some(reason);
                })?;
            }
            ProgressEvent::ActionFailed { action_id, error } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Failed;
                    entry.detail = Some(error);
                })?;
            }
            ProgressEvent::RollbackStarted => {
                for entry in self.entries.values_mut() {
                    if matches!(
                        entry.status,
                        ActionProgressStatus::Ready | ActionProgressStatus::Running
                    ) {
                        entry.status = ActionProgressStatus::RollingBack;
                    }
                }
            }
            ProgressEvent::RollbackAction { action_id } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::RollingBack;
                })?;
            }
            ProgressEvent::RollbackFinished { success } => {
                self.finished = true;
                self.successful = Some(success);
                if success {
                    for entry in self.entries.values_mut() {
                        if entry.status == ActionProgressStatus::RollingBack {
                            entry.status = ActionProgressStatus::Stopped;
                        }
                    }
                }
            }
            ProgressEvent::Log { action_id, message } => {
                self.logs.push_back(ProgressLog { action_id, message });
                while self.logs.len() > 200 {
                    self.logs.pop_front();
                }
            }
            ProgressEvent::ClockAdvanced { elapsed } => self.advance(elapsed),
            ProgressEvent::Completed { success } => {
                self.finished = true;
                self.successful = Some(success);
            }
        }

        Ok(())
    }

    pub fn advance(&mut self, duration: Duration) {
        self.elapsed = self.elapsed.saturating_add(duration);
        for entry in self.entries.values_mut() {
            if entry.status == ActionProgressStatus::Running {
                entry.elapsed = entry.elapsed.saturating_add(duration);
            }
        }
    }

    pub fn ready_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.status == ActionProgressStatus::Ready)
            .count()
    }

    pub fn total_count(&self) -> usize {
        self.entries.len()
    }

    fn update_action<F>(&mut self, action_id: &ActionId, update: F) -> Result<()>
    where
        F: FnOnce(&mut ProgressEntry),
    {
        let Some(entry) = self.entries.get_mut(action_id) else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("progress event referenced unknown action '{action_id}'"),
            )
            .with_context("action_id", action_id.to_string()));
        };
        update(entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::domain::{ActionKind, ActionSpec, EnvironmentConfig};

    use super::{ActionProgressStatus, ProgressEvent, ProgressState};

    fn configuration() -> Option<EnvironmentConfig> {
        let mut configuration = EnvironmentConfig::new("Personal Blog").ok()?;
        configuration
            .add_action(ActionSpec::new("api", ActionKind::RunCommand).ok()?)
            .ok()?;
        Some(configuration)
    }

    #[test]
    fn progress_consumes_typed_events_and_tracks_elapsed_time() {
        let Some(configuration) = configuration() else {
            return;
        };
        let mut state = ProgressState::from_configuration(&configuration);
        let Some(action) = configuration.actions.first() else {
            return;
        };
        assert!(
            state
                .apply(ProgressEvent::ActionStarted {
                    action_id: action.id.clone(),
                })
                .is_ok()
        );
        state.advance(Duration::from_millis(25));
        assert_eq!(state.elapsed, Duration::from_millis(25));
        assert_eq!(
            state.entry(&action.id).map(|entry| entry.status),
            Some(ActionProgressStatus::Running)
        );
        assert!(
            state
                .apply(ProgressEvent::ActionReady {
                    action_id: action.id.clone(),
                })
                .is_ok()
        );
        assert_eq!(state.ready_count(), 1);
    }

    #[test]
    fn unknown_action_events_are_errors() {
        let Some(configuration) = configuration() else {
            return;
        };
        let mut state = ProgressState::from_configuration(&configuration);
        let Some(action_id) = crate::domain::ActionId::new("missing").ok() else {
            return;
        };
        assert!(
            state
                .apply(ProgressEvent::ActionStarted { action_id })
                .is_err()
        );
    }
}
