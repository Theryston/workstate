use std::sync::Arc;

use crate::{
    application::{
        planner::{ActionHandlerRegistry, CancellationToken, ExecutionPlan, run_with_timeout},
        reconciliation::{ApplicationEvent, EventSink},
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::engine::RuntimeJournal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackFailure {
    pub action_id: Option<crate::domain::ActionId>,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackReport {
    pub attempted_actions: usize,
    pub completed_actions: usize,
    pub failures: Vec<RollbackFailure>,
}

impl RollbackReport {
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.failures.is_empty() {
            return format!(
                "rollback completed for {} action(s)",
                self.completed_actions
            );
        }
        self.failures
            .iter()
            .map(|failure| {
                failure
                    .action_id
                    .as_ref()
                    .map(|action_id| format!("{action_id}: {}", failure.message))
                    .unwrap_or_else(|| failure.message.clone())
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

pub struct RollbackEngine {
    handlers: Arc<ActionHandlerRegistry>,
    timeout: std::time::Duration,
}

impl RollbackEngine {
    pub fn new(handlers: Arc<ActionHandlerRegistry>, timeout: std::time::Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "rollback timeout must be greater than zero",
            ));
        }
        Ok(Self { handlers, timeout })
    }

    pub async fn execute(
        &self,
        plan: &ExecutionPlan,
        journal: &RuntimeJournal,
        events: Arc<dyn EventSink>,
    ) -> Result<RollbackReport> {
        events.emit(ApplicationEvent::RollbackStarted).await?;
        journal.begin_rollback()?;
        let cancellation = CancellationToken::new();
        let results = journal.completed_results()?;
        let mut report = RollbackReport::default();

        for action_id in plan.ordered_action_ids().iter().rev() {
            let Some(result) = results.get(action_id) else {
                continue;
            };
            let compensating_result = journal.compensating_result(result)?;
            if compensating_result.resources.is_empty() && compensating_result.mutations.is_empty()
            {
                continue;
            }
            report.attempted_actions += 1;
            events
                .emit(ApplicationEvent::RollbackActionStarted {
                    action_id: action_id.clone(),
                })
                .await?;

            let Some(entry) = plan.entry(action_id) else {
                let failure = RollbackFailure {
                    action_id: Some(action_id.clone()),
                    message: "the action was missing from the execution plan".to_owned(),
                };
                journal.record_compensation_failure(&failure)?;
                report.failures.push(failure);
                continue;
            };
            let Some(handler) = self.handlers.handler_for(&entry.action.kind) else {
                let failure = RollbackFailure {
                    action_id: Some(action_id.clone()),
                    message: format!("no handler is registered for action '{action_id}'"),
                };
                journal.record_compensation_failure(&failure)?;
                report.failures.push(failure);
                continue;
            };

            let result = run_with_timeout(
                handler.compensate(&entry.action, &compensating_result, cancellation.clone()),
                entry.timeout.unwrap_or(self.timeout),
                cancellation.clone(),
                Some(action_id),
                "rollback",
            )
            .await;
            match result {
                Ok(compensation) => {
                    for output in compensation.outputs {
                        events
                            .emit(ApplicationEvent::ActionOutput {
                                action_id: action_id.clone(),
                                stream: output.stream,
                                message: output.message,
                            })
                            .await?;
                    }
                    journal.mark_compensated(action_id, &compensating_result)?;
                    report.completed_actions += 1;
                    events
                        .emit(ApplicationEvent::RollbackActionCompleted {
                            action_id: action_id.clone(),
                            success: true,
                            detail: None,
                        })
                        .await?;
                }
                Err(error) => {
                    let failure = RollbackFailure {
                        action_id: Some(action_id.clone()),
                        message: error.to_string(),
                    };
                    journal.record_compensation_failure(&failure)?;
                    report.failures.push(failure.clone());
                    events
                        .emit(ApplicationEvent::RollbackActionCompleted {
                            action_id: action_id.clone(),
                            success: false,
                            detail: Some(failure.message.clone()),
                        })
                        .await?;
                }
            }
        }

        journal.finish_rollback(&report)?;
        events
            .emit(ApplicationEvent::RollbackFinished {
                success: report.succeeded(),
            })
            .await?;
        Ok(report)
    }
}
