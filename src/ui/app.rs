use std::{io, path::Path};

use crossterm::event::KeyCode;
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    application::ports::{DirectoryCatalog, FileCatalog},
    domain::{EnvironmentConfig, EnvironmentName, EnvironmentSlug},
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{
    editor::{EditorAction, EditorState},
    event::{CrosstermEventSource, EventSource, UiEvent, run_with_terminal},
    progress::{ProgressEvent, ProgressOperation, ProgressState},
    state::{SelectorAction, SelectorState},
    theme::Theme,
    widgets,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOutcome {
    Saved(EnvironmentConfig),
    Cancelled,
}

pub fn select_environment(state: SelectorState, no_color: bool) -> Result<Option<EnvironmentSlug>> {
    let mut session = super::event::CrosstermTerminalSession;
    run_with_terminal(&mut session, || {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)
            .map_err(|source| ui_error("could not initialize the terminal UI", source))?;
        let mut source = CrosstermEventSource::default();
        run_selector_loop(&mut terminal, &mut source, state, no_color)
    })
}

pub fn edit_environment(state: EditorState, no_color: bool) -> Result<EditorOutcome> {
    edit_environment_with_directory_catalog(state, None, no_color)
}

pub fn edit_environment_with_directory_catalog(
    state: EditorState,
    directory_catalog: Option<&dyn DirectoryCatalog>,
    no_color: bool,
) -> Result<EditorOutcome> {
    edit_environment_with_catalogs(state, directory_catalog, None, no_color)
}

pub fn edit_environment_with_catalogs(
    state: EditorState,
    directory_catalog: Option<&dyn DirectoryCatalog>,
    file_catalog: Option<&dyn FileCatalog>,
    no_color: bool,
) -> Result<EditorOutcome> {
    let mut session = super::event::CrosstermTerminalSession;
    run_with_terminal(&mut session, || {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)
            .map_err(|source| ui_error("could not initialize the terminal UI", source))?;
        let mut source = CrosstermEventSource::default();
        run_editor_loop_with_catalogs(
            &mut terminal,
            &mut source,
            state,
            directory_catalog,
            file_catalog,
            no_color,
        )
    })
}

pub fn confirm_delete(
    name: &EnvironmentName,
    directory: &Path,
    active: bool,
    no_color: bool,
) -> Result<bool> {
    let mut session = super::event::CrosstermTerminalSession;
    run_with_terminal(&mut session, || {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)
            .map_err(|source| ui_error("could not initialize the terminal UI", source))?;
        let mut source = CrosstermEventSource::default();
        run_delete_loop(
            &mut terminal,
            &mut source,
            name,
            directory,
            active,
            no_color,
        )
    })
}

pub trait ProgressEventSource {
    fn next(&mut self) -> Result<Option<ProgressEvent>>;
}

pub fn show_progress<S>(
    configuration: &EnvironmentConfig,
    source: &mut S,
    no_color: bool,
) -> Result<ProgressState>
where
    S: ProgressEventSource,
{
    show_lifecycle_progress(configuration, source, ProgressOperation::Run, no_color)
}

pub fn show_lifecycle_progress<S>(
    configuration: &EnvironmentConfig,
    source: &mut S,
    operation: ProgressOperation,
    no_color: bool,
) -> Result<ProgressState>
where
    S: ProgressEventSource,
{
    let mut session = super::event::CrosstermTerminalSession;
    run_with_terminal(&mut session, || {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)
            .map_err(|source| ui_error("could not initialize the terminal UI", source))?;
        run_progress_loop(
            &mut terminal,
            source,
            ProgressState::for_operation(configuration, operation),
            no_color,
        )
    })
}

fn run_selector_loop<B, S>(
    terminal: &mut Terminal<B>,
    source: &mut S,
    mut state: SelectorState,
    no_color: bool,
) -> Result<Option<EnvironmentSlug>>
where
    B: ratatui::backend::Backend,
    S: EventSource,
{
    let theme = Theme::new(!no_color);
    loop {
        terminal
            .draw(|frame| widgets::render_selector(frame, &state, theme))
            .map_err(|source| ui_error("could not draw the environment selector", source))?;

        match source.next()? {
            UiEvent::Key(key) => match state.handle_key(key.code) {
                SelectorAction::Selected => return Ok(state.selected_slug()),
                SelectorAction::Cancel => return Ok(None),
                SelectorAction::None => {}
            },
            UiEvent::Resize { .. } | UiEvent::Tick => {}
        }
    }
}

fn run_editor_loop_with_catalogs<B, S>(
    terminal: &mut Terminal<B>,
    source: &mut S,
    mut state: EditorState,
    directory_catalog: Option<&dyn DirectoryCatalog>,
    file_catalog: Option<&dyn FileCatalog>,
    no_color: bool,
) -> Result<EditorOutcome>
where
    B: ratatui::backend::Backend,
    S: EventSource,
{
    let theme = Theme::new(!no_color);
    let mut save_confirmation = false;
    loop {
        terminal
            .draw(|frame| {
                widgets::render_editor(frame, &state, theme);
                if save_confirmation {
                    widgets::render_confirmation(
                        frame,
                        "Save environment?",
                        &format!("Save {}?", state.configuration.name),
                        "y confirm · n or Esc cancel",
                        theme,
                    );
                }
            })
            .map_err(|source| ui_error("could not draw the environment editor", source))?;

        match source.next()? {
            UiEvent::Key(key) if save_confirmation => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    state.validate()?;
                    return Ok(EditorOutcome::Saved(state.configuration));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    save_confirmation = false;
                }
                _ => {}
            },
            UiEvent::Key(key) => {
                match state.handle_key_event_with_catalogs(key, directory_catalog, file_catalog) {
                    EditorAction::SaveRequested => {
                        if state.validate().is_ok() {
                            save_confirmation = true;
                        }
                    }
                    EditorAction::CancelRequested => return Ok(EditorOutcome::Cancelled),
                    EditorAction::None
                    | EditorAction::PaletteOpened
                    | EditorAction::ReviewOpened => {}
                }
            }
            UiEvent::Resize { .. } | UiEvent::Tick => {}
        }
    }
}

fn run_delete_loop<B, S>(
    terminal: &mut Terminal<B>,
    source: &mut S,
    name: &EnvironmentName,
    directory: &Path,
    active: bool,
    no_color: bool,
) -> Result<bool>
where
    B: ratatui::backend::Backend,
    S: EventSource,
{
    let theme = Theme::new(!no_color);
    loop {
        terminal
            .draw(|frame| {
                widgets::render_delete_confirmation(frame, name, directory, active, theme)
            })
            .map_err(|source| ui_error("could not draw the delete confirmation", source))?;

        match source.next()? {
            UiEvent::Key(key) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => return Ok(false),
                _ => {}
            },
            UiEvent::Resize { .. } | UiEvent::Tick => {}
        }
    }
}

fn run_progress_loop<B, S>(
    terminal: &mut Terminal<B>,
    source: &mut S,
    mut state: ProgressState,
    no_color: bool,
) -> Result<ProgressState>
where
    B: ratatui::backend::Backend,
    S: ProgressEventSource,
{
    let theme = Theme::new(!no_color);
    loop {
        terminal
            .draw(|frame| widgets::render_progress(frame, &state, theme))
            .map_err(|source| ui_error("could not draw lifecycle progress", source))?;
        if state.finished {
            return Ok(state);
        }
        let Some(event) = source.next()? else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "lifecycle progress ended before a completion event",
            ));
        };
        state.apply(event)?;
    }
}

fn ui_error(message: &str, source: io::Error) -> WorkstateError {
    WorkstateError::with_source(ErrorCategory::Ui, message, source)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        domain::{EnvironmentConfig, EnvironmentName, EnvironmentSlug},
        error::{ErrorCategory, Result, WorkstateError},
    };

    use super::{
        EditorOutcome, ProgressEventSource, run_delete_loop, run_editor_loop_with_catalogs,
        run_progress_loop, run_selector_loop,
    };
    use crate::ui::{
        editor::{EditorMode, EditorState},
        event::{EventSource, UiEvent},
        progress::ProgressEvent,
        state::{EnvironmentListItem, EnvironmentStatus, SelectorState},
    };

    struct FakeEvents {
        events: VecDeque<UiEvent>,
    }

    impl FakeEvents {
        fn keys(keys: impl IntoIterator<Item = char>) -> Self {
            Self {
                events: keys
                    .into_iter()
                    .map(|key| UiEvent::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)))
                    .collect(),
            }
        }

        fn one(key: KeyCode) -> Self {
            Self {
                events: VecDeque::from([UiEvent::Key(KeyEvent::new(key, KeyModifiers::NONE))]),
            }
        }
    }

    impl EventSource for FakeEvents {
        fn next(&mut self) -> Result<UiEvent> {
            self.events.pop_front().ok_or_else(|| {
                WorkstateError::new(ErrorCategory::Ui, "fake event source was exhausted")
            })
        }
    }

    struct FakeProgressEvents {
        events: VecDeque<ProgressEvent>,
    }

    impl ProgressEventSource for FakeProgressEvents {
        fn next(&mut self) -> Result<Option<ProgressEvent>> {
            Ok(self.events.pop_front())
        }
    }

    #[test]
    fn selector_returns_the_highlighted_environment() {
        let Some(name) = EnvironmentName::new("Personal Blog").ok() else {
            return;
        };
        let Some(slug) = EnvironmentSlug::new("personal-blog").ok() else {
            return;
        };
        let state = SelectorState::new(vec![EnvironmentListItem::new(
            name,
            slug.clone(),
            EnvironmentStatus::Unknown,
        )]);
        let backend = TestBackend::new(80, 12);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let mut events = FakeEvents::one(KeyCode::Enter);
        let result = run_selector_loop(&mut terminal, &mut events, state, true);
        assert_eq!(result.ok(), Some(Some(slug)));
    }

    #[test]
    fn editor_requires_explicit_save_confirmation_and_returns_the_configuration() {
        let Some(configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let state = EditorState::new(configuration.clone(), EditorMode::Create);
        let backend = TestBackend::new(100, 24);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let mut events = FakeEvents::keys(['s', 'y']);
        let result =
            run_editor_loop_with_catalogs(&mut terminal, &mut events, state, None, None, true);
        assert_eq!(result.ok(), Some(EditorOutcome::Saved(configuration)));
    }

    #[test]
    fn delete_confirmation_accepts_only_the_explicit_confirmation_key() {
        let Some(name) = EnvironmentName::new("Personal Blog").ok() else {
            return;
        };
        let backend = TestBackend::new(100, 20);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let mut events = FakeEvents::one(KeyCode::Char('y'));
        let result = run_delete_loop(
            &mut terminal,
            &mut events,
            &name,
            std::path::Path::new("/home/example/.workstate/personal-blog"),
            true,
            true,
        );
        assert_eq!(result.ok(), Some(true));
    }

    #[test]
    fn progress_loop_closes_after_a_completion_event() {
        let Some(configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let backend = TestBackend::new(100, 20);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let mut events = FakeProgressEvents {
            events: VecDeque::from([ProgressEvent::Completed { success: true }]),
        };
        let result = run_progress_loop(
            &mut terminal,
            &mut events,
            crate::ui::progress::ProgressState::from_configuration(&configuration),
            true,
        );
        assert!(result.is_ok());
        let Some(state) = result.ok() else {
            return;
        };
        assert_eq!(state.successful, Some(true));
    }
}
