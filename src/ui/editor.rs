use std::{collections::BTreeSet, path::PathBuf};

use crossterm::event::KeyCode;

use crate::{
    application::ports::{ConfigStore, FileSystem},
    domain::{
        ActionId, ActionKind, ActionSpec, CommandSpec, ComposeSpec, ContainerSpec, EmulatorSpec,
        EnvironmentConfig, ExecutionMode, ReadinessCheck, TilingPreference, WorkspaceId,
        WorkspaceSpec, WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::filesystem::PathResolver,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Create,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorPanel {
    Actions,
    Workspaces,
    Inspector,
    Review,
}

impl EditorPanel {
    fn next(self) -> Self {
        match self {
            Self::Actions => Self::Workspaces,
            Self::Workspaces => Self::Inspector,
            Self::Inspector => Self::Review,
            Self::Review => Self::Actions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorField {
    EnvironmentName,
    ActionDisplayLabel,
    WorkingDirectory,
    Application,
    ProjectPath,
    CommandProgram,
    ContainerName,
    ComposeProjectName,
    EmulatorAvd,
    ReadinessDelay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    pub field: EditorField,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPaletteEntry {
    pub label: &'static str,
    pub kind: ActionKind,
}

pub fn action_palette() -> Vec<ActionPaletteEntry> {
    vec![
        ActionPaletteEntry {
            label: "Open application",
            kind: ActionKind::OpenApplication,
        },
        ActionPaletteEntry {
            label: "Open project",
            kind: ActionKind::OpenProject,
        },
        ActionPaletteEntry {
            label: "Run command",
            kind: ActionKind::RunCommand,
        },
        ActionPaletteEntry {
            label: "Start service",
            kind: ActionKind::StartService,
        },
        ActionPaletteEntry {
            label: "Create or select workspace",
            kind: ActionKind::CreateOrSelectWorkspace,
        },
        ActionPaletteEntry {
            label: "Configure tiling",
            kind: ActionKind::ConfigureTiling,
        },
        ActionPaletteEntry {
            label: "Start Docker container",
            kind: ActionKind::StartContainer,
        },
        ActionPaletteEntry {
            label: "Start Docker Compose stack",
            kind: ActionKind::StartCompose,
        },
        ActionPaletteEntry {
            label: "Start Android Emulator",
            kind: ActionKind::StartAndroidEmulator,
        },
        ActionPaletteEntry {
            label: "Wait for condition",
            kind: ActionKind::WaitForCondition,
        },
        ActionPaletteEntry {
            label: "Verify resource",
            kind: ActionKind::VerifyResource,
        },
        ActionPaletteEntry {
            label: "Custom action",
            kind: ActionKind::Custom {
                name: "custom-action".to_owned(),
            },
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    None,
    SaveRequested,
    CancelRequested,
    PaletteOpened,
    ReviewOpened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorReview {
    pub environment_name: String,
    pub environment_slug: String,
    pub workspace_count: usize,
    pub action_count: usize,
    pub dependencies: Vec<String>,
    pub valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    ConfirmationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    pub configuration: EnvironmentConfig,
    pub mode: EditorMode,
    pub panel: EditorPanel,
    pub selected_action: Option<usize>,
    pub selected_workspace: Option<usize>,
    pub selected_palette: usize,
    pub palette_open: bool,
    pub delete_confirmation: bool,
    pub input: Option<TextInput>,
    pub validation_errors: Vec<String>,
    pub notice: Option<String>,
    pub dirty: bool,
}

impl EditorState {
    pub fn new(configuration: EnvironmentConfig, mode: EditorMode) -> Self {
        let selected_action = (!configuration.actions.is_empty()).then_some(0);
        let selected_workspace = (!configuration.workspaces.is_empty()).then_some(0);
        Self {
            configuration,
            mode,
            panel: EditorPanel::Actions,
            selected_action,
            selected_workspace,
            selected_palette: 0,
            palette_open: false,
            delete_confirmation: false,
            input: None,
            validation_errors: Vec::new(),
            notice: None,
            dirty: false,
        }
    }

    pub fn action_palette(&self) -> Vec<ActionPaletteEntry> {
        action_palette()
    }

    pub fn selected_action_spec(&self) -> Option<&ActionSpec> {
        self.selected_action
            .and_then(|index| self.configuration.actions.get(index))
    }

    pub fn selected_action_spec_mut(&mut self) -> Option<&mut ActionSpec> {
        self.selected_action
            .and_then(|index| self.configuration.actions.get_mut(index))
    }

    pub fn selected_workspace_spec(&self) -> Option<&WorkspaceSpec> {
        self.selected_workspace
            .and_then(|index| self.configuration.workspaces.get(index))
    }

    pub fn add_action_from_palette(&mut self, palette_index: usize) -> Result<ActionId> {
        let palette = action_palette();
        let Some(entry) = palette.get(palette_index) else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "the selected action palette entry does not exist",
            ));
        };
        let id = self.next_action_id(&entry.kind)?;
        let mut action = ActionSpec::new(id.as_str().to_owned(), entry.kind.clone())
            .map_err(WorkstateError::from)?;
        action.display_label = Some(entry.label.to_owned());
        self.configuration
            .add_action(action)
            .map_err(WorkstateError::from)?;
        self.selected_action = self.configuration.actions.len().checked_sub(1);
        self.panel = EditorPanel::Inspector;
        self.palette_open = false;
        self.mark_dirty();
        Ok(id)
    }

    pub fn remove_action(&mut self, index: usize) -> Result<ActionId> {
        if index >= self.configuration.actions.len() {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "the selected action does not exist",
            ));
        }
        let removed = self.configuration.actions.remove(index);
        let removed_id = removed.id.clone();
        for action in &mut self.configuration.actions {
            action
                .depends_on
                .retain(|dependency| dependency != &removed_id);
        }
        self.selected_action = selection_after_removal(self.configuration.actions.len(), index);
        self.mark_dirty();
        Ok(removed_id)
    }

    pub fn add_workspace(
        &mut self,
        id: impl Into<String>,
        target: WorkspaceTarget,
    ) -> Result<WorkspaceId> {
        let workspace = WorkspaceSpec::new(id, target).map_err(WorkstateError::from)?;
        let workspace_id = workspace.id.clone();
        self.configuration
            .add_workspace(workspace)
            .map_err(WorkstateError::from)?;
        self.selected_workspace = self.configuration.workspaces.len().checked_sub(1);
        self.mark_dirty();
        Ok(workspace_id)
    }

    pub fn set_workspace_target(
        &mut self,
        workspace_id: &WorkspaceId,
        target: WorkspaceTarget,
    ) -> Result<()> {
        let Some(workspace) = self
            .configuration
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.id == workspace_id)
        else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("workspace '{workspace_id}' does not exist"),
            ));
        };
        workspace.target = target;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_workspace_tiling(
        &mut self,
        workspace_id: &WorkspaceId,
        tiling: TilingPreference,
    ) -> Result<()> {
        let Some(workspace) = self
            .configuration
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.id == workspace_id)
        else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("workspace '{workspace_id}' does not exist"),
            ));
        };
        workspace.tiling = tiling;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_display_label(
        &mut self,
        action_id: &ActionId,
        label: impl Into<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        action.display_label = Some(label.into());
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_working_directory(
        &mut self,
        action_id: &ActionId,
        path: Option<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        action.working_directory = path;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_desktop_workspace(
        &mut self,
        action_id: &ActionId,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        action.desktop_workspace = workspace_id;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_execution_mode(
        &mut self,
        action_id: &ActionId,
        mode: Option<ExecutionMode>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if mode.is_some()
            && !matches!(
                &action.kind,
                ActionKind::RunCommand | ActionKind::StartService
            )
        {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!(
                    "execution mode is only available for command actions such as '{action_id}'"
                ),
            ));
        }
        action.execution_mode = mode;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_application(
        &mut self,
        action_id: &ActionId,
        application: Option<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(
            &action.kind,
            ActionKind::OpenApplication | ActionKind::OpenProject
        ) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("application is not available for action '{action_id}'"),
            ));
        }
        action.parameters.application = application;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_project_path(
        &mut self,
        action_id: &ActionId,
        project_path: Option<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::OpenProject) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("project path is not available for action '{action_id}'"),
            ));
        }
        action.parameters.project_path = project_path;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_command(
        &mut self,
        action_id: &ActionId,
        command: Option<CommandSpec>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(
            &action.kind,
            ActionKind::RunCommand | ActionKind::StartService
        ) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("command is not available for action '{action_id}'"),
            ));
        }
        action.parameters.command = command;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_container_name(&mut self, action_id: &ActionId, name: String) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::StartContainer) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("container configuration is not available for action '{action_id}'"),
            ));
        }
        let container = action
            .parameters
            .container
            .get_or_insert_with(|| ContainerSpec {
                name: String::new(),
                image: None,
                command: None,
            });
        container.name = name;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_compose_project_name(
        &mut self,
        action_id: &ActionId,
        project_name: String,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::StartCompose) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("Compose configuration is not available for action '{action_id}'"),
            ));
        }
        let compose = action
            .parameters
            .compose
            .get_or_insert_with(|| ComposeSpec {
                project_name: None,
                files: Vec::new(),
                services: Vec::new(),
                up_command: None,
                down_command: None,
            });
        compose.project_name = Some(project_name);
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_emulator_avd(&mut self, action_id: &ActionId, avd: String) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::StartAndroidEmulator) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("emulator configuration is not available for action '{action_id}'"),
            ));
        }
        let emulator = action
            .parameters
            .emulator
            .get_or_insert_with(|| EmulatorSpec {
                avd: String::new(),
                arguments: Vec::new(),
            });
        emulator.avd = avd;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_readiness_delay(
        &mut self,
        action_id: &ActionId,
        value: String,
    ) -> Result<()> {
        let milliseconds = value.parse::<u64>().map_err(|_| {
            WorkstateError::new(
                ErrorCategory::Ui,
                "readiness delay must be a positive number of milliseconds",
            )
        })?;
        if milliseconds == 0 {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "readiness delay must be a positive number of milliseconds",
            ));
        }
        let action = self.action_mut(action_id)?;
        if !matches!(
            &action.kind,
            ActionKind::WaitForCondition | ActionKind::VerifyResource
        ) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("readiness checks are not available for action '{action_id}'"),
            ));
        }
        if let Some(check) = action.readiness_checks.first_mut() {
            *check = ReadinessCheck::Delay { milliseconds };
        } else {
            action
                .readiness_checks
                .push(ReadinessCheck::Delay { milliseconds });
        }
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_workspace_parameter(
        &mut self,
        action_id: &ActionId,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::CreateOrSelectWorkspace) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("workspace parameter is not available for action '{action_id}'"),
            ));
        }
        action.parameters.workspace_id = workspace_id;
        self.mark_dirty();
        Ok(())
    }

    pub fn cycle_selected_workspace_target(&mut self) -> Result<()> {
        let Some(index) = self.selected_workspace else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "no workspace is selected",
            ));
        };
        let Some(workspace) = self.configuration.workspaces.get(index) else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "the selected workspace does not exist",
            ));
        };
        let display_name = workspace
            .name
            .clone()
            .unwrap_or_else(|| workspace.id.to_string());
        let target = match &workspace.target {
            WorkspaceTarget::Current => WorkspaceTarget::NextEmpty,
            WorkspaceTarget::NextEmpty => WorkspaceTarget::Create {
                name: display_name.clone(),
            },
            WorkspaceTarget::Create { .. } => WorkspaceTarget::Existing {
                reference: crate::domain::WorkspaceReference::Name(display_name),
            },
            WorkspaceTarget::Existing { .. } => WorkspaceTarget::None,
            WorkspaceTarget::None => WorkspaceTarget::Current,
        };
        let workspace_id = workspace.id.clone();
        self.set_workspace_target(&workspace_id, target)
    }

    pub fn toggle_selected_workspace_tiling(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace_spec() else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "no workspace is selected",
            ));
        };
        let tiling = match workspace.tiling {
            TilingPreference::Unchanged | TilingPreference::Disabled => TilingPreference::Enabled,
            TilingPreference::Enabled => TilingPreference::Disabled,
        };
        let workspace_id = workspace.id.clone();
        self.set_workspace_tiling(&workspace_id, tiling)
    }

    pub fn cycle_selected_action_workspace(&mut self) -> Result<()> {
        let action_id = self.selected_action_id()?;
        let current = self.action(&action_id)?.desktop_workspace.clone();
        let mut targets = Vec::with_capacity(self.configuration.workspaces.len() + 1);
        targets.push(None);
        targets.extend(
            self.configuration
                .workspaces
                .iter()
                .map(|workspace| Some(workspace.id.clone())),
        );
        let current_index = targets
            .iter()
            .position(|target| target == &current)
            .unwrap_or(0);
        let next_index = (current_index + 1) % targets.len();
        self.set_action_desktop_workspace(&action_id, targets[next_index].clone())
    }

    pub fn cycle_selected_action_execution_mode(&mut self) -> Result<()> {
        let action_id = self.selected_action_id()?;
        let mode = self.action(&action_id)?.execution_mode;
        let next = match mode {
            None => Some(ExecutionMode::RunOnce),
            Some(ExecutionMode::RunOnce) => Some(ExecutionMode::Background),
            Some(ExecutionMode::Background) => None,
        };
        self.set_action_execution_mode(&action_id, next)
    }

    pub fn add_dependency_to_selected_action(&mut self) -> Result<()> {
        let action_id = self.selected_action_id()?;
        let Some(dependency_id) = self
            .configuration
            .actions
            .iter()
            .find(|action| {
                action.id != action_id
                    && !self
                        .action(&action_id)
                        .is_ok_and(|selected| selected.depends_on.contains(&action.id))
            })
            .map(|action| action.id.clone())
        else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "no available action can be added as a dependency",
            ));
        };
        self.add_dependency(&action_id, &dependency_id)
    }

    pub fn remove_last_dependency_from_selected_action(&mut self) -> Result<()> {
        let action_id = self.selected_action_id()?;
        let Some(dependency_id) = self.action(&action_id)?.depends_on.last().cloned() else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "the selected action has no dependencies",
            ));
        };
        self.remove_dependency(&action_id, &dependency_id)
    }

    pub fn add_dependency(&mut self, action_id: &ActionId, dependency_id: &ActionId) -> Result<()> {
        if action_id == dependency_id {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("action '{action_id}' cannot depend on itself"),
            ));
        }
        if !self.has_action(action_id) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("action '{action_id}' does not exist"),
            ));
        }
        if !self.has_action(dependency_id) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("dependency '{dependency_id}' does not exist"),
            ));
        }
        let action = self.action_mut(action_id)?;
        if action.depends_on.contains(dependency_id) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("action '{action_id}' already depends on '{dependency_id}'"),
            ));
        }
        action.depends_on.push(dependency_id.clone());
        self.mark_dirty();
        Ok(())
    }

    pub fn remove_dependency(
        &mut self,
        action_id: &ActionId,
        dependency_id: &ActionId,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        action
            .depends_on
            .retain(|dependency| dependency != dependency_id);
        self.mark_dirty();
        Ok(())
    }

    pub fn dependency_path(&self, action_id: &ActionId) -> Vec<String> {
        let mut paths = Vec::new();
        let mut visited = BTreeSet::new();
        self.collect_dependency_paths(action_id, action_id.to_string(), &mut visited, &mut paths);
        paths
    }

    pub fn validate(&mut self) -> Result<()> {
        match self.configuration.validate() {
            Ok(()) => {
                self.validation_errors.clear();
                Ok(())
            }
            Err(error) => {
                self.validation_errors = vec![error.to_string()];
                Err(WorkstateError::from(error))
            }
        }
    }

    pub fn validate_path(
        &mut self,
        action_id: &ActionId,
        home: PathBuf,
        file_system: &dyn FileSystem,
    ) -> Result<PathBuf> {
        let Some(path) = self.action(action_id)?.working_directory.clone() else {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("action '{action_id}' does not have a working directory"),
            ));
        };
        let resolver = PathResolver::new(home, file_system)?;
        match resolver.resolve_directory(&path) {
            Ok(resolved) => Ok(resolved),
            Err(error) => {
                self.validation_errors.push(error.to_string());
                Err(error)
            }
        }
    }

    pub fn review(&mut self) -> EditorReview {
        let valid = self.validate().is_ok();
        let dependencies = self
            .configuration
            .actions
            .iter()
            .flat_map(|action| {
                action
                    .depends_on
                    .iter()
                    .map(|dependency| format!("{} -> {}", action.id, dependency))
            })
            .collect();
        EditorReview {
            environment_name: self.configuration.name.to_string(),
            environment_slug: self.configuration.slug.to_string(),
            workspace_count: self.configuration.workspaces.len(),
            action_count: self.configuration.actions.len(),
            dependencies,
            valid,
        }
    }

    pub fn save(&mut self, store: &dyn ConfigStore, confirmed: bool) -> Result<SaveOutcome> {
        self.validate()?;
        if !confirmed {
            return Ok(SaveOutcome::ConfirmationRequired);
        }
        match self.mode {
            EditorMode::Create => store.create(&self.configuration)?,
            EditorMode::Edit => store.save(&self.configuration)?,
        }
        self.dirty = false;
        self.notice = Some("Environment saved successfully.".to_owned());
        Ok(SaveOutcome::Saved)
    }

    pub fn handle_key(&mut self, key: KeyCode) -> EditorAction {
        if self.input.is_some() {
            return self.handle_input_key(key);
        }
        if self.palette_open {
            return self.handle_palette_key(key);
        }
        if self.delete_confirmation {
            return match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.delete_confirmation = false;
                    if let Some(index) = self.selected_action
                        && let Err(error) = self.remove_action(index)
                    {
                        self.validation_errors.push(error.to_string());
                    }
                    EditorAction::None
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.delete_confirmation = false;
                    EditorAction::None
                }
                _ => EditorAction::None,
            };
        }

        match key {
            KeyCode::Tab => {
                self.panel = self.panel.next();
                EditorAction::None
            }
            KeyCode::Up => {
                self.move_selection(-1);
                EditorAction::None
            }
            KeyCode::Down => {
                self.move_selection(1);
                EditorAction::None
            }
            KeyCode::Char('a') => {
                self.palette_open = true;
                EditorAction::PaletteOpened
            }
            KeyCode::Char('n') if self.panel == EditorPanel::Workspaces => {
                let result = self.next_workspace_id().and_then(|id| {
                    self.add_workspace(id.as_str(), WorkspaceTarget::NextEmpty)
                        .map(|_| ())
                });
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('t') if self.panel == EditorPanel::Workspaces => {
                let result = self.cycle_selected_workspace_target();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('i') if self.panel == EditorPanel::Workspaces => {
                let result = self.toggle_selected_workspace_tiling();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('e') => {
                self.begin_default_input();
                EditorAction::None
            }
            KeyCode::Char('w') if self.selected_action.is_some() => {
                self.begin_input(EditorField::WorkingDirectory);
                EditorAction::None
            }
            KeyCode::Char('o')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(
                        &action.kind,
                        ActionKind::OpenApplication | ActionKind::OpenProject
                    )
                }) =>
            {
                self.begin_input(EditorField::Application);
                EditorAction::None
            }
            KeyCode::Char('p')
                if self
                    .selected_action_spec()
                    .is_some_and(|action| matches!(&action.kind, ActionKind::OpenProject)) =>
            {
                self.begin_input(EditorField::ProjectPath);
                EditorAction::None
            }
            KeyCode::Char('c')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(
                        &action.kind,
                        ActionKind::RunCommand | ActionKind::StartService
                    )
                }) =>
            {
                self.begin_input(EditorField::CommandProgram);
                EditorAction::None
            }
            KeyCode::Char('v')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(
                        &action.kind,
                        ActionKind::StartContainer
                            | ActionKind::StartCompose
                            | ActionKind::StartAndroidEmulator
                    )
                }) =>
            {
                let field = match self.selected_action_spec().map(|action| &action.kind) {
                    Some(ActionKind::StartContainer) => EditorField::ContainerName,
                    Some(ActionKind::StartCompose) => EditorField::ComposeProjectName,
                    Some(ActionKind::StartAndroidEmulator) => EditorField::EmulatorAvd,
                    _ => EditorField::ActionDisplayLabel,
                };
                self.begin_input(field);
                EditorAction::None
            }
            KeyCode::Char('r')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(
                        &action.kind,
                        ActionKind::WaitForCondition | ActionKind::VerifyResource
                    )
                }) =>
            {
                self.begin_input(EditorField::ReadinessDelay);
                EditorAction::None
            }
            KeyCode::Char('x')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(&action.kind, ActionKind::CreateOrSelectWorkspace)
                }) =>
            {
                let workspace_id = self
                    .selected_workspace_spec()
                    .map(|workspace| workspace.id.clone());
                let action_id = self.selected_action_id();
                if let Ok(action_id) = action_id {
                    let result = self.set_action_workspace_parameter(&action_id, workspace_id);
                    self.record_error(result);
                }
                EditorAction::None
            }
            KeyCode::Char('m')
                if self.selected_action_spec().is_some_and(|action| {
                    matches!(
                        &action.kind,
                        ActionKind::RunCommand | ActionKind::StartService
                    )
                }) =>
            {
                let result = self.cycle_selected_action_execution_mode();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('g') if self.selected_action.is_some() => {
                let result = self.cycle_selected_action_workspace();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('+') if self.selected_action.is_some() => {
                let result = self.add_dependency_to_selected_action();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('-') if self.selected_action.is_some() => {
                let result = self.remove_last_dependency_from_selected_action();
                self.record_error(result);
                EditorAction::None
            }
            KeyCode::Char('d')
                if matches!(
                    self.panel,
                    EditorPanel::Actions | EditorPanel::Inspector | EditorPanel::Review
                ) && self.selected_action.is_some() =>
            {
                self.delete_confirmation = true;
                EditorAction::None
            }
            KeyCode::Char('s') => {
                self.panel = EditorPanel::Review;
                EditorAction::SaveRequested
            }
            KeyCode::Enter if self.panel == EditorPanel::Review => EditorAction::SaveRequested,
            KeyCode::Esc | KeyCode::Char('q') => EditorAction::CancelRequested,
            _ => EditorAction::None,
        }
    }

    fn handle_palette_key(&mut self, key: KeyCode) -> EditorAction {
        let palette_length = action_palette().len();
        match key {
            KeyCode::Up if palette_length > 0 => {
                self.selected_palette =
                    (self.selected_palette + palette_length - 1) % palette_length;
                EditorAction::None
            }
            KeyCode::Down if palette_length > 0 => {
                self.selected_palette = (self.selected_palette + 1) % palette_length;
                EditorAction::None
            }
            KeyCode::Enter => {
                if let Err(error) = self.add_action_from_palette(self.selected_palette) {
                    self.validation_errors.push(error.to_string());
                }
                EditorAction::None
            }
            KeyCode::Esc => {
                self.palette_open = false;
                EditorAction::None
            }
            _ => EditorAction::None,
        }
    }

    fn handle_input_key(&mut self, key: KeyCode) -> EditorAction {
        let Some(input) = &mut self.input else {
            return EditorAction::None;
        };
        match key {
            KeyCode::Char(character) => input.value.push(character),
            KeyCode::Backspace => {
                input.value.pop();
            }
            KeyCode::Enter => {
                let input = self.input.take();
                if let Some(input) = input {
                    self.commit_input(input);
                }
            }
            KeyCode::Esc => {
                self.input = None;
            }
            _ => {}
        }
        EditorAction::None
    }

    fn commit_input(&mut self, input: TextInput) {
        let value = input.value;
        let result = match input.field {
            EditorField::EnvironmentName => self
                .configuration
                .rename(value)
                .map_err(WorkstateError::from),
            EditorField::ActionDisplayLabel => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_display_label(&action_id, value)),
            EditorField::WorkingDirectory => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_working_directory(&action_id, Some(value))),
            EditorField::Application => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_application(&action_id, Some(value))),
            EditorField::ProjectPath => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_project_path(&action_id, Some(value))),
            EditorField::CommandProgram => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_command(&action_id, Some(CommandSpec::new(value)))),
            EditorField::ContainerName => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_container_name(&action_id, value)),
            EditorField::ComposeProjectName => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_compose_project_name(&action_id, value)),
            EditorField::EmulatorAvd => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_emulator_avd(&action_id, value)),
            EditorField::ReadinessDelay => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_readiness_delay(&action_id, value)),
        };
        if let Err(error) = result {
            self.validation_errors.push(error.to_string());
        } else {
            self.mark_dirty();
        }
    }

    pub fn begin_input(&mut self, field: EditorField) {
        let value = match field {
            EditorField::EnvironmentName => self.configuration.name.to_string(),
            EditorField::ActionDisplayLabel => self
                .selected_action_spec()
                .and_then(|action| action.display_label.clone())
                .unwrap_or_default(),
            EditorField::WorkingDirectory => self
                .selected_action_spec()
                .and_then(|action| action.working_directory.clone())
                .unwrap_or_default(),
            EditorField::Application => self
                .selected_action_spec()
                .and_then(|action| action.parameters.application.clone())
                .unwrap_or_default(),
            EditorField::ProjectPath => self
                .selected_action_spec()
                .and_then(|action| action.parameters.project_path.clone())
                .unwrap_or_default(),
            EditorField::CommandProgram => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .command
                        .as_ref()
                        .map(|command| command.program.clone())
                })
                .unwrap_or_default(),
            EditorField::ContainerName => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .container
                        .as_ref()
                        .map(|container| container.name.clone())
                })
                .unwrap_or_default(),
            EditorField::ComposeProjectName => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .compose
                        .as_ref()
                        .and_then(|compose| compose.project_name.clone())
                })
                .unwrap_or_default(),
            EditorField::EmulatorAvd => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .emulator
                        .as_ref()
                        .map(|emulator| emulator.avd.clone())
                })
                .unwrap_or_default(),
            EditorField::ReadinessDelay => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .readiness_checks
                        .first()
                        .and_then(|check| match check {
                            ReadinessCheck::Delay { milliseconds } => {
                                Some(milliseconds.to_string())
                            }
                            _ => None,
                        })
                })
                .unwrap_or_default(),
        };
        self.input = Some(TextInput { field, value });
    }

    fn begin_default_input(&mut self) {
        let field = match self.panel {
            EditorPanel::Actions | EditorPanel::Inspector => {
                if self.selected_action.is_some() {
                    EditorField::ActionDisplayLabel
                } else {
                    EditorField::EnvironmentName
                }
            }
            EditorPanel::Workspaces | EditorPanel::Review => EditorField::EnvironmentName,
        };
        self.begin_input(field);
    }

    fn move_selection(&mut self, offset: isize) {
        match self.panel {
            EditorPanel::Actions | EditorPanel::Inspector | EditorPanel::Review => {
                self.selected_action = move_index(
                    self.selected_action,
                    self.configuration.actions.len(),
                    offset,
                );
            }
            EditorPanel::Workspaces => {
                self.selected_workspace = move_index(
                    self.selected_workspace,
                    self.configuration.workspaces.len(),
                    offset,
                );
            }
        }
    }

    fn next_action_id(&self, kind: &ActionKind) -> Result<ActionId> {
        let raw_base = match kind {
            ActionKind::Custom { name } => name.clone(),
            _ => kind.key(),
        };
        let base = raw_base
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_owned();
        let base = if base.is_empty() {
            "action".to_owned()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut counter = 2usize;
        while self.has_action_name(&candidate) {
            candidate = format!("{base}-{counter}");
            counter = counter.checked_add(1).ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Ui,
                    "could not allocate a unique action identifier",
                )
            })?;
        }
        ActionId::new(candidate).map_err(WorkstateError::from)
    }

    fn action(&self, action_id: &ActionId) -> Result<&ActionSpec> {
        self.configuration
            .actions
            .iter()
            .find(|action| &action.id == action_id)
            .ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Ui,
                    format!("action '{action_id}' does not exist"),
                )
            })
    }

    fn action_mut(&mut self, action_id: &ActionId) -> Result<&mut ActionSpec> {
        self.configuration
            .actions
            .iter_mut()
            .find(|action| &action.id == action_id)
            .ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Ui,
                    format!("action '{action_id}' does not exist"),
                )
            })
    }

    fn has_action(&self, action_id: &ActionId) -> bool {
        self.configuration
            .actions
            .iter()
            .any(|action| &action.id == action_id)
    }

    fn has_action_name(&self, action_id: &str) -> bool {
        self.configuration
            .actions
            .iter()
            .any(|action| action.id.as_str() == action_id)
    }

    fn collect_dependency_paths(
        &self,
        action_id: &ActionId,
        path: String,
        visited: &mut BTreeSet<ActionId>,
        paths: &mut Vec<String>,
    ) {
        let Some(action) = self
            .configuration
            .actions
            .iter()
            .find(|action| &action.id == action_id)
        else {
            paths.push(format!("{path} -> missing action"));
            return;
        };
        for dependency in &action.depends_on {
            let next_path = format!("{path} -> {dependency}");
            if !visited.insert(dependency.clone()) {
                paths.push(format!("{next_path} (cycle)"));
                continue;
            }
            paths.push(next_path.clone());
            self.collect_dependency_paths(dependency, next_path, visited, paths);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.notice = None;
    }

    fn selected_action_id(&self) -> Result<ActionId> {
        self.selected_action_spec()
            .map(|action| action.id.clone())
            .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
    }

    fn next_workspace_id(&self) -> Result<WorkspaceId> {
        let mut candidate = "workspace".to_owned();
        let mut counter = 2usize;
        while self
            .configuration
            .workspaces
            .iter()
            .any(|workspace| workspace.id.as_str() == candidate)
        {
            candidate = format!("workspace-{counter}");
            counter = counter.checked_add(1).ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Ui,
                    "could not allocate a unique workspace identifier",
                )
            })?;
        }
        WorkspaceId::new(candidate).map_err(WorkstateError::from)
    }

    fn record_error(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.validation_errors.push(error.to_string());
        }
    }
}

fn move_index(current: Option<usize>, length: usize, offset: isize) -> Option<usize> {
    if length == 0 {
        return None;
    }
    let current = current.unwrap_or(0) as isize;
    Some((current + offset).rem_euclid(length as isize) as usize)
}

fn selection_after_removal(length: usize, removed: usize) -> Option<usize> {
    if length == 0 {
        None
    } else {
        Some(removed.min(length - 1))
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::{
        domain::{ActionKind, EnvironmentConfig, ExecutionMode, TilingPreference, WorkspaceTarget},
        infrastructure::filesystem::local::LocalFileSystem,
    };

    use super::{EditorMode, EditorPanel, EditorState, SaveOutcome, action_palette};

    #[test]
    fn palette_contains_the_capability_oriented_mvp_actions() {
        let palette = action_palette();
        assert_eq!(palette.len(), 12);
        assert!(palette.iter().any(|entry| entry.label == "Open project"));
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Start Docker Compose stack")
        );
        assert!(palette.iter().any(|entry| entry.label == "Custom action"));
    }

    #[test]
    fn editor_adds_actions_and_workspaces_without_a_second_configuration_model() {
        let Some(configuration) = EnvironmentConfig::new("Personal Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let action_id = editor.add_action_from_palette(1);
        assert!(action_id.is_ok());
        let Some(action_id) = action_id.ok() else {
            return;
        };
        assert_eq!(editor.configuration.actions.len(), 1);
        assert_eq!(
            editor.configuration.actions[0].kind,
            ActionKind::OpenProject
        );
        assert!(
            editor
                .set_action_working_directory(&action_id, Some("~/Projects/blog".to_owned()))
                .is_ok()
        );
        assert!(
            editor
                .set_action_execution_mode(&action_id, Some(ExecutionMode::RunOnce))
                .is_err()
        );

        let workspace = editor.add_workspace(
            "editor",
            WorkspaceTarget::Create {
                name: "Editor".to_owned(),
            },
        );
        assert!(workspace.is_ok());
        let Some(workspace) = workspace.ok() else {
            return;
        };
        assert!(
            editor
                .set_workspace_tiling(&workspace, TilingPreference::Enabled)
                .is_ok()
        );
        assert!(
            editor
                .set_action_desktop_workspace(&action_id, Some(workspace))
                .is_ok()
        );
        assert!(editor.validate().is_err());
    }

    #[test]
    fn dependency_editor_rejects_self_and_missing_references_and_can_remove_edges() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let first = editor.add_action_from_palette(11);
        assert!(first.is_ok());
        let Some(first) = first.ok() else {
            return;
        };
        let second = editor.add_action_from_palette(11);
        assert!(second.is_ok());
        let Some(second) = second.ok() else {
            return;
        };
        assert!(editor.add_dependency(&first, &first).is_err());
        let Some(missing) = crate::domain::ActionId::new("missing").ok() else {
            return;
        };
        assert!(editor.add_dependency(&first, &missing).is_err());
        assert!(editor.add_dependency(&first, &second).is_ok());
        assert_eq!(
            editor.dependency_path(&first),
            vec!["custom-action -> custom-action-2".to_owned()]
        );
        assert!(editor.remove_dependency(&first, &second).is_ok());
        assert!(editor.dependency_path(&first).is_empty());
    }

    #[test]
    fn save_requires_validation_and_confirmation_before_using_the_store() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let Ok(paths) =
            crate::infrastructure::persistence::WorkstatePaths::new(directory.path().to_path_buf())
        else {
            return;
        };
        let file_system = LocalFileSystem;
        let result = editor.save(
            &crate::infrastructure::persistence::TomlConfigStore::new(
                std::sync::Arc::new(file_system),
                paths,
            ),
            false,
        );
        assert_eq!(result.ok(), Some(SaveOutcome::ConfirmationRequired));
        assert_eq!(editor.panel, EditorPanel::Actions);
    }

    #[test]
    fn keyboard_editor_exposes_paths_modes_workspaces_and_dependencies() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let first = editor.add_action_from_palette(2);
        assert!(first.is_ok());
        let second = editor.add_action_from_palette(2);
        assert!(second.is_ok());

        assert_eq!(
            editor.handle_key(KeyCode::Char('c')),
            super::EditorAction::None
        );
        assert_eq!(
            editor.input.as_ref().map(|input| input.field),
            Some(super::EditorField::CommandProgram)
        );
        editor.handle_key(KeyCode::Char('b'));
        editor.handle_key(KeyCode::Enter);
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.command.as_ref())
                .map(|command| command.program.as_str()),
            Some("b")
        );
        assert_eq!(
            editor.handle_key(KeyCode::Char('m')),
            super::EditorAction::None
        );
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.execution_mode),
            Some(ExecutionMode::RunOnce)
        );
        assert_eq!(
            editor.handle_key(KeyCode::Char('+')),
            super::EditorAction::None
        );
        assert_eq!(
            editor
                .selected_action_spec()
                .map(|action| action.depends_on.len()),
            Some(1)
        );

        editor.handle_key(KeyCode::Tab);
        editor.handle_key(KeyCode::Tab);
        editor.handle_key(KeyCode::Tab);
        assert_eq!(editor.panel, EditorPanel::Workspaces);
        editor.handle_key(KeyCode::Char('n'));
        assert_eq!(editor.configuration.workspaces.len(), 1);
        editor.handle_key(KeyCode::Char('t'));
        assert!(matches!(
            editor
                .selected_workspace_spec()
                .map(|workspace| &workspace.target),
            Some(WorkspaceTarget::Create { .. })
        ));
        editor.handle_key(KeyCode::Char('i'));
        assert_eq!(
            editor
                .selected_workspace_spec()
                .map(|workspace| workspace.tiling),
            Some(TilingPreference::Enabled)
        );
    }
}
