use std::{future::Future, io, time::Duration};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEvent},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::error::{ErrorCategory, Result, WorkstateError};

pub trait TerminalSession {
    fn enter(&mut self) -> Result<()>;
    fn restore(&mut self) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct CrosstermTerminalSession;

impl TerminalSession for CrosstermTerminalSession {
    fn enter(&mut self) -> Result<()> {
        terminal::enable_raw_mode()
            .map_err(|source| terminal_error("could not enable terminal raw mode", source))?;

        if let Err(source) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let restore_result = self.restore();
            if let Err(error) = restore_result {
                tracing::error!(error = %error, "terminal restoration failed after terminal setup error");
            }
            return Err(terminal_error("could not enter the terminal UI", source));
        }

        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        let screen_result = execute!(io::stdout(), LeaveAlternateScreen, Show)
            .map_err(|source| terminal_error("could not restore the terminal screen", source));
        let raw_mode_result = terminal::disable_raw_mode()
            .map_err(|source| terminal_error("could not disable terminal raw mode", source));

        match (screen_result, raw_mode_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(screen_error), Ok(())) => Err(screen_error),
            (Ok(()), Err(raw_mode_error)) => Err(raw_mode_error),
            (Err(screen_error), Err(raw_mode_error)) => {
                Err(screen_error.with_context("raw_mode_restore_error", raw_mode_error.to_string()))
            }
        }
    }
}

pub struct TerminalGuard<'a> {
    session: &'a mut dyn TerminalSession,
    active: bool,
}

impl<'a> TerminalGuard<'a> {
    pub fn enter(session: &'a mut dyn TerminalSession) -> Result<Self> {
        session.enter()?;
        Ok(Self {
            session,
            active: true,
        })
    }

    pub fn finish(mut self) -> Result<()> {
        let result = self.session.restore();
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Drop for TerminalGuard<'_> {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.session.restore()
        {
            tracing::error!(error = %error, "terminal restoration failed during guard cleanup");
        }
    }
}

pub fn run_with_terminal<F, T>(session: &mut dyn TerminalSession, operation: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let guard = TerminalGuard::enter(session)?;
    let operation_result = operation();
    finish_operation(guard, operation_result)
}

pub async fn run_with_terminal_async<F, Fut, T>(
    session: &mut dyn TerminalSession,
    operation: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let guard = TerminalGuard::enter(session)?;
    let operation_result = operation().await;
    finish_operation(guard, operation_result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    Key(KeyEvent),
    Resize { width: u16, height: u16 },
    Tick,
}

pub trait EventSource {
    fn next(&mut self) -> Result<UiEvent>;
}

#[derive(Debug, Clone, Copy)]
pub struct CrosstermEventSource {
    poll_interval: Duration,
}

impl Default for CrosstermEventSource {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(100),
        }
    }
}

impl CrosstermEventSource {
    pub fn new(poll_interval: Duration) -> Self {
        Self { poll_interval }
    }
}

impl EventSource for CrosstermEventSource {
    fn next(&mut self) -> Result<UiEvent> {
        if !event::poll(self.poll_interval)
            .map_err(|source| terminal_error("could not poll terminal input", source))?
        {
            return Ok(UiEvent::Tick);
        }

        let event = event::read()
            .map_err(|source| terminal_error("could not read terminal input", source))?;
        let event = match event {
            Event::Key(key) => UiEvent::Key(key),
            Event::Resize(width, height) => UiEvent::Resize { width, height },
            _ => UiEvent::Tick,
        };
        Ok(event)
    }
}

fn finish_operation<T>(guard: TerminalGuard<'_>, operation_result: Result<T>) -> Result<T> {
    let restore_result = guard.finish();
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => {
            Err(error.with_context("terminal_restore_error", restore_error.to_string()))
        }
    }
}

fn terminal_error(message: &str, source: io::Error) -> WorkstateError {
    WorkstateError::with_source(ErrorCategory::Ui, message, source)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::error::{ErrorCategory, Result, WorkstateError};

    use super::{TerminalSession, UiEvent, run_with_terminal, run_with_terminal_async};

    #[derive(Clone, Default)]
    struct RecordingSession {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TerminalSession for RecordingSession {
        fn enter(&mut self) -> Result<()> {
            let mut calls = self.calls.lock().map_err(|_| {
                WorkstateError::new(ErrorCategory::Ui, "recording session lock failed")
            })?;
            calls.push("enter");
            Ok(())
        }

        fn restore(&mut self) -> Result<()> {
            let mut calls = self.calls.lock().map_err(|_| {
                WorkstateError::new(ErrorCategory::Ui, "recording session lock failed")
            })?;
            calls.push("restore");
            Ok(())
        }
    }

    #[test]
    fn terminal_is_restored_after_success_and_error() {
        let mut session = RecordingSession::default();
        assert!(run_with_terminal(&mut session, || Ok(())).is_ok());
        assert!(
            run_with_terminal(&mut session, || {
                Err::<(), _>(WorkstateError::new(
                    ErrorCategory::Runtime,
                    "operation failed",
                ))
            })
            .is_err()
        );

        let calls = session.calls.lock();
        assert!(calls.is_ok());
        let Some(calls) = calls.ok() else {
            return;
        };
        assert_eq!(*calls, vec!["enter", "restore", "enter", "restore"]);
    }

    #[tokio::test]
    async fn terminal_is_restored_when_an_async_operation_fails() {
        let mut session = RecordingSession::default();
        let result = run_with_terminal_async(&mut session, || async {
            Err::<(), _>(WorkstateError::new(ErrorCategory::Runtime, "task failed"))
        })
        .await;
        assert!(result.is_err());

        let calls = session.calls.lock();
        assert!(calls.is_ok());
        let Some(calls) = calls.ok() else {
            return;
        };
        assert_eq!(*calls, vec!["enter", "restore"]);
    }

    #[test]
    fn ui_events_remain_typed() {
        let event = UiEvent::Tick;
        assert_eq!(event, UiEvent::Tick);
    }
}
