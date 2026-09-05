use std::{collections::BTreeSet, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    application::ports::{ConfigStore, DesktopWorkspaceSnapshot, FileSystem},
    domain::{
        ActionId, ActionKind, ActionSpec, CommandSpec, ComposeSpec, ContainerSpec, DomainError,
        EmulatorSpec, EnvironmentConfig, ExecutionMode, ReadinessCheck, TilingPreference,
        WorkspaceId, WorkspaceSpec, WorkspaceTarget,
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
    Inspector,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorField {
    ActionLabel,
    Application,
    ProjectPath,
    WorkingDirectory,
    Command,
    ExecutionMode,
    DesktopWorkspace,
    Tiling,
    ContainerName,
    ComposeProjectName,
    EmulatorAvd,
    ReadinessDelay,
    Dependencies,
}

impl InspectorField {
    pub fn label(self) -> &'static str {
        match self {
            Self::ActionLabel => "Action name",
            Self::Application => "Application",
            Self::ProjectPath => "Project path",
            Self::WorkingDirectory => "Working directory",
            Self::Command => "Command",
            Self::ExecutionMode => "Execution mode",
            Self::DesktopWorkspace => "Desktop workspace",
            Self::Tiling => "Tiling",
            Self::ContainerName => "Container",
            Self::ComposeProjectName => "Compose project",
            Self::EmulatorAvd => "Android virtual device",
            Self::ReadinessDelay => "Readiness delay",
            Self::Dependencies => "Depends on",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    pub field: EditorField,
    pub value: String,
    pub replace_on_next_char: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorChoice {
    pub label: String,
    pub detail: Option<String>,
    pub value: InspectorChoiceValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorChoiceValue {
    DesktopWorkspace(Option<WorkspaceId>),
    LinkLiveWorkspace,
    AddNextEmptyWorkspace,
    ExecutionMode(Option<ExecutionMode>),
    Tiling {
        workspace_id: WorkspaceId,
        preference: TilingPreference,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorPicker {
    Choices {
        field: InspectorField,
        title: String,
        options: Vec<InspectorChoice>,
        selected: usize,
    },
    Dependencies {
        action_id: ActionId,
        options: Vec<ActionId>,
        selected: usize,
        checked: BTreeSet<ActionId>,
    },
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
            label: "Open Project with Zed",
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
enum ValidationTarget {
    Environment,
    Action {
        action_id: ActionId,
        field: Option<InspectorField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    pub configuration: EnvironmentConfig,
    pub live_workspaces: Vec<DesktopWorkspaceSnapshot>,
    pub selected_live_workspace: Option<usize>,
    pub workspace_picker_open: bool,
    pub workspace_observation_error: Option<String>,
    pub mode: EditorMode,
    pub panel: EditorPanel,
    pub selected_action: Option<usize>,
    pub selected_inspector: Option<usize>,
    pub selected_palette: usize,
    pub palette_open: bool,
    pub delete_confirmation: bool,
    pub input: Option<TextInput>,
    pub inspector_picker: Option<InspectorPicker>,
    pub workspace_picker_target: Option<InspectorField>,
    pub validation_errors: Vec<String>,
    pub notice: Option<String>,
    pub dirty: bool,
    validation_targets: Vec<ValidationTarget>,
    validation_feedback_active: bool,
}

impl EditorState {
    pub fn new(mut configuration: EnvironmentConfig, mode: EditorMode) -> Self {
        for action in &mut configuration.actions {
            if matches!(&action.kind, ActionKind::OpenProject) {
                action.parameters.application = Some("zed".to_owned());
            }
        }
        let selected_action = (!configuration.actions.is_empty()).then_some(0);
        Self {
            configuration,
            live_workspaces: Vec::new(),
            selected_live_workspace: None,
            workspace_picker_open: false,
            workspace_observation_error: None,
            mode,
            panel: EditorPanel::Actions,
            selected_action,
            selected_inspector: selected_action.map(|_| 0),
            selected_palette: 0,
            palette_open: false,
            delete_confirmation: false,
            input: None,
            inspector_picker: None,
            workspace_picker_target: None,
            validation_errors: Vec::new(),
            notice: None,
            dirty: false,
            validation_targets: Vec::new(),
            validation_feedback_active: false,
        }
    }

    pub fn with_live_workspaces(mut self, mut workspaces: Vec<DesktopWorkspaceSnapshot>) -> Self {
        workspaces.sort_by(|left, right| {
            left.position
                .unwrap_or(u32::MAX)
                .cmp(&right.position.unwrap_or(u32::MAX))
                .then_with(|| left.identity.cmp(&right.identity))
        });
        self.selected_live_workspace = (!workspaces.is_empty()).then_some(0);
        self.live_workspaces = workspaces;
        self
    }

    pub fn with_workspace_observation_error(mut self, error: impl Into<String>) -> Self {
        self.workspace_observation_error = Some(error.into());
        self
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

    pub fn inspector_fields(&self) -> Vec<InspectorField> {
        let Some(action) = self.selected_action_spec() else {
            return Vec::new();
        };

        let mut fields = vec![InspectorField::ActionLabel];
        match &action.kind {
            ActionKind::OpenApplication => fields.extend([
                InspectorField::Application,
                InspectorField::WorkingDirectory,
                InspectorField::DesktopWorkspace,
            ]),
            ActionKind::OpenProject => fields.extend([
                InspectorField::ProjectPath,
                InspectorField::DesktopWorkspace,
            ]),
            ActionKind::RunCommand | ActionKind::StartService => fields.extend([
                InspectorField::Command,
                InspectorField::WorkingDirectory,
                InspectorField::ExecutionMode,
            ]),
            ActionKind::ConfigureTiling => {
                fields.extend([InspectorField::DesktopWorkspace, InspectorField::Tiling]);
            }
            ActionKind::StartContainer => {
                fields.extend([
                    InspectorField::ContainerName,
                    InspectorField::WorkingDirectory,
                ]);
            }
            ActionKind::StartCompose => fields.extend([
                InspectorField::ComposeProjectName,
                InspectorField::WorkingDirectory,
            ]),
            ActionKind::StartAndroidEmulator => {
                fields.extend([
                    InspectorField::EmulatorAvd,
                    InspectorField::DesktopWorkspace,
                ]);
            }
            ActionKind::WaitForCondition | ActionKind::VerifyResource => {
                fields.push(InspectorField::ReadinessDelay);
            }
            ActionKind::Custom { .. } => {}
        }
        fields.push(InspectorField::Dependencies);
        fields
    }

    pub fn selected_inspector_field(&self) -> Option<InspectorField> {
        self.selected_inspector
            .and_then(|index| self.inspector_fields().get(index).copied())
    }

    pub fn inspector_field_value(&self, field: InspectorField) -> String {
        let Some(action) = self.selected_action_spec() else {
            return "not available".to_owned();
        };
        match field {
            InspectorField::ActionLabel => action_label(action),
            InspectorField::Application => action
                .parameters
                .application
                .clone()
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ProjectPath => action
                .parameters
                .project_path
                .clone()
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::WorkingDirectory => action
                .working_directory
                .clone()
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::Command => action
                .parameters
                .command
                .as_ref()
                .map(command_label)
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ExecutionMode => action
                .execution_mode
                .map(execution_mode_label)
                .unwrap_or("not set")
                .to_owned(),
            InspectorField::DesktopWorkspace => action
                .desktop_workspace
                .as_ref()
                .map(|id| self.workspace_label(id))
                .unwrap_or_else(|| "Current workspace".to_owned()),
            InspectorField::Tiling => action
                .desktop_workspace
                .as_ref()
                .and_then(|id| {
                    self.configuration
                        .workspaces
                        .iter()
                        .find(|workspace| &workspace.id == id)
                })
                .map(|workspace| tiling_label(workspace.tiling).to_owned())
                .unwrap_or_else(|| "Select a workspace first".to_owned()),
            InspectorField::ContainerName => action
                .parameters
                .container
                .as_ref()
                .map(|container| container.name.clone())
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ComposeProjectName => action
                .parameters
                .compose
                .as_ref()
                .and_then(|compose| compose.project_name.clone())
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::EmulatorAvd => action
                .parameters
                .emulator
                .as_ref()
                .map(|emulator| emulator.avd.clone())
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ReadinessDelay => action
                .readiness_checks
                .first()
                .map(readiness_label)
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::Dependencies => {
                if action.depends_on.is_empty() {
                    "none".to_owned()
                } else {
                    action
                        .depends_on
                        .iter()
                        .map(|dependency| self.action_label_for_id(dependency))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
        }
    }

    pub fn action_label_for_id(&self, action_id: &ActionId) -> String {
        self.configuration
            .actions
            .iter()
            .find(|action| &action.id == action_id)
            .map(action_label)
            .unwrap_or_else(|| action_id.to_string())
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
        if matches!(&action.kind, ActionKind::OpenProject) {
            action.parameters.application = Some("zed".to_owned());
        }
        self.configuration
            .add_action(action)
            .map_err(WorkstateError::from)?;
        self.selected_action = self.configuration.actions.len().checked_sub(1);
        self.selected_inspector = Some(0);
        self.panel = EditorPanel::Actions;
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
        self.selected_inspector = self.selected_action.is_some().then_some(0);
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
        if !matches!(&action.kind, ActionKind::OpenApplication) {
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
                self.clear_validation_errors();
                Ok(())
            }
            Err(error) => {
                self.validation_feedback_active = true;
                self.set_validation_error(&error);
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
                self.record_notice(error.to_string());
                Err(error)
            }
        }
    }

    pub fn review(&mut self) -> EditorReview {
        let valid = self.configuration.validate().is_ok();
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
        self.handle_key_event(KeyEvent::new(key, KeyModifiers::NONE))
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> EditorAction {
        if self.workspace_picker_open {
            return self.handle_workspace_picker_key(key.code);
        }
        if self.inspector_picker.is_some() {
            return self.handle_inspector_picker_key(key.code);
        }
        if self.input.is_some() {
            return self.handle_input_key(key.code);
        }
        if self.palette_open {
            return self.handle_palette_key(key.code);
        }
        if self.delete_confirmation {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.delete_confirmation = false;
                    if let Some(index) = self.selected_action
                        && let Err(error) = self.remove_action(index)
                    {
                        self.record_notice(error.to_string());
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

        if is_save_shortcut(key) {
            return EditorAction::SaveRequested;
        }

        match key.code {
            KeyCode::Tab => {
                self.panel = match self.panel {
                    EditorPanel::Actions => EditorPanel::Inspector,
                    EditorPanel::Inspector => EditorPanel::Actions,
                };
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
            KeyCode::Char('a') if self.panel == EditorPanel::Actions => {
                self.selected_palette = 0;
                self.palette_open = true;
                EditorAction::PaletteOpened
            }
            KeyCode::Enter | KeyCode::Right if self.panel == EditorPanel::Actions => {
                if self.selected_action.is_some() {
                    self.panel = EditorPanel::Inspector;
                    self.normalize_inspector_selection();
                } else {
                    self.notice = Some("Add an action before opening the inspector.".to_owned());
                }
                EditorAction::None
            }
            KeyCode::Enter => match self.panel {
                EditorPanel::Actions => {
                    if self.selected_action.is_some() {
                        self.panel = EditorPanel::Inspector;
                        self.normalize_inspector_selection();
                    } else {
                        self.notice =
                            Some("Add an action before opening the inspector.".to_owned());
                    }
                    EditorAction::None
                }
                EditorPanel::Inspector => {
                    self.activate_selected_inspector_field();
                    EditorAction::None
                }
            },
            KeyCode::Char('d')
                if self.panel == EditorPanel::Actions && self.selected_action.is_some() =>
            {
                self.delete_confirmation = true;
                EditorAction::None
            }
            KeyCode::Esc | KeyCode::Left if self.panel == EditorPanel::Inspector => {
                self.panel = EditorPanel::Actions;
                EditorAction::None
            }
            KeyCode::Esc => match self.panel {
                EditorPanel::Actions => EditorAction::CancelRequested,
                EditorPanel::Inspector => {
                    self.panel = EditorPanel::Actions;
                    EditorAction::None
                }
            },
            KeyCode::Char('q') => EditorAction::CancelRequested,
            _ => EditorAction::None,
        }
    }

    fn activate_selected_inspector_field(&mut self) {
        let Some(field) = self.selected_inspector_field() else {
            self.notice = Some("No editable fields are available for this action.".to_owned());
            return;
        };
        match field {
            InspectorField::ActionLabel => self.begin_input(EditorField::ActionDisplayLabel),
            InspectorField::Application => self.begin_input(EditorField::Application),
            InspectorField::ProjectPath => self.begin_input(EditorField::ProjectPath),
            InspectorField::WorkingDirectory => self.begin_input(EditorField::WorkingDirectory),
            InspectorField::Command => self.begin_input(EditorField::CommandProgram),
            InspectorField::ContainerName => self.begin_input(EditorField::ContainerName),
            InspectorField::ComposeProjectName => self.begin_input(EditorField::ComposeProjectName),
            InspectorField::EmulatorAvd => self.begin_input(EditorField::EmulatorAvd),
            InspectorField::ReadinessDelay => self.begin_input(EditorField::ReadinessDelay),
            InspectorField::DesktopWorkspace => self.open_workspace_choice_picker(field),
            InspectorField::ExecutionMode => self.open_execution_mode_picker(),
            InspectorField::Tiling => self.open_tiling_picker(),
            InspectorField::Dependencies => self.open_dependency_picker(),
        }
    }

    fn open_workspace_choice_picker(&mut self, field: InspectorField) {
        if field != InspectorField::DesktopWorkspace {
            return;
        }
        let current = self.selected_action_spec().and_then(|action| match field {
            InspectorField::DesktopWorkspace => action.desktop_workspace.clone(),
            _ => None,
        });
        let mut options = Vec::with_capacity(self.configuration.workspaces.len() + 3);
        let current_label = "Current workspace";
        let current_value = InspectorChoiceValue::DesktopWorkspace(None);
        options.push(InspectorChoice {
            label: current_label.to_owned(),
            detail: None,
            value: current_value,
        });
        for workspace in &self.configuration.workspaces {
            let value = InspectorChoiceValue::DesktopWorkspace(Some(workspace.id.clone()));
            options.push(InspectorChoice {
                label: self.workspace_label(&workspace.id),
                detail: Some(workspace_target_label(workspace)),
                value,
            });
        }
        options.push(InspectorChoice {
            label: "Link live COSMIC workspace...".to_owned(),
            detail: Some("Choose an existing workspace from the current session".to_owned()),
            value: InspectorChoiceValue::LinkLiveWorkspace,
        });
        options.push(InspectorChoice {
            label: "Add next empty workspace".to_owned(),
            detail: Some("Create a saved target resolved during execution".to_owned()),
            value: InspectorChoiceValue::AddNextEmptyWorkspace,
        });
        let selected = options
            .iter()
            .position(|option| {
                matches!(&option.value, InspectorChoiceValue::DesktopWorkspace(value) if value == &current)
            })
            .unwrap_or(0);
        self.inspector_picker = Some(InspectorPicker::Choices {
            field,
            title: field.label().to_owned(),
            options,
            selected,
        });
    }

    fn open_execution_mode_picker(&mut self) {
        let current = self
            .selected_action_spec()
            .and_then(|action| action.execution_mode);
        let options = vec![
            InspectorChoice {
                label: "Run once".to_owned(),
                detail: Some("Complete during environment setup".to_owned()),
                value: InspectorChoiceValue::ExecutionMode(Some(ExecutionMode::RunOnce)),
            },
            InspectorChoice {
                label: "Background".to_owned(),
                detail: Some("Keep running after Workstate exits".to_owned()),
                value: InspectorChoiceValue::ExecutionMode(Some(ExecutionMode::Background)),
            },
            InspectorChoice {
                label: "Not set".to_owned(),
                detail: Some("Leave the action unconfigured".to_owned()),
                value: InspectorChoiceValue::ExecutionMode(None),
            },
        ];
        let selected = options
            .iter()
            .position(|option| {
                matches!(&option.value, InspectorChoiceValue::ExecutionMode(value) if value == &current)
            })
            .unwrap_or(0);
        self.inspector_picker = Some(InspectorPicker::Choices {
            field: InspectorField::ExecutionMode,
            title: "Execution mode".to_owned(),
            options,
            selected,
        });
    }

    fn open_tiling_picker(&mut self) {
        let Some(workspace_id) = self
            .selected_action_spec()
            .and_then(|action| action.desktop_workspace.clone())
        else {
            self.notice = Some("Select a desktop workspace before editing tiling.".to_owned());
            return;
        };
        let current = self
            .configuration
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .map(|workspace| workspace.tiling)
            .unwrap_or(TilingPreference::Unchanged);
        let options = [
            TilingPreference::Unchanged,
            TilingPreference::Enabled,
            TilingPreference::Disabled,
        ]
        .into_iter()
        .map(|preference| InspectorChoice {
            label: tiling_label(preference).to_owned(),
            detail: Some("Workspace setting".to_owned()),
            value: InspectorChoiceValue::Tiling {
                workspace_id: workspace_id.clone(),
                preference,
            },
        })
        .collect::<Vec<_>>();
        let selected = options
            .iter()
            .position(|option| {
                matches!(&option.value, InspectorChoiceValue::Tiling { preference, .. } if preference == &current)
            })
            .unwrap_or(0);
        self.inspector_picker = Some(InspectorPicker::Choices {
            field: InspectorField::Tiling,
            title: "Tiling preference".to_owned(),
            options,
            selected,
        });
    }

    fn open_dependency_picker(&mut self) {
        let Some(action) = self.selected_action_spec() else {
            self.notice = Some("Select an action before editing dependencies.".to_owned());
            return;
        };
        let action_id = action.id.clone();
        let checked = action.depends_on.iter().cloned().collect::<BTreeSet<_>>();
        let options = self
            .configuration
            .actions
            .iter()
            .filter(|candidate| candidate.id != action_id)
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        self.inspector_picker = Some(InspectorPicker::Dependencies {
            action_id,
            options,
            selected: 0,
            checked,
        });
    }

    fn handle_inspector_picker_key(&mut self, key: KeyCode) -> EditorAction {
        match key {
            KeyCode::Up => self.move_picker_selection(-1),
            KeyCode::Down => self.move_picker_selection(1),
            KeyCode::Char(' ') => self.toggle_picker_dependency(),
            KeyCode::Enter => self.confirm_inspector_picker(),
            KeyCode::Esc => self.inspector_picker = None,
            _ => {}
        }
        EditorAction::None
    }

    fn move_picker_selection(&mut self, offset: isize) {
        let Some(picker) = self.inspector_picker.as_mut() else {
            return;
        };
        let (selected, length) = match picker {
            InspectorPicker::Choices {
                selected, options, ..
            } => (selected, options.len()),
            InspectorPicker::Dependencies {
                selected, options, ..
            } => (selected, options.len()),
        };
        if length > 0 {
            *selected = ((*selected as isize + offset).rem_euclid(length as isize)) as usize;
        }
    }

    fn toggle_picker_dependency(&mut self) {
        let Some(InspectorPicker::Dependencies {
            options,
            selected,
            checked,
            ..
        }) = self.inspector_picker.as_mut()
        else {
            return;
        };
        let Some(action_id) = options.get(*selected).cloned() else {
            return;
        };
        if !checked.insert(action_id.clone()) {
            checked.remove(&action_id);
        }
    }

    fn confirm_inspector_picker(&mut self) {
        let Some(picker) = self.inspector_picker.take() else {
            return;
        };
        match picker {
            InspectorPicker::Choices {
                field,
                options,
                selected,
                ..
            } => {
                let Some(choice) = options.get(selected).cloned() else {
                    return;
                };
                match choice.value {
                    InspectorChoiceValue::DesktopWorkspace(workspace_id) => {
                        let result = self.set_selected_action_workspace_field(field, workspace_id);
                        self.record_error(result);
                    }
                    InspectorChoiceValue::LinkLiveWorkspace => {
                        self.workspace_picker_target = Some(field);
                        self.open_workspace_picker();
                    }
                    InspectorChoiceValue::AddNextEmptyWorkspace => {
                        let result = self.next_workspace_id().and_then(|id| {
                            self.add_workspace(id.as_str(), WorkspaceTarget::NextEmpty)
                        });
                        match result {
                            Ok(workspace_id) => {
                                let result = self
                                    .set_selected_action_workspace_field(field, Some(workspace_id));
                                self.record_error(result);
                            }
                            Err(error) => self.record_notice(error.to_string()),
                        }
                    }
                    InspectorChoiceValue::ExecutionMode(mode) => {
                        let result = self
                            .selected_action_id()
                            .and_then(|action_id| self.set_action_execution_mode(&action_id, mode));
                        self.record_error(result);
                    }
                    InspectorChoiceValue::Tiling {
                        workspace_id,
                        preference,
                    } => {
                        let result = self.set_workspace_tiling(&workspace_id, preference);
                        self.record_error(result);
                    }
                }
            }
            InspectorPicker::Dependencies {
                action_id,
                options,
                checked,
                ..
            } => {
                let dependencies = options
                    .into_iter()
                    .filter(|action_id| checked.contains(action_id))
                    .collect::<Vec<_>>();
                let result = self.action_mut(&action_id).map(|action| {
                    action.depends_on = dependencies;
                });
                if result.is_ok() {
                    self.mark_dirty();
                }
                self.record_error(result.map(|_| ()));
            }
        }
    }

    fn set_selected_action_workspace_field(
        &mut self,
        field: InspectorField,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<()> {
        let action_id = self.selected_action_id()?;
        match field {
            InspectorField::DesktopWorkspace => {
                self.set_action_desktop_workspace(&action_id, workspace_id)
            }
            _ => Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!(
                    "workspace selection is not available for field '{}'.",
                    field.label()
                ),
            )),
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
                    self.record_notice(error.to_string());
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
            KeyCode::Char(character) => {
                if input.replace_on_next_char {
                    input.value.clear();
                    input.replace_on_next_char = false;
                }
                input.value.push(character);
            }
            KeyCode::Backspace => {
                input.replace_on_next_char = false;
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
            self.record_notice(error.to_string());
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
        self.input = Some(TextInput {
            field,
            value,
            replace_on_next_char: true,
        });
    }

    fn move_selection(&mut self, offset: isize) {
        match self.panel {
            EditorPanel::Actions => {
                self.selected_action = move_index(
                    self.selected_action,
                    self.configuration.actions.len(),
                    offset,
                );
                self.selected_inspector = self.selected_action.is_some().then_some(0);
            }
            EditorPanel::Inspector => {
                self.selected_inspector = move_index(
                    self.selected_inspector,
                    self.inspector_fields().len(),
                    offset,
                );
            }
        }
    }

    fn normalize_inspector_selection(&mut self) {
        self.selected_inspector =
            move_index(self.selected_inspector, self.inspector_fields().len(), 0);
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
        self.refresh_validation_errors();
    }

    fn clear_validation_errors(&mut self) {
        self.validation_errors.clear();
        self.validation_targets.clear();
    }

    fn set_validation_error(&mut self, error: &DomainError) {
        self.validation_errors = vec![self.validation_message(error)];
        self.validation_targets = vec![self.validation_target(error)];
    }

    fn refresh_validation_errors(&mut self) {
        if !self.validation_feedback_active || self.validation_targets.is_empty() {
            return;
        }

        let Some(error) = self.configuration.validate().err() else {
            self.clear_validation_errors();
            return;
        };
        let target = self.validation_target(&error);
        if self
            .validation_targets
            .iter()
            .any(|current| current == &target)
        {
            self.validation_errors = vec![self.validation_message(&error)];
            self.validation_targets = vec![target];
        } else {
            self.clear_validation_errors();
        }
    }

    fn validation_target(&self, error: &DomainError) -> ValidationTarget {
        let Some(action_id) = validation_action_id(error) else {
            return ValidationTarget::Environment;
        };
        let Some(action) = self
            .configuration
            .actions
            .iter()
            .find(|action| action.id.as_str() == action_id)
        else {
            return ValidationTarget::Environment;
        };
        ValidationTarget::Action {
            action_id: action.id.clone(),
            field: validation_field(error),
        }
    }

    fn validation_message(&self, error: &DomainError) -> String {
        if let DomainError::DependencyCycle { actions } = error {
            let labels = actions
                .split(", ")
                .map(|action_id| self.action_name(action_id))
                .collect::<Vec<_>>()
                .join(", ");
            return format!("action dependency cycle detected: {labels}");
        }

        let message = error.to_string();
        let Some(action_id) = validation_action_id(error) else {
            return message;
        };
        let Some(action) = self
            .configuration
            .actions
            .iter()
            .find(|action| action.id.as_str() == action_id)
        else {
            return message;
        };
        message.replace(
            &format!("action {action_id}"),
            &format!("action '{}'", action_label(action)),
        )
    }

    fn record_notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    fn action_name(&self, action_id: &str) -> String {
        self.configuration
            .actions
            .iter()
            .find(|action| action.id.as_str() == action_id)
            .map(action_label)
            .unwrap_or_else(|| action_id.to_owned())
    }

    fn selected_action_id(&self) -> Result<ActionId> {
        self.selected_action_spec()
            .map(|action| action.id.clone())
            .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
    }

    fn next_workspace_id(&self) -> Result<WorkspaceId> {
        self.next_workspace_id_from("workspace")
    }

    fn next_workspace_id_from(&self, base: &str) -> Result<WorkspaceId> {
        let base = if base.is_empty() { "workspace" } else { base };
        let mut candidate = base.to_owned();
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

    fn open_workspace_picker(&mut self) {
        if self.live_workspaces.is_empty() {
            self.workspace_picker_target = None;
            self.record_notice(
                "No live desktop workspaces are available to link from the current session."
                    .to_owned(),
            );
            return;
        }
        self.workspace_picker_open = true;
        if self.selected_live_workspace.is_none() {
            self.selected_live_workspace = Some(0);
        }
    }

    fn handle_workspace_picker_key(&mut self, key: KeyCode) -> EditorAction {
        match key {
            KeyCode::Up => {
                self.selected_live_workspace =
                    move_index(self.selected_live_workspace, self.live_workspaces.len(), -1);
            }
            KeyCode::Down => {
                self.selected_live_workspace =
                    move_index(self.selected_live_workspace, self.live_workspaces.len(), 1);
            }
            KeyCode::Enter => {
                let target = self.workspace_picker_target;
                let result = self
                    .link_selected_live_workspace()
                    .and_then(|workspace_id| {
                        if let Some(field) = target {
                            self.set_selected_action_workspace_field(field, Some(workspace_id))
                        } else {
                            Ok(())
                        }
                    });
                match result {
                    Ok(()) => {
                        self.workspace_picker_target = None;
                        self.workspace_picker_open = false;
                    }
                    Err(error) => {
                        self.workspace_picker_target = target;
                        self.record_notice(error.to_string());
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.workspace_picker_open = false,
            _ => {}
        }
        EditorAction::None
    }

    fn link_selected_live_workspace(&mut self) -> Result<WorkspaceId> {
        let index = self.selected_live_workspace.ok_or_else(|| {
            WorkstateError::new(ErrorCategory::Ui, "no live desktop workspace is selected")
        })?;
        let workspace = self.live_workspaces.get(index).cloned().ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Ui,
                "the selected live workspace does not exist",
            )
        })?;
        if self.configuration.workspaces.iter().any(|configured| {
            matches!(
                &configured.target,
                WorkspaceTarget::Existing {
                    reference: crate::domain::WorkspaceReference::Identifier(identity)
                } if identity == &workspace.identity
            )
        }) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!(
                    "desktop workspace '{}' is already linked to this environment",
                    workspace.identity
                ),
            ));
        }
        let id_base = workspace
            .name
            .as_deref()
            .unwrap_or(workspace.identity.as_str())
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let id_base = id_base.trim_matches('-');
        let workspace_id = self.next_workspace_id_from(id_base)?;
        let mut specification = WorkspaceSpec::new(
            workspace_id.as_str(),
            WorkspaceTarget::Existing {
                reference: crate::domain::WorkspaceReference::Identifier(
                    workspace.identity.clone(),
                ),
            },
        )
        .map_err(WorkstateError::from)?;
        specification.name = workspace.name;
        self.configuration
            .add_workspace(specification)
            .map_err(WorkstateError::from)?;
        self.mark_dirty();
        Ok(workspace_id)
    }

    fn record_error(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.record_notice(error.to_string());
        }
    }

    fn workspace_label(&self, workspace_id: &WorkspaceId) -> String {
        self.configuration
            .workspaces
            .iter()
            .find(|workspace| &workspace.id == workspace_id)
            .map(|workspace| {
                workspace
                    .name
                    .as_ref()
                    .map(|name| format!("{name} ({})", workspace.id))
                    .unwrap_or_else(|| workspace.id.to_string())
            })
            .unwrap_or_else(|| workspace_id.to_string())
    }
}

fn action_label(action: &ActionSpec) -> String {
    if let Some(label) = &action.display_label
        && !label.is_empty()
    {
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

fn validation_action_id(error: &DomainError) -> Option<&str> {
    match error {
        DomainError::MissingDependency { action_id, .. }
        | DomainError::SelfDependency { action_id }
        | DomainError::DuplicateDependency { action_id, .. }
        | DomainError::MissingWorkspaceReference { action_id, .. }
        | DomainError::MissingActionParameter { action_id, .. }
        | DomainError::InvalidActionParameter { action_id, .. }
        | DomainError::InvalidActionTimeout { action_id, .. }
        | DomainError::InvalidRetryPolicy { action_id }
        | DomainError::InvalidExecutionMode { action_id, .. }
        | DomainError::InvalidCommand { action_id, .. }
        | DomainError::InvalidReadinessCheck { action_id, .. } => Some(action_id),
        _ => None,
    }
}

fn validation_field(error: &DomainError) -> Option<InspectorField> {
    match error {
        DomainError::MissingDependency { .. }
        | DomainError::SelfDependency { .. }
        | DomainError::DuplicateDependency { .. } => Some(InspectorField::Dependencies),
        DomainError::MissingWorkspaceReference { .. } => Some(InspectorField::DesktopWorkspace),
        DomainError::MissingActionParameter { parameter, .. }
        | DomainError::InvalidActionParameter { parameter, .. } => match parameter.as_str() {
            "display_label" => Some(InspectorField::ActionLabel),
            "application" => Some(InspectorField::Application),
            "project_path" => Some(InspectorField::ProjectPath),
            "working_directory" => Some(InspectorField::WorkingDirectory),
            "command" => Some(InspectorField::Command),
            "desktop_workspace" => Some(InspectorField::DesktopWorkspace),
            "container" | "container.name" => Some(InspectorField::ContainerName),
            "compose" | "compose.project_name" => Some(InspectorField::ComposeProjectName),
            "emulator" | "emulator.avd" => Some(InspectorField::EmulatorAvd),
            "readiness_checks" => Some(InspectorField::ReadinessDelay),
            _ => None,
        },
        DomainError::InvalidActionTimeout { .. } | DomainError::InvalidRetryPolicy { .. } => None,
        DomainError::InvalidExecutionMode { .. } => Some(InspectorField::ExecutionMode),
        DomainError::InvalidCommand { .. } => Some(InspectorField::Command),
        DomainError::InvalidReadinessCheck { .. } => Some(InspectorField::ReadinessDelay),
        _ => None,
    }
}

fn command_label(command: &CommandSpec) -> String {
    if command.arguments.is_empty() {
        command.program.clone()
    } else {
        format!("{} {}", command.program, command.arguments.join(" "))
    }
}

fn readiness_label(check: &ReadinessCheck) -> String {
    match check {
        ReadinessCheck::None => "none".to_owned(),
        ReadinessCheck::Tcp { host, port, .. } => format!("TCP {host}:{port}"),
        ReadinessCheck::Http { url, .. } => format!("HTTP {url}"),
        ReadinessCheck::Command { command, .. } => format!("command {}", command_label(command)),
        ReadinessCheck::Delay { milliseconds } => format!("{milliseconds} ms"),
        ReadinessCheck::Container { name, .. } => format!("container {name}"),
        ReadinessCheck::Compose { services, .. } => {
            if services.is_empty() {
                "Compose services".to_owned()
            } else {
                format!("Compose {}", services.join(", "))
            }
        }
    }
}

fn workspace_target_label(workspace: &WorkspaceSpec) -> String {
    match &workspace.target {
        WorkspaceTarget::Current => "current".to_owned(),
        WorkspaceTarget::Existing { reference } => match reference {
            crate::domain::WorkspaceReference::Name(name) => format!("existing {name}"),
            crate::domain::WorkspaceReference::Identifier(identifier) => {
                format!("existing #{identifier}")
            }
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

fn is_save_shortcut(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
        return false;
    }
    key.modifiers == KeyModifiers::NONE
        || key.modifiers == KeyModifiers::SHIFT
        || key.modifiers.contains(KeyModifiers::CONTROL)
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        application::ports::DesktopWorkspaceSnapshot,
        domain::{
            ActionKind, ActionSpec, EnvironmentConfig, ExecutionMode, TilingPreference,
            WorkspaceTarget,
        },
        infrastructure::filesystem::local::LocalFileSystem,
    };

    use super::{
        EditorMode, EditorPanel, EditorState, InspectorField, SaveOutcome, action_palette,
    };

    #[test]
    fn palette_contains_the_capability_oriented_mvp_actions() {
        let palette = action_palette();
        assert_eq!(palette.len(), 11);
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Open Project with Zed")
        );
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Start Docker Compose stack")
        );
        assert!(palette.iter().any(|entry| entry.label == "Custom action"));
        assert!(
            palette
                .iter()
                .all(|entry| entry.label != "Create or select workspace")
        );
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
    fn validation_feedback_is_deferred_and_uses_the_action_name() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(mut action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        action.display_label = Some("Blog editor".to_owned());
        assert!(configuration.add_action(action).is_ok());
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        assert!(editor.validation_errors.is_empty());
        assert!(editor.validate().is_err());
        assert_eq!(editor.validation_errors.len(), 1);
        assert!(editor.validation_errors[0].contains("Blog editor"));
        assert!(!editor.validation_errors[0].contains("open-project"));
    }

    #[test]
    fn changing_the_invalid_field_revalidates_and_removes_its_error() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        let action_id = action.id.clone();
        assert!(configuration.add_action(action).is_ok());
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        assert!(editor.validate().is_err());
        assert!(!editor.validation_errors.is_empty());
        assert!(
            editor
                .set_action_project_path(&action_id, Some("~/Projects/blog".to_owned()))
                .is_ok()
        );
        assert!(editor.validation_errors.is_empty());
    }

    #[test]
    fn dependency_editor_rejects_self_and_missing_references_and_can_remove_edges() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let first = editor.add_action_from_palette(10);
        assert!(first.is_ok());
        let Some(first) = first.ok() else {
            return;
        };
        let second = editor.add_action_from_palette(10);
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
    fn keyboard_editor_uses_contextual_fields_and_generic_navigation() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let first = editor.add_action_from_palette(2);
        assert!(first.is_ok());
        let second = editor.add_action_from_palette(2);
        assert!(second.is_ok());

        assert_eq!(editor.panel, EditorPanel::Actions);
        assert_eq!(
            editor.inspector_fields(),
            vec![
                super::InspectorField::ActionLabel,
                super::InspectorField::Command,
                super::InspectorField::WorkingDirectory,
                super::InspectorField::ExecutionMode,
                super::InspectorField::Dependencies,
            ]
        );
        editor.handle_key(KeyCode::Enter);
        assert_eq!(editor.panel, EditorPanel::Inspector);
        assert_eq!(editor.handle_key(KeyCode::Down), super::EditorAction::None);
        assert_eq!(
            editor.selected_inspector_field(),
            Some(super::InspectorField::Command)
        );
        editor.handle_key(KeyCode::Enter);
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
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Enter);
        editor.handle_key(KeyCode::Up);
        editor.handle_key(KeyCode::Up);
        editor.handle_key(KeyCode::Enter);
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.execution_mode),
            Some(ExecutionMode::RunOnce)
        );
        editor.handle_key(KeyCode::Down);
        assert_eq!(
            editor.selected_inspector_field(),
            Some(super::InspectorField::Dependencies)
        );
        editor.handle_key(KeyCode::Enter);
        editor.handle_key(KeyCode::Char(' '));
        editor.handle_key(KeyCode::Enter);
        assert_eq!(
            editor
                .selected_action_spec()
                .map(|action| action.depends_on.len()),
            Some(1)
        );
        assert_eq!(editor.handle_key(KeyCode::Esc), super::EditorAction::None);
        assert_eq!(editor.panel, EditorPanel::Actions);
    }

    #[test]
    fn dependency_values_use_action_names_instead_of_identifiers() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
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
        mobile.depends_on.push(api_id.clone());
        assert!(configuration.add_action(api).is_ok());
        assert!(configuration.add_action(mobile).is_ok());

        let mut editor = EditorState::new(configuration, EditorMode::Create);
        editor.selected_action = Some(1);
        assert_eq!(
            editor.inspector_field_value(InspectorField::Dependencies),
            "Open API"
        );
        assert_ne!(
            editor.inspector_field_value(InspectorField::Dependencies),
            api_id.to_string()
        );
    }

    #[test]
    fn contextual_workspace_field_links_and_assigns_a_live_workspace() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let workspace = DesktopWorkspaceSnapshot {
            identity: "cosmic-2".to_owned(),
            name: Some("Code".to_owned()),
            position: Some(1),
            focused: false,
            tiling_enabled: Some(true),
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        let mut configuration = configuration;
        assert!(configuration.add_action(action).is_ok());
        let mut editor = EditorState::new(configuration, EditorMode::Create)
            .with_live_workspaces(vec![workspace]);
        editor.handle_key(KeyCode::Enter);
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Enter);
        assert!(editor.inspector_picker.is_some());
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Enter);
        assert!(editor.workspace_picker_open);
        assert_eq!(editor.handle_key(KeyCode::Enter), super::EditorAction::None);
        assert!(!editor.workspace_picker_open);
        assert_eq!(editor.configuration.workspaces.len(), 1);
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.desktop_workspace.clone()),
            editor
                .configuration
                .workspaces
                .first()
                .map(|workspace| workspace.id.clone())
        );
        assert_eq!(
            editor.configuration.workspaces[0].target,
            WorkspaceTarget::Existing {
                reference: crate::domain::WorkspaceReference::Identifier("cosmic-2".to_owned())
            }
        );
        assert_eq!(
            editor.configuration.workspaces[0].name.as_deref(),
            Some("Code")
        );
    }

    #[test]
    fn open_project_exposes_only_contextual_fields_and_defaults_to_zed() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let editor = EditorState::new(configuration, EditorMode::Create);
        let fields = editor.inspector_fields();
        assert!(fields.contains(&super::InspectorField::ProjectPath));
        assert!(fields.contains(&super::InspectorField::DesktopWorkspace));
        assert!(!fields.contains(&super::InspectorField::Application));
        assert!(!fields.contains(&super::InspectorField::WorkingDirectory));
        assert!(!fields.contains(&super::InspectorField::ExecutionMode));
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.application.as_deref()),
            Some("zed")
        );
    }

    #[test]
    fn enter_and_escape_move_between_action_and_inspector_focus_and_control_s_saves() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        assert!(editor.add_action_from_palette(10).is_ok());
        assert_eq!(editor.panel, EditorPanel::Actions);
        assert_eq!(editor.handle_key(KeyCode::Right), super::EditorAction::None);
        assert_eq!(editor.panel, EditorPanel::Inspector);
        assert_eq!(editor.handle_key(KeyCode::Left), super::EditorAction::None);
        assert_eq!(editor.panel, EditorPanel::Actions);
        assert_eq!(editor.handle_key(KeyCode::Enter), super::EditorAction::None);
        assert_eq!(editor.panel, EditorPanel::Inspector);
        assert_eq!(editor.handle_key(KeyCode::Esc), super::EditorAction::None);
        assert_eq!(editor.panel, EditorPanel::Actions);
        assert_eq!(
            editor.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL,)),
            super::EditorAction::SaveRequested
        );
    }

    #[test]
    fn action_mutation_shortcuts_are_scoped_to_the_actions_panel() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        assert!(editor.add_action_from_palette(10).is_ok());
        editor.panel = EditorPanel::Inspector;

        assert_eq!(
            editor.handle_key(KeyCode::Char('a')),
            super::EditorAction::None
        );
        assert!(!editor.palette_open);
        assert_eq!(
            editor.handle_key(KeyCode::Char('d')),
            super::EditorAction::None
        );
        assert!(!editor.delete_confirmation);

        editor.panel = EditorPanel::Actions;
        assert_eq!(
            editor.handle_key(KeyCode::Char('a')),
            super::EditorAction::PaletteOpened
        );
        assert!(editor.palette_open);
        assert_eq!(editor.handle_key(KeyCode::Esc), super::EditorAction::None);
        assert_eq!(
            editor.handle_key(KeyCode::Char('d')),
            super::EditorAction::None
        );
        assert!(editor.delete_confirmation);
    }
}
