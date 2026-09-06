use std::sync::Arc;

use super::errors::CosmicError;

pub(crate) mod capabilities;
pub(crate) mod connection;
pub(crate) mod operations;
pub(crate) mod snapshot;
pub(crate) mod state;

#[derive(Clone, Default)]
pub struct CosmicWaylandCoordinator {
    operation_lock: Arc<tokio::sync::Mutex<()>>,
}

impl CosmicWaylandCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run_blocking<T, F>(
        &self,
        operation: &'static str,
        operation_fn: F,
    ) -> Result<T, CosmicError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, CosmicError> + Send + 'static,
    {
        let _operation_guard = self.operation_lock.lock().await;
        tokio::task::spawn_blocking(operation_fn)
            .await
            .map_err(|source| CosmicError::BlockingTaskFailed {
                operation: operation.to_owned(),
                detail: source.to_string(),
            })?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tokio::sync::Notify;

    use super::CosmicWaylandCoordinator;

    #[tokio::test]
    async fn serializes_blocking_operations() {
        let coordinator = Arc::new(CosmicWaylandCoordinator::new());
        let started = Arc::new(Notify::new());
        let (release_sender, release_receiver) = std::sync::mpsc::channel();

        let first_coordinator = Arc::clone(&coordinator);
        let started_for_task = Arc::clone(&started);
        let first = tokio::spawn(async move {
            first_coordinator
                .run_blocking("first", move || {
                    started_for_task.notify_one();
                    release_receiver.recv().map_err(|error| {
                        super::CosmicError::BlockingTaskFailed {
                            operation: "first".to_owned(),
                            detail: error.to_string(),
                        }
                    })?;
                    Ok(())
                })
                .await
        });

        let first_started =
            tokio::time::timeout(std::time::Duration::from_secs(1), started.notified()).await;
        assert!(first_started.is_ok());

        let second_started = Arc::new(AtomicBool::new(false));
        let second_coordinator = Arc::clone(&coordinator);
        let second_started_for_task = Arc::clone(&second_started);
        let second = tokio::spawn(async move {
            second_coordinator
                .run_blocking("second", move || {
                    second_started_for_task.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let second_was_blocked = !second_started.load(Ordering::SeqCst);
        let release_sent = release_sender.send(());
        assert!(release_sent.is_ok());

        let first_result = first.await;
        assert!(first_result.is_ok());
        let second_result = second.await;
        assert!(second_result.is_ok());
        assert!(second_was_blocked);
    }
}
