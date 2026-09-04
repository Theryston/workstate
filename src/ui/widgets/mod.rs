use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::domain::{
    ActionKind, ActionSpec, EnvironmentName, ExecutionMode, TilingPreference, WorkspaceReference,
    WorkspaceSpec, WorkspaceTarget,
};

use super::{
    editor::{EditorPanel, EditorState, action_palette},
    progress::{ActionProgressStatus, ProgressState},
    state::{EnvironmentStatus, SELECTOR_EMPTY_MESSAGE, SelectorState},
    theme::Theme,
};

pub fn render_selector(frame: &mut Frame<'_>, state: &SelectorState, theme: Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(" Environments ", theme.title_style()));

    if state.empty() {
        let message = Paragraph::new(SELECTOR_EMPTY_MESSAGE)
            .style(theme.muted_style())
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(message, frame.area());
        return;
    }

    let items = state
        .items()
        .iter()
        .map(|item| {
            let status_style = environment_status_style(item.status, theme);
            ListItem::new(Line::from(vec![
                Span::styled(item.name.to_string(), theme.text_style()),
                Span::styled(format!("  {}  ", item.slug), theme.muted_style()),
                Span::styled(item.status.to_string(), status_style),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_index());
    frame.render_stateful_widget(list, frame.area(), &mut list_state);
}

pub fn render_editor(frame: &mut Frame<'_>, state: &EditorState, theme: Theme) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled(" Workstate ", theme.title_style()),
        Span::styled(
            format!(
                "{} · {}",
                state.configuration.name, state.configuration.slug
            ),
            theme.text_style(),
        ),
    ]))
    .block(panel_block("Environment", theme))
    .wrap(Wrap { trim: true });
    frame.render_widget(header, sections[0]);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(sections[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(columns[0]);
    render_action_list(frame, state, theme, left[0]);
    render_workspace_list(frame, state, theme, left[1]);

    match state.panel {
        EditorPanel::Review => render_review(frame, state, theme, columns[1]),
        EditorPanel::Actions | EditorPanel::Workspaces | EditorPanel::Inspector => {
            render_inspector(frame, state, theme, columns[1])
        }
    }

    let footer = Paragraph::new(
        "↑↓ move  Tab panel  a add  e label/name  w directory  o app  p project  c command  v tool  r check  m mode  g workspace  x target  +/- dependencies  s save",
    )
    .style(theme.muted_style())
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(theme.border_style()),
    );
    frame.render_widget(footer, sections[2]);

    if state.palette_open {
        render_palette(frame, state, theme);
    }
    if let Some(input) = &state.input {
        render_input(frame, input.field, &input.value, theme);
    }
    if state.delete_confirmation {
        let label = state.selected_action_spec().map(action_label);
        render_confirmation(
            frame,
            "Delete selected action?",
            label.as_deref().unwrap_or("the selected action"),
            "y confirm · n or Esc cancel",
            theme,
        );
    }
}

pub fn render_progress(frame: &mut Frame<'_>, state: &ProgressState, theme: Theme) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(frame.area());
    let summary = format!(
        " {}  ·  {} / {} ready  ·  {} ms",
        state.environment_name,
        state.ready_count(),
        state.total_count(),
        state.elapsed.as_millis()
    );
    frame.render_widget(
        Paragraph::new(summary).block(panel_block("Starting environment", theme)),
        sections[0],
    );

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(sections[1]);
    let entries = state
        .entries()
        .map(|entry| {
            let detail = entry
                .detail
                .as_deref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            let timing = entry
                .timeout
                .map(|timeout| {
                    format!("  {}/{} ms", entry.elapsed.as_millis(), timeout.as_millis())
                })
                .unwrap_or_else(|| format!("  {} ms", entry.elapsed.as_millis()));
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{}  ", entry.status),
                    progress_status_style(entry.status, theme),
                ),
                Span::styled(entry.label.clone(), theme.text_style()),
                Span::styled(timing, theme.muted_style()),
                Span::styled(detail, theme.muted_style()),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(entries).block(panel_block("Actions", theme)),
        columns[0],
    );

    let logs = state
        .logs()
        .map(|log| {
            let prefix = log
                .action_id
                .as_ref()
                .map(|id| format!("[{id}] "))
                .unwrap_or_default();
            ListItem::new(format!("{prefix}{}", log.message))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(logs)
            .style(theme.muted_style())
            .block(panel_block("Logs", theme)),
        columns[1],
    );

    let footer = match state.successful {
        Some(true) => "Environment ready · press q to close",
        Some(false) => "Setup failed and rollback finished · press q to close",
        None => "Starting · progress is streamed from the application layer",
    };
    frame.render_widget(
        Paragraph::new(footer).style(theme.muted_style()),
        sections[2],
    );
}

pub fn render_confirmation(
    frame: &mut Frame<'_>,
    title: &str,
    subject: &str,
    instruction: &str,
    theme: Theme,
) {
    let area = centered_rect(64, 34, frame.area());
    frame.render_widget(Clear, area);
    let content = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(subject, theme.text_style())),
        Line::from(""),
        Line::from(Span::styled(instruction, theme.muted_style())),
    ]))
    .block(panel_block(title, theme))
    .wrap(Wrap { trim: true });
    frame.render_widget(content, area);
}

pub fn render_delete_confirmation(
    frame: &mut Frame<'_>,
    name: &EnvironmentName,
    directory: &std::path::Path,
    active: bool,
    theme: Theme,
) {
    let area = centered_rect(76, 50, frame.area());
    frame.render_widget(Clear, area);
    let active_text = if active { "yes" } else { "no" };
    let content = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            format!("Environment: {name}"),
            theme.text_style(),
        )),
        Line::from(Span::styled(
            format!("Directory:   {}", directory.display()),
            theme.muted_style(),
        )),
        Line::from(Span::styled(
            format!("Active:      {active_text}"),
            if active {
                theme.warning_style()
            } else {
                theme.muted_style()
            },
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Active resources will be stopped first.",
            theme.warning_style(),
        )),
        Line::from(Span::styled(
            "The environment directory will then be removed.",
            theme.error_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y confirm · n or Esc cancel",
            theme.muted_style(),
        )),
    ]))
    .block(panel_block("Delete environment", theme))
    .wrap(Wrap { trim: true });
    frame.render_widget(content, area);
}

fn render_action_list(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let items = state
        .configuration
        .actions
        .iter()
        .map(|action| {
            ListItem::new(Line::from(vec![
                Span::styled(action.id.to_string(), theme.title_style()),
                Span::styled(format!("  {}", action_label(action)), theme.text_style()),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block("Actions · a to add", theme))
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_action);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_workspace_list(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let items = state
        .configuration
        .workspaces
        .iter()
        .map(|workspace| {
            ListItem::new(Line::from(vec![
                Span::styled(workspace.id.to_string(), theme.title_style()),
                Span::styled(
                    format!("  {}", workspace_target_label(workspace)),
                    theme.text_style(),
                ),
                Span::styled(
                    format!("  tiling {}", tiling_label(workspace.tiling)),
                    theme.muted_style(),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items).block(panel_block("Workspaces", theme));
    frame.render_widget(list, area);
}

fn render_inspector(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let mut lines = Vec::new();
    if let Some(action) = state.selected_action_spec() {
        lines.push(Line::from(vec![
            Span::styled("Action ID  ", theme.muted_style()),
            Span::styled(action.id.to_string(), theme.title_style()),
        ]));
        lines.push(Line::from(format!("Kind       {}", action.kind.key())));
        lines.push(Line::from(format!(
            "Label      {}",
            action.display_label.as_deref().unwrap_or("not set")
        )));
        lines.push(Line::from(format!(
            "Directory  {}",
            action.working_directory.as_deref().unwrap_or("not set")
        )));
        let workspace = action
            .desktop_workspace
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "current".to_owned());
        lines.push(Line::from(format!("Workspace  {workspace}")));
        let mode = action
            .execution_mode
            .as_ref()
            .map(|value| execution_mode_label(*value).to_owned())
            .unwrap_or_else(|| "not set".to_owned());
        lines.push(Line::from(format!("Mode       {mode}")));
        let dependencies = if action.depends_on.is_empty() {
            "none".to_owned()
        } else {
            action
                .depends_on
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(Line::from(format!("Depends on {dependencies}")));
        for path in state.dependency_path(&action.id) {
            lines.push(Line::from(Span::styled(
                format!("Path       {path}"),
                theme.muted_style(),
            )));
        }
        lines.push(Line::from(format!(
            "Readiness  {} check(s)",
            action.readiness_checks.len()
        )));
        let timeout = action
            .timeout
            .as_ref()
            .map(|value| format!("{} ms", value.milliseconds))
            .unwrap_or_else(|| "default".to_owned());
        lines.push(Line::from(format!("Timeout    {timeout}")));
        lines.push(Line::from(format!(
            "Retry      {} attempt(s), {} ms delay",
            action.retry_policy.max_attempts, action.retry_policy.delay_milliseconds
        )));
        lines.push(Line::from(format!(
            "Cleanup    {}",
            format!("{:?}", action.cleanup_policy).to_ascii_lowercase()
        )));
        if let Some(application) = &action.parameters.application {
            lines.push(Line::from(format!("Application {application}")));
        }
        if let Some(project) = &action.parameters.project_path {
            lines.push(Line::from(format!("Project     {project}")));
        }
        if let Some(command) = &action.parameters.command {
            lines.push(Line::from(format!("Command     {}", command.program)));
        }
        if let Some(container) = &action.parameters.container {
            lines.push(Line::from(format!("Container   {}", container.name)));
            if let Some(image) = &container.image {
                lines.push(Line::from(format!("Image       {image}")));
            }
        }
        if let Some(compose) = &action.parameters.compose {
            lines.push(Line::from(format!(
                "Compose     {}",
                compose.project_name.as_deref().unwrap_or("default project")
            )));
        }
        if let Some(emulator) = &action.parameters.emulator {
            lines.push(Line::from(format!("AVD         {}", emulator.avd)));
        }
        if let Some(workspace_id) = &action.parameters.workspace_id {
            lines.push(Line::from(format!("Workspace ID {workspace_id}")));
        }
        let hint = match &action.kind {
            ActionKind::OpenApplication => "Edit: o application · w directory · g workspace",
            ActionKind::OpenProject => {
                "Edit: o application · p project · w directory · g workspace"
            }
            ActionKind::RunCommand | ActionKind::StartService => {
                "Edit: c command · w directory · m mode · g workspace"
            }
            ActionKind::CreateOrSelectWorkspace => "Edit: x workspace target",
            ActionKind::ConfigureTiling => "Edit: g workspace",
            ActionKind::StartContainer => "Edit: v container",
            ActionKind::StartCompose => "Edit: v Compose project",
            ActionKind::StartAndroidEmulator => "Edit: v Android virtual device",
            ActionKind::WaitForCondition | ActionKind::VerifyResource => "Edit: r readiness delay",
            ActionKind::Custom { .. } => "Edit common properties and dependencies",
        };
        lines.push(Line::from(Span::styled(hint, theme.muted_style())));
    } else {
        lines.push(Line::from(Span::styled(
            "No action selected. Press a to add one.",
            theme.muted_style(),
        )));
    }

    if let Some(notice) = &state.notice {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(notice, theme.success_style())));
    }
    if !state.validation_errors.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Validation", theme.error_style())));
        lines.extend(
            state
                .validation_errors
                .iter()
                .map(|error| Line::from(Span::styled(format!("· {error}"), theme.error_style()))),
        );
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(panel_block("Inspector", theme))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_review(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled("Review before saving", theme.title_style())),
        Line::from(format!("Name       {}", state.configuration.name)),
        Line::from(format!("Slug       {}", state.configuration.slug)),
        Line::from(format!(
            "Workspaces {}",
            state.configuration.workspaces.len()
        )),
        Line::from(format!("Actions    {}", state.configuration.actions.len())),
    ];
    for action in &state.configuration.actions {
        let dependencies = if action.depends_on.is_empty() {
            "no dependencies".to_owned()
        } else {
            action
                .depends_on
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(Line::from(format!(
            "· {}  →  {}",
            action_label(action),
            dependencies
        )));
    }
    if !state.validation_errors.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Resolve validation errors before saving.",
            theme.error_style(),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press Enter or s to request save confirmation.",
            theme.success_style(),
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(panel_block("Review · save", theme))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_palette(frame: &mut Frame<'_>, state: &EditorState, theme: Theme) {
    let area = centered_rect(70, 72, frame.area());
    frame.render_widget(Clear, area);
    let items = action_palette()
        .into_iter()
        .map(|entry| ListItem::new(entry.label))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block("Add action · choose a capability", theme))
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_palette));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_input(
    frame: &mut Frame<'_>,
    field: super::editor::EditorField,
    value: &str,
    theme: Theme,
) {
    let area = centered_rect(70, 24, frame.area());
    frame.render_widget(Clear, area);
    let title = format!(
        "Edit {} · Enter to apply · Esc to cancel",
        field_label(field)
    );
    let content = Paragraph::new(value.to_owned())
        .block(panel_block(&title, theme))
        .style(theme.text_style());
    frame.render_widget(content, area);
}

fn panel_block(title: &str, theme: Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(title.to_owned(), theme.title_style()))
}

fn action_label(action: &ActionSpec) -> String {
    if let Some(label) = &action.display_label {
        return label.clone();
    }

    match &action.kind {
        ActionKind::OpenApplication => "Open application".to_owned(),
        ActionKind::OpenProject => "Open project".to_owned(),
        ActionKind::RunCommand => "Run command".to_owned(),
        ActionKind::StartService => "Start service".to_owned(),
        ActionKind::CreateOrSelectWorkspace => "Create or select workspace".to_owned(),
        ActionKind::ConfigureTiling => "Configure tiling".to_owned(),
        ActionKind::StartContainer => "Start Docker container".to_owned(),
        ActionKind::StartCompose => "Start Docker Compose stack".to_owned(),
        ActionKind::StartAndroidEmulator => "Start Android Emulator".to_owned(),
        ActionKind::WaitForCondition => "Wait for condition".to_owned(),
        ActionKind::VerifyResource => "Verify resource".to_owned(),
        ActionKind::Custom { name } => format!("Custom action: {name}"),
    }
}

fn workspace_target_label(workspace: &WorkspaceSpec) -> String {
    match &workspace.target {
        WorkspaceTarget::Current => "current".to_owned(),
        WorkspaceTarget::Existing { reference } => match reference {
            WorkspaceReference::Name(name) => format!("existing {name}"),
            WorkspaceReference::Identifier(identifier) => format!("existing #{identifier}"),
        },
        WorkspaceTarget::NextEmpty => "next empty".to_owned(),
        WorkspaceTarget::Create { name } => format!("create {name}"),
        WorkspaceTarget::None => "no movement".to_owned(),
    }
}

fn tiling_label(tiling: TilingPreference) -> &'static str {
    match tiling {
        TilingPreference::Unchanged => "unchanged",
        TilingPreference::Enabled => "enabled",
        TilingPreference::Disabled => "disabled",
    }
}

fn execution_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::RunOnce => "run once",
        ExecutionMode::Background => "background",
    }
}

fn environment_status_style(status: EnvironmentStatus, theme: Theme) -> Style {
    match status {
        EnvironmentStatus::Ready => theme.success_style(),
        EnvironmentStatus::Partial => theme.warning_style(),
        EnvironmentStatus::Stopped | EnvironmentStatus::Unknown => theme.muted_style(),
    }
}

fn progress_status_style(status: ActionProgressStatus, theme: Theme) -> Style {
    match status {
        ActionProgressStatus::Ready | ActionProgressStatus::Stopped => theme.success_style(),
        ActionProgressStatus::Failed => theme.error_style(),
        ActionProgressStatus::Running | ActionProgressStatus::RollingBack => theme.warning_style(),
        ActionProgressStatus::Pending | ActionProgressStatus::Skipped => theme.muted_style(),
    }
}

fn field_label(field: super::editor::EditorField) -> &'static str {
    match field {
        super::editor::EditorField::EnvironmentName => "environment name",
        super::editor::EditorField::ActionDisplayLabel => "action label",
        super::editor::EditorField::WorkingDirectory => "working directory",
        super::editor::EditorField::Application => "application",
        super::editor::EditorField::ProjectPath => "project path",
        super::editor::EditorField::CommandProgram => "command",
        super::editor::EditorField::ContainerName => "container name",
        super::editor::EditorField::ComposeProjectName => "Compose project name",
        super::editor::EditorField::EmulatorAvd => "Android virtual device",
        super::editor::EditorField::ReadinessDelay => "readiness delay in milliseconds",
    }
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::{Terminal, backend::TestBackend};

    use crate::domain::{
        ActionKind, ActionSpec, EnvironmentConfig, EnvironmentName, EnvironmentSlug, ExecutionMode,
        Timeout,
    };

    use super::{
        Theme, render_delete_confirmation, render_editor, render_progress, render_selector,
    };
    use crate::ui::{
        ActionProgressStatus, EnvironmentListItem, EnvironmentStatus, ProgressEvent, ProgressState,
        SelectorState,
    };
    use crate::ui::{EditorMode, EditorState};

    #[test]
    fn selector_empty_state_renders_an_actionable_snapshot() {
        let backend = TestBackend::new(64, 8);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(_) => return,
        };
        let state = SelectorState::new(Vec::new());
        let result = terminal.draw(|frame| render_selector(frame, &state, Theme::new(false)));
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let rendered = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("No environments yet"));
        let configuration = EnvironmentConfig::new("Snapshot");
        assert!(configuration.is_ok());
    }

    #[test]
    fn populated_selector_renders_name_slug_and_status() {
        let Some(name) = EnvironmentName::new("Personal Blog").ok() else {
            return;
        };
        let Some(slug) = EnvironmentSlug::new("personal-blog").ok() else {
            return;
        };
        let state = SelectorState::new(vec![EnvironmentListItem::new(
            name,
            slug,
            EnvironmentStatus::Ready,
        )]);
        let backend = TestBackend::new(80, 8);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let result = terminal.draw(|frame| render_selector(frame, &state, Theme::new(false)));
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let rendered = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Personal Blog"));
        assert!(rendered.contains("personal-blog"));
        assert!(rendered.contains("ready"));
    }

    #[test]
    fn editor_snapshot_contains_the_dynamic_builder_surface_and_validation() {
        let Some(mut configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let mut state = EditorState::new(configuration, EditorMode::Create);
        assert!(state.validate().is_err());
        let backend = TestBackend::new(120, 30);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let result = terminal.draw(|frame| render_editor(frame, &state, Theme::new(false)));
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let rendered = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Personal Blog"));
        assert!(rendered.contains("Validation"));
        assert!(rendered.contains("Open project"));
    }

    #[test]
    fn progress_snapshot_contains_timeout_and_runtime_status() {
        let Some(mut configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let Some(mut action) = ActionSpec::new("api", ActionKind::RunCommand).ok() else {
            return;
        };
        action.execution_mode = Some(ExecutionMode::Background);
        action.timeout = Timeout::new(1_000).ok();
        assert!(configuration.add_action(action.clone()).is_ok());
        let mut state = ProgressState::from_configuration(&configuration);
        assert!(
            state
                .apply(ProgressEvent::ActionStarted {
                    action_id: action.id.clone(),
                })
                .is_ok()
        );
        assert!(
            state
                .apply(ProgressEvent::ClockAdvanced {
                    elapsed: Duration::from_millis(100),
                })
                .is_ok()
        );
        let backend = TestBackend::new(120, 20);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let result = terminal.draw(|frame| render_progress(frame, &state, Theme::new(false)));
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let rendered = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("running"));
        assert!(rendered.contains("100/1000 ms"));
        assert_eq!(
            state.entry(&action.id).map(|entry| entry.status),
            Some(ActionProgressStatus::Running)
        );
    }

    #[test]
    fn delete_snapshot_explains_active_cleanup_and_directory_removal() {
        let Some(name) = EnvironmentName::new("Personal Blog").ok() else {
            return;
        };
        let backend = TestBackend::new(120, 20);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let result = terminal.draw(|frame| {
            render_delete_confirmation(
                frame,
                &name,
                std::path::Path::new("/home/example/.workstate/personal-blog"),
                true,
                Theme::new(false),
            )
        });
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let rendered = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Active resources will be stopped first"));
        assert!(rendered.contains("directory will then be removed"));
        assert!(rendered.contains("y confirm"));
    }
}
