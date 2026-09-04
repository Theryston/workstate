use std::{
    collections::{BTreeMap, VecDeque},
    fmt::{self, Display, Formatter},
    time::Duration,
};

use crate::{
    application::reconciliation::ApplicationEvent,
    domain::{ActionId, EnvironmentConfig},
    error::{ErrorCategory, Result, WorkstateError},
};

use tokio::{
    runtime::Handle,
    sync::mpsc,
    time::{Duration as TokioDuration, timeout},
};

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressOperation {
    Run,
    Stop,
}

impl ProgressOperation {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Run => "Starting environment",
            Self::Stop => "Stopping environment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionProgressStatus {
    Pending,
    Running,
    Ready,
    Skipped,
    Failed,
    Cancelled,
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
            Self::Cancelled => "cancelled",
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
    ActionCancelled {
        action_id: ActionId,
        reason: String,
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

pub struct ApplicationProgressEventSource {
    receiver: mpsc::Receiver<ApplicationEvent>,
    runtime: Handle,
    poll_interval: TokioDuration,
    pending: VecDeque<ProgressEvent>,
    closed: bool,
}

impl ApplicationProgressEventSource {
    pub fn new(receiver: mpsc::Receiver<ApplicationEvent>, runtime: Handle) -> Self {
        Self {
            receiver,
            runtime,
            poll_interval: TokioDuration::from_millis(100),
            pending: VecDeque::new(),
            closed: false,
        }
    }

    fn next_event(&mut self) -> Option<ApplicationEvent> {
        self.runtime
            .block_on(async { timeout(self.poll_interval, self.receiver.recv()).await })
            .ok()
            .flatten()
    }

    fn enqueue_event(&mut self, event: ApplicationEvent) {
        let events = match event {
            ApplicationEvent::ActionStarted { action_id, .. } => {
                vec![ProgressEvent::ActionStarted { action_id }]
            }
            ApplicationEvent::ActionOutput {
                action_id,
                stream,
                message,
            } => {
                let prefix = match stream {
                    crate::application::planner::ActionOutputStream::Stdout => "",
                    crate::application::planner::ActionOutputStream::Stderr => "stderr: ",
                    crate::application::planner::ActionOutputStream::Log => "",
                };
                vec![ProgressEvent::Log {
                    action_id: Some(action_id),
                    message: format!("{prefix}{message}"),
                }]
            }
            ApplicationEvent::ActionReady { action_id, .. } => {
                vec![ProgressEvent::ActionReady { action_id }]
            }
            ApplicationEvent::ActionSkipped { action_id, reason } => {
                vec![ProgressEvent::ActionSkipped { action_id, reason }]
            }
            ApplicationEvent::ActionFailed { action_id, error } => {
                vec![ProgressEvent::ActionFailed { action_id, error }]
            }
            ApplicationEvent::ActionCancelled { action_id, reason } => {
                vec![ProgressEvent::ActionCancelled { action_id, reason }]
            }
            ApplicationEvent::RollbackStarted => vec![ProgressEvent::RollbackStarted],
            ApplicationEvent::RollbackActionStarted { action_id } => {
                vec![ProgressEvent::RollbackAction { action_id }]
            }
            ApplicationEvent::RollbackActionCompleted {
                action_id,
                success,
                detail,
            } => {
                if success {
                    detail
                        .map(|message| ProgressEvent::Log {
                            action_id: Some(action_id),
                            message,
                        })
                        .into_iter()
                        .collect()
                } else {
                    vec![ProgressEvent::ActionFailed {
                        action_id,
                        error: detail.unwrap_or_else(|| "rollback failed".to_owned()),
                    }]
                }
            }
            ApplicationEvent::RollbackFinished { success } => {
                vec![ProgressEvent::RollbackFinished { success }]
            }
            ApplicationEvent::ResourceCleanupSkipped { resource, reason } => {
                vec![ProgressEvent::Log {
                    action_id: None,
                    message: format!("Preserved {resource}: {reason}"),
                }]
            }
            ApplicationEvent::RunCompleted {
                already_correct, ..
            } => vec![
                ProgressEvent::Log {
                    action_id: None,
                    message: if already_correct {
                        "Every action was already in the desired state.".to_owned()
                    } else {
                        "All actions reached the desired state.".to_owned()
                    },
                },
                ProgressEvent::Completed { success: true },
            ],
            ApplicationEvent::RunFailed {
                action_id, error, ..
            } => vec![ProgressEvent::Log {
                action_id,
                message: format!("Run failed: {error}"),
            }],
            ApplicationEvent::StopCompleted { .. } => {
                vec![ProgressEvent::Completed { success: true }]
            }
            ApplicationEvent::StopFailed { error, .. } => vec![
                ProgressEvent::Log {
                    action_id: None,
                    message: format!("Stop failed: {error}"),
                },
                ProgressEvent::Completed { success: false },
            ],
            ApplicationEvent::RunStarted { .. }
            | ApplicationEvent::PlanBuilt { .. }
            | ApplicationEvent::ActionObserved { .. }
            | ApplicationEvent::StopStarted { .. }
            | ApplicationEvent::DeleteStarted { .. }
            | ApplicationEvent::DeleteCompleted { .. } => Vec::new(),
        };
        self.pending.extend(events);
    }
}

impl crate::ui::app::ProgressEventSource for ApplicationProgressEventSource {
    fn next(&mut self) -> Result<Option<ProgressEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if self.closed {
                return Ok(None);
            }
            let Some(event) = self.next_event() else {
                if self.receiver.is_closed() {
                    self.closed = true;
                    return Ok(Some(ProgressEvent::Completed { success: false }));
                }
                return Ok(Some(ProgressEvent::ClockAdvanced {
                    elapsed: Duration::from_millis(100),
                }));
            };
            self.enqueue_event(event);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressState {
    pub environment_name: String,
    pub operation: ProgressOperation,
    order: Vec<ActionId>,
    entries: BTreeMap<ActionId, ProgressEntry>,
    logs: VecDeque<ProgressLog>,
    pub elapsed: Duration,
    pub spinner_frame: usize,
    pub finished: bool,
    pub successful: Option<bool>,
}

impl ProgressState {
    pub fn from_configuration(configuration: &EnvironmentConfig) -> Self {
        Self::for_operation(configuration, ProgressOperation::Run)
    }

    pub fn for_operation(configuration: &EnvironmentConfig, operation: ProgressOperation) -> Self {
        let order = configuration
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>();
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
            operation,
            order,
            entries,
            logs: VecDeque::new(),
            elapsed: Duration::ZERO,
            spinner_frame: 0,
            finished: false,
            successful: None,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &ProgressEntry> {
        self.order
            .iter()
            .filter_map(|action_id| self.entries.get(action_id))
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
                let operation = self.operation;
                self.update_action(&action_id, |entry| {
                    entry.status = match operation {
                        ProgressOperation::Run => ActionProgressStatus::Ready,
                        ProgressOperation::Stop => ActionProgressStatus::Stopped,
                    };
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
            ProgressEvent::ActionCancelled { action_id, reason } => {
                self.update_action(&action_id, |entry| {
                    entry.status = ActionProgressStatus::Cancelled;
                    entry.detail = Some(reason);
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
                } else {
                    for entry in self.entries.values_mut() {
                        if entry.status == ActionProgressStatus::RollingBack {
                            entry.status = ActionProgressStatus::Failed;
                            entry.detail = Some("rollback failed".to_owned());
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
                if success && self.operation == ProgressOperation::Stop {
                    for entry in self.entries.values_mut() {
                        if entry.status == ActionProgressStatus::Pending {
                            entry.status = ActionProgressStatus::Stopped;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn advance(&mut self, duration: Duration) {
        self.elapsed = self.elapsed.saturating_add(duration);
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        for entry in self.entries.values_mut() {
            if entry.status == ActionProgressStatus::Running {
                entry.elapsed = entry.elapsed.saturating_add(duration);
            }
        }
    }

    pub fn ready_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| {
                matches!(
                    entry.status,
                    ActionProgressStatus::Ready | ActionProgressStatus::Stopped
                )
            })
            .count()
    }

    pub fn running_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.status == ActionProgressStatus::Running)
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.status == ActionProgressStatus::Pending)
            .count()
    }

    pub fn spinner(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()]
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

    use crate::{
        application::reconciliation::ApplicationEvent,
        domain::{ActionId, ActionKind, ActionSpec, EnvironmentConfig},
    };

    use super::{
        ActionProgressStatus, ApplicationProgressEventSource, ProgressEvent, ProgressOperation,
        ProgressState,
    };

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

    #[test]
    fn stop_progress_marks_completed_actions_as_stopped() {
        let Some(configuration) = configuration() else {
            return;
        };
        let Some(action) = configuration.actions.first() else {
            return;
        };
        let mut state = ProgressState::for_operation(&configuration, ProgressOperation::Stop);
        assert!(
            state
                .apply(ProgressEvent::ActionStarted {
                    action_id: action.id.clone(),
                })
                .is_ok()
        );
        assert!(
            state
                .apply(ProgressEvent::ActionReady {
                    action_id: action.id.clone(),
                })
                .is_ok()
        );
        assert_eq!(
            state.entry(&action.id).map(|entry| entry.status),
            Some(ActionProgressStatus::Stopped)
        );
        assert!(
            state
                .apply(ProgressEvent::Completed { success: true })
                .is_ok()
        );
        assert_eq!(state.successful, Some(true));
    }

    #[test]
    fn progress_source_maps_application_action_events() {
        let Some(action_id) = ActionId::new("api").ok() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        else {
            return;
        };
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(sender);
        let mut source = ApplicationProgressEventSource::new(receiver, runtime.handle().clone());
        source.enqueue_event(ApplicationEvent::ActionStarted {
            action_id: action_id.clone(),
            attempt: 1,
            execution_mode: None,
        });
        assert_eq!(
            source.pending.pop_front(),
            Some(ProgressEvent::ActionStarted { action_id })
        );
    }
}
