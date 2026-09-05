use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::domain::{ActionKind, ActionSpec, EnvironmentName};

use super::{
    editor::{EditorPanel, EditorState, InspectorChoice, InspectorPicker, action_palette},
    progress::{ActionProgressStatus, ProgressOperation, ProgressState},
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
    let footer_height = editor_footer_height(state);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(footer_height),
        ])
        .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Workstate", theme.brand_style()),
        Span::styled("  ", theme.muted_style()),
        Span::styled(state.configuration.name.to_string(), theme.text_style()),
    ]))
    .block(panel_block("Environment", theme))
    .style(theme.text_style());
    frame.render_widget(header, sections[0]);

    let inspector_width = match state.panel {
        EditorPanel::Actions => 25,
        EditorPanel::Inspector => 80,
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(100 - inspector_width),
            Constraint::Percentage(inspector_width),
        ])
        .split(sections[1]);

    render_action_list(frame, state, theme, columns[0]);
    render_inspector(frame, state, theme, columns[1]);

    render_editor_footer(frame, state, theme, sections[2]);

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
    if let Some(picker) = &state.inspector_picker {
        render_inspector_picker(frame, state, picker, theme);
    }
    if state.workspace_picker_open {
        render_live_workspace_picker(frame, state, theme);
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
    let summary = if state.total_count() == 0 {
        format!(
            " {}  ·  no actions configured  ·  {} ms",
            state.environment_name,
            state.elapsed.as_millis()
        )
    } else {
        format!(
            " {}  ·  {} / {} complete  ·  {} running  ·  {} pending  ·  {} ms",
            state.environment_name,
            state.ready_count(),
            state.total_count(),
            state.running_count(),
            state.pending_count(),
            state.elapsed.as_millis()
        )
    };
    frame.render_widget(
        Paragraph::new(summary).block(panel_block(state.operation.title(), theme)),
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
                    format!("{}  ", progress_marker(entry.status, state.spinner())),
                    progress_status_style(entry.status, theme),
                ),
                Span::styled(entry.label.clone(), theme.text_style()),
                Span::styled(
                    format!("  {}", entry.status),
                    progress_status_style(entry.status, theme),
                ),
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
            .block(panel_block("Activity", theme)),
        columns[1],
    );

    let footer = match (state.operation, state.successful) {
        (ProgressOperation::Run, Some(true)) => "Environment ready · closing",
        (ProgressOperation::Run, Some(false)) => "Run failed · rollback finished · closing",
        (ProgressOperation::Stop, Some(true)) => "Environment stopped · closing",
        (ProgressOperation::Stop, Some(false)) => "Stop failed · closing",
        (_, None) => "Live lifecycle updates · the interface closes when the operation finishes",
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
    let mut items = state
        .configuration
        .actions
        .iter()
        .map(|action| {
            ListItem::new(Line::from(vec![Span::styled(
                action_label(action),
                theme.text_style(),
            )]))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new(Span::styled(
            "No actions yet. Press a to add an action.",
            theme.muted_style(),
        )));
    }
    let list = List::new(items)
        .block(focused_panel_block(
            "Actions",
            theme,
            state.panel == EditorPanel::Actions,
        ))
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_action);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_live_workspace_picker(frame: &mut Frame<'_>, state: &EditorState, theme: Theme) {
    let area = centered_rect(70, 66, frame.area());
    frame.render_widget(Clear, area);
    let items = state
        .live_workspaces
        .iter()
        .map(|workspace| {
            let label = workspace
                .name
                .as_deref()
                .unwrap_or(workspace.identity.as_str());
            let active = if workspace.focused { " · active" } else { "" };
            let tiling = workspace
                .tiling_enabled
                .map(|enabled| if enabled { "on" } else { "off" })
                .unwrap_or("unknown");
            ListItem::new(Line::from(vec![
                Span::styled(label.to_owned(), theme.text_style()),
                Span::styled(format!("  #{}", workspace.identity), theme.muted_style()),
                Span::styled(active, theme.success_style()),
                Span::styled(format!(" · tiling {tiling}"), theme.muted_style()),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block(
            "Select COSMIC workspace · Enter confirm · Esc cancel",
            theme,
        ))
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_live_workspace);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_inspector(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let Some(action) = state.selected_action_spec() else {
        frame.render_widget(
            Paragraph::new("Select an action to inspect its fields.")
                .block(focused_panel_block(
                    "Inspector",
                    theme,
                    state.panel == EditorPanel::Inspector,
                ))
                .style(theme.muted_style())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    let fields = state.inspector_fields();
    let items = fields
        .iter()
        .map(|field| {
            let label = format!("{:<22}", field.label());
            ListItem::new(Line::from(vec![
                Span::styled(label, theme.muted_style()),
                Span::styled(state.inspector_field_value(*field), theme.text_style()),
            ]))
        })
        .collect::<Vec<_>>();
    let title = format!("Inspector · {}", action_label(action));
    let list = List::new(items)
        .block(focused_panel_block(
            &title,
            theme,
            state.panel == EditorPanel::Inspector,
        ))
        .highlight_style(theme.selected_style())
        .highlight_symbol("▸ ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_inspector);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn editor_footer_height(state: &EditorState) -> u16 {
    let controls_height = 2;
    if state.validation_errors.is_empty() {
        return controls_height + u16::from(state.notice.is_some());
    }

    let displayed_errors = state.validation_errors.len().min(5) as u16;
    let overflow_line = u16::from(state.validation_errors.len() > 5);
    controls_height + 1 + displayed_errors + overflow_line
}

fn render_editor_footer(frame: &mut Frame<'_>, state: &EditorState, theme: Theme, area: Rect) {
    let mut lines = Vec::new();
    if !state.validation_errors.is_empty() {
        lines.push(Line::from(Span::styled(
            "Validation errors",
            theme.error_style(),
        )));
        lines.extend(
            state
                .validation_errors
                .iter()
                .take(5)
                .map(|error| Line::from(Span::styled(format!("  {error}"), theme.error_style()))),
        );
        if state.validation_errors.len() > 5 {
            lines.push(Line::from(Span::styled(
                format!("  ... and {} more", state.validation_errors.len() - 5),
                theme.error_style(),
            )));
        }
    } else if let Some(notice) = &state.notice {
        lines.push(Line::from(Span::styled(
            notice.clone(),
            theme.warning_style(),
        )));
    }
    lines.push(render_editor_controls(state, theme));

    let footer = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(theme.border_style()),
    );
    frame.render_widget(footer, area);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorControl {
    key: &'static str,
    label: &'static str,
}

fn render_editor_controls(state: &EditorState, theme: Theme) -> Line<'static> {
    let controls = editor_controls(state);
    let mut spans = Vec::with_capacity(controls.len() * 3);
    for (index, control) in controls.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(control.key, theme.key_style()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(control.label, theme.muted_style()));
    }
    Line::from(spans)
}

fn editor_controls(state: &EditorState) -> Vec<EditorControl> {
    if state.input.is_some() {
        return vec![
            EditorControl {
                key: "Enter",
                label: "Apply",
            },
            EditorControl {
                key: "Esc",
                label: "Cancel",
            },
        ];
    }
    if state.palette_open {
        return vec![
            EditorControl {
                key: "↑↓",
                label: "Move",
            },
            EditorControl {
                key: "Enter",
                label: "Add action",
            },
            EditorControl {
                key: "Esc",
                label: "Cancel",
            },
        ];
    }
    if state.delete_confirmation {
        return vec![
            EditorControl {
                key: "y",
                label: "Confirm",
            },
            EditorControl {
                key: "Esc",
                label: "Cancel",
            },
        ];
    }
    if state.workspace_picker_open {
        return vec![
            EditorControl {
                key: "↑↓",
                label: "Navigate",
            },
            EditorControl {
                key: "Enter",
                label: "Select",
            },
            EditorControl {
                key: "Esc",
                label: "Cancel",
            },
        ];
    }
    if let Some(picker) = &state.inspector_picker {
        return match picker {
            InspectorPicker::Choices { .. } => vec![
                EditorControl {
                    key: "↑↓",
                    label: "Navigate",
                },
                EditorControl {
                    key: "Enter",
                    label: "Select",
                },
                EditorControl {
                    key: "Esc",
                    label: "Cancel",
                },
            ],
            InspectorPicker::Dependencies { .. } => vec![
                EditorControl {
                    key: "↑↓",
                    label: "Navigate",
                },
                EditorControl {
                    key: "Space",
                    label: "Toggle",
                },
                EditorControl {
                    key: "Enter",
                    label: "Confirm",
                },
                EditorControl {
                    key: "Esc",
                    label: "Cancel",
                },
            ],
        };
    }

    match (state.panel, state.selected_action.is_some()) {
        (EditorPanel::Actions, true) => vec![
            EditorControl {
                key: "↑↓",
                label: "Move",
            },
            EditorControl {
                key: "→",
                label: "Inspect",
            },
            EditorControl {
                key: "a",
                label: "Add action",
            },
            EditorControl {
                key: "d",
                label: "Delete action",
            },
            EditorControl {
                key: "s",
                label: "Save",
            },
            EditorControl {
                key: "q",
                label: "Exit",
            },
        ],
        (EditorPanel::Actions, false) => vec![
            EditorControl {
                key: "↑↓",
                label: "Navigate",
            },
            EditorControl {
                key: "a",
                label: "Add action",
            },
            EditorControl {
                key: "s",
                label: "Save",
            },
            EditorControl {
                key: "q",
                label: "Exit",
            },
        ],
        (EditorPanel::Inspector, true) => vec![
            EditorControl {
                key: "↑↓",
                label: "Move",
            },
            EditorControl {
                key: "←",
                label: "Back",
            },
            EditorControl {
                key: "Enter",
                label: "Edit field",
            },
            EditorControl {
                key: "s",
                label: "Save",
            },
            EditorControl {
                key: "q",
                label: "Exit",
            },
        ],
        (EditorPanel::Inspector, false) => vec![
            EditorControl {
                key: "←",
                label: "Back",
            },
            EditorControl {
                key: "s",
                label: "Save",
            },
            EditorControl {
                key: "q",
                label: "Exit",
            },
        ],
    }
}

fn render_inspector_picker(
    frame: &mut Frame<'_>,
    state: &EditorState,
    picker: &InspectorPicker,
    theme: Theme,
) {
    match picker {
        InspectorPicker::Choices {
            title,
            options,
            selected,
            ..
        } => {
            let items = options
                .iter()
                .map(|choice| render_choice(choice, theme))
                .collect::<Vec<_>>();
            let list = List::new(items)
                .block(panel_block(&format!("Select {title}"), theme))
                .highlight_style(theme.selected_style())
                .highlight_symbol("▸ ");
            let mut list_state = ListState::default();
            list_state.select(Some(*selected));
            let area = centered_rect(68, 62, frame.area());
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(list, area, &mut list_state);
        }
        InspectorPicker::Dependencies {
            options,
            selected,
            checked,
            ..
        } => {
            let mut items = options
                .iter()
                .map(|action_id| {
                    let marker = if checked.contains(action_id) {
                        "[x] "
                    } else {
                        "[ ] "
                    };
                    let label = state.action_label_for_id(action_id);
                    ListItem::new(Line::from(vec![
                        Span::styled(marker, theme.muted_style()),
                        Span::styled(label, theme.text_style()),
                    ]))
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                items.push(ListItem::new(Span::styled(
                    "No other actions are available.",
                    theme.muted_style(),
                )));
            }
            let list = List::new(items)
                .block(panel_block(
                    "Select dependencies · Space toggle · Enter apply",
                    theme,
                ))
                .highlight_style(theme.selected_style())
                .highlight_symbol("▸ ");
            let mut list_state = ListState::default();
            list_state.select((!options.is_empty()).then_some(*selected));
            let area = centered_rect(68, 62, frame.area());
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(list, area, &mut list_state);
        }
    }
}

fn render_choice(choice: &InspectorChoice, theme: Theme) -> ListItem<'static> {
    let mut spans = vec![Span::styled(choice.label.clone(), theme.text_style())];
    if let Some(detail) = &choice.detail {
        spans.push(Span::styled(format!("  {detail}"), theme.muted_style()));
    }
    ListItem::new(Line::from(spans))
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

fn focused_panel_block(title: &str, theme: Theme, focused: bool) -> Block<'static> {
    let title_style = if focused {
        theme.title_style()
    } else {
        theme.muted_style()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(theme.title)
        } else {
            theme.border_style()
        })
        .title(Span::styled(title.to_owned(), title_style))
}

fn action_label(action: &ActionSpec) -> String {
    if let Some(label) = &action.display_label {
        return label.clone();
    }

    match &action.kind {
        ActionKind::OpenApplication => "Open application".to_owned(),
        ActionKind::OpenProject => "Open Project with Zed".to_owned(),
        ActionKind::RunCommand => "Run command".to_owned(),
        ActionKind::StartService => "Start service".to_owned(),
        ActionKind::ConfigureTiling => "Configure tiling".to_owned(),
        ActionKind::StartContainer => "Start Docker container".to_owned(),
        ActionKind::StartCompose => "Start Docker Compose stack".to_owned(),
        ActionKind::StartAndroidEmulator => "Start Android Emulator".to_owned(),
        ActionKind::WaitForCondition => "Wait for condition".to_owned(),
        ActionKind::VerifyResource => "Verify resource".to_owned(),
        ActionKind::Custom { name } => format!("Custom action: {name}"),
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
        ActionProgressStatus::Pending
        | ActionProgressStatus::Skipped
        | ActionProgressStatus::Cancelled => theme.muted_style(),
    }
}

fn progress_marker(status: ActionProgressStatus, spinner: &str) -> &str {
    match status {
        ActionProgressStatus::Pending => "·",
        ActionProgressStatus::Running => spinner,
        ActionProgressStatus::Ready | ActionProgressStatus::Stopped => "✓",
        ActionProgressStatus::Skipped => "↷",
        ActionProgressStatus::Failed => "✗",
        ActionProgressStatus::Cancelled => "!",
        ActionProgressStatus::RollingBack => "↺",
    }
}

fn field_label(field: super::editor::EditorField) -> &'static str {
    match field {
        super::editor::EditorField::EnvironmentName => "environment name",
        super::editor::EditorField::ActionDisplayLabel => "action label",
        super::editor::EditorField::WorkingDirectory => "working directory",
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

    use crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend};

    use crate::domain::{
        ActionKind, ActionSpec, EnvironmentConfig, EnvironmentName, EnvironmentSlug, ExecutionMode,
        Timeout,
    };

    use super::{
        Theme, editor_controls, render_delete_confirmation, render_editor, render_editor_controls,
        render_inspector, render_progress, render_selector,
    };
    use crate::ui::{
        ActionProgressStatus, EnvironmentListItem, EnvironmentStatus, ProgressEvent, ProgressState,
        SelectorState,
    };
    use crate::ui::{EditorMode, EditorState};

    fn control_pairs(state: &EditorState) -> Vec<(&'static str, &'static str)> {
        editor_controls(state)
            .into_iter()
            .map(|control| (control.key, control.label))
            .collect()
    }

    #[test]
    fn editor_footer_controls_follow_the_active_panel_and_selection() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let empty_state = EditorState::new(configuration, EditorMode::Create);
        assert_eq!(
            control_pairs(&empty_state),
            vec![
                ("↑↓", "Navigate"),
                ("a", "Add action"),
                ("s", "Save"),
                ("q", "Exit")
            ]
        );

        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("run-command", ActionKind::RunCommand).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let mut state = EditorState::new(configuration, EditorMode::Create);
        assert_eq!(
            control_pairs(&state),
            vec![
                ("↑↓", "Move"),
                ("→", "Inspect"),
                ("a", "Add action"),
                ("d", "Delete action"),
                ("s", "Save"),
                ("q", "Exit"),
            ]
        );

        state.panel = crate::ui::EditorPanel::Inspector;
        assert_eq!(
            control_pairs(&state),
            vec![
                ("↑↓", "Move"),
                ("←", "Back"),
                ("Enter", "Edit field"),
                ("s", "Save"),
                ("q", "Exit"),
            ]
        );
    }

    #[test]
    fn editor_footer_keys_use_a_distinct_style_from_labels() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let state = EditorState::new(configuration, EditorMode::Create);
        let line = render_editor_controls(&state, Theme::new(true));
        assert!(line.spans.len() >= 3);
        assert_ne!(line.spans[0].style, line.spans[2].style);
    }

    #[test]
    fn editor_footer_controls_are_visible_without_messages() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let state = EditorState::new(configuration, EditorMode::Create);
        let backend = TestBackend::new(120, 10);
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
        assert!(rendered.contains("s Save"));
        assert!(rendered.contains("a Add action"));
    }

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
        assert!(rendered.contains("Validation errors"));
        assert!(rendered.contains("s Save"));
        assert!(rendered.contains("Open Project with Zed"));
        assert!(rendered.contains("Project path"));
        assert!(rendered.contains("Desktop workspace"));
        assert!(!rendered.contains("Working directory"));
        assert!(!rendered.contains("Execution mode"));
        assert!(!rendered.contains("Application"));

        let backend = TestBackend::new(100, 16);
        let Ok(mut terminal) = Terminal::new(backend) else {
            return;
        };
        let result = terminal.draw(|frame| {
            let area = frame.area();
            render_inspector(frame, &state, Theme::new(false), area);
        });
        assert!(result.is_ok());
        let Some(completed) = result.ok() else {
            return;
        };
        let inspector = completed
            .buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!inspector.contains("Validation"));
    }

    #[test]
    fn dependency_picker_renders_action_names_instead_of_action_ids() {
        let Some(mut configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let Some(mut api) = ActionSpec::new("api", ActionKind::RunCommand).ok() else {
            return;
        };
        let Some(mut mobile) = ActionSpec::new("mobile", ActionKind::RunCommand).ok() else {
            return;
        };
        api.display_label = Some("Open API".to_owned());
        mobile.display_label = Some("Open Mobile API".to_owned());
        let api_id = api.id.clone();
        mobile.depends_on.push(api_id);
        assert!(configuration.add_action(api).is_ok());
        assert!(configuration.add_action(mobile).is_ok());

        let mut state = EditorState::new(configuration, EditorMode::Create);
        state.selected_action = Some(1);
        state.panel = crate::ui::EditorPanel::Inspector;
        state.selected_inspector = Some(state.inspector_fields().len().saturating_sub(1));
        state.handle_key(KeyCode::Enter);

        let backend = TestBackend::new(100, 20);
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
        assert!(rendered.contains("Open API"));
        assert!(!rendered.contains("api"));
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
