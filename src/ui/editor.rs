use std::{collections::BTreeSet, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    application::ports::{
        AndroidVirtualDevice, ConfigStore, DesktopWorkspaceSnapshot, DirectoryCatalog,
        DirectoryCompletion, DirectorySuggestion, FileCatalog, FileSystem, InstalledApplication,
    },
    domain::{
        ActionId, ActionKind, ActionSpec, CommandSpec, ComposeSpec, ContainerSpec, DomainError,
        EmulatorSpec, EnvironmentConfig, ExecutionMode, TilingPreference, WorkspaceId,
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
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorField {
    EnvironmentName,
    ActionDisplayLabel,
    ApplicationArguments,
    WorkingDirectory,
    ProjectPath,
    CommandProgram,
    ContainerName,
    ContainerImage,
    ComposeFile,
    EmulatorAvd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorField {
    ActionLabel,
    Application,
    ApplicationArguments,
    ProjectPath,
    WorkingDirectory,
    Command,
    ExecutionMode,
    DesktopWorkspace,
    Tiling,
    ContainerName,
    ContainerImage,
    ComposeFile,
    EmulatorAvd,
    Dependencies,
}

impl InspectorField {
    pub fn label(self) -> &'static str {
        match self {
            Self::ActionLabel => "Action name",
            Self::Application => "Application",
            Self::ApplicationArguments => "Arguments",
            Self::ProjectPath => "Project path",
            Self::WorkingDirectory => "Working directory",
            Self::Command => "Command",
            Self::ExecutionMode => "Execution mode",
            Self::DesktopWorkspace => "Desktop workspace",
            Self::Tiling => "Tiling",
            Self::ContainerName => "Container",
            Self::ContainerImage => "Container image",
            Self::ComposeFile => "Compose file",
            Self::EmulatorAvd => "Device",
            Self::Dependencies => "Depends on",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    pub field: EditorField,
    pub value: String,
    pub cursor: usize,
    pub replace_on_next_char: bool,
    pub path_completion: Option<PathInputState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathInputState {
    pub suggestions: Vec<DirectorySuggestion>,
    pub selected: Option<usize>,
    pub validation_error: Option<String>,
}

impl PathInputState {
    fn from_completion(completion: DirectoryCompletion, value: &str) -> Self {
        let selected = completion
            .suggestions
            .iter()
            .position(|suggestion| suggestion.value == value)
            .or_else(|| (!completion.suggestions.is_empty()).then_some(0));
        Self {
            suggestions: completion.suggestions,
            selected,
            validation_error: completion.validation_error,
        }
    }

    fn selected_value(&self) -> Option<String> {
        self.selected
            .and_then(|index| self.suggestions.get(index))
            .map(|suggestion| suggestion.value.clone())
    }

    fn move_selection(&mut self, offset: isize) {
        if self.suggestions.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        self.selected =
            Some((current + offset).rem_euclid(self.suggestions.len() as isize) as usize);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorChoice {
    pub label: String,
    pub detail: Option<String>,
    pub value: InspectorChoiceValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorChoiceValue {
    Application(Option<String>),
    Emulator(Option<String>),
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
            label: "Open Project with VS Code",
            kind: ActionKind::OpenProjectWithVsCode,
        },
        ActionPaletteEntry {
            label: "Open Project with Cursor",
            kind: ActionKind::OpenProjectWithCursor,
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
struct DuplicateCommandGroup {
    command: CommandSpec,
    working_directory: Option<String>,
    execution_mode: ExecutionMode,
    action_names: Vec<String>,
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
    pub installed_applications: Vec<InstalledApplication>,
    pub application_observation_error: Option<String>,
    pub available_android_virtual_devices: Vec<AndroidVirtualDevice>,
    pub android_observation_error: Option<String>,
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
            if let Some(application) = action.kind.project_editor_application() {
                action.parameters.application = Some(application.to_owned());
            }
        }
        let selected_action = (!configuration.actions.is_empty()).then_some(0);
        Self {
            configuration,
            installed_applications: Vec::new(),
            application_observation_error: None,
            available_android_virtual_devices: Vec::new(),
            android_observation_error: None,
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

    pub fn with_installed_applications(
        mut self,
        mut applications: Vec<InstalledApplication>,
    ) -> Self {
        applications.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        self.installed_applications = applications;
        self
    }

    pub fn with_application_observation_error(mut self, error: impl Into<String>) -> Self {
        self.application_observation_error = Some(error.into());
        self
    }

    pub fn with_available_android_virtual_devices(
        mut self,
        mut devices: Vec<AndroidVirtualDevice>,
    ) -> Self {
        devices.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });
        self.available_android_virtual_devices = devices;
        self
    }

    pub fn with_android_observation_error(mut self, error: impl Into<String>) -> Self {
        self.android_observation_error = Some(error.into());
        self
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
                InspectorField::ApplicationArguments,
                InspectorField::WorkingDirectory,
                InspectorField::DesktopWorkspace,
            ]),
            ActionKind::OpenProject
            | ActionKind::OpenProjectWithVsCode
            | ActionKind::OpenProjectWithCursor => fields.extend([
                InspectorField::ProjectPath,
                InspectorField::DesktopWorkspace,
            ]),
            ActionKind::RunCommand => fields.extend([
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
                    InspectorField::ContainerImage,
                    InspectorField::WorkingDirectory,
                ]);
            }
            ActionKind::StartCompose => fields.extend([
                InspectorField::WorkingDirectory,
                InspectorField::ComposeFile,
            ]),
            ActionKind::StartAndroidEmulator => {
                fields.extend([
                    InspectorField::EmulatorAvd,
                    InspectorField::DesktopWorkspace,
                ]);
            }
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
            InspectorField::Application => action
                .parameters
                .application
                .as_ref()
                .map(|id| self.application_label(id))
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ApplicationArguments => {
                application_arguments_label(&action.parameters.application_arguments)
            }
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
            InspectorField::ContainerImage => action
                .parameters
                .container
                .as_ref()
                .and_then(|container| container.image.clone())
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::ComposeFile => action
                .parameters
                .compose
                .as_ref()
                .and_then(|compose| compose.compose_file.clone())
                .unwrap_or_else(|| "not set".to_owned()),
            InspectorField::EmulatorAvd => action
                .parameters
                .emulator
                .as_ref()
                .map(|emulator| emulator.avd.clone())
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
        if let Some(application) = action.kind.project_editor_application() {
            action.parameters.application = Some(application.to_owned());
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
        if mode.is_some() && !matches!(&action.kind, ActionKind::RunCommand) {
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

    pub fn set_action_application_arguments(
        &mut self,
        action_id: &ActionId,
        arguments: Vec<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if !matches!(&action.kind, ActionKind::OpenApplication) {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                format!("application arguments are not available for action '{action_id}'"),
            ));
        }
        if arguments
            .iter()
            .any(|argument| argument.contains('\0') || argument.chars().any(char::is_control))
        {
            return Err(WorkstateError::new(
                ErrorCategory::Ui,
                "application arguments must not contain control characters",
            ));
        }
        action.parameters.application_arguments = arguments;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_project_path(
        &mut self,
        action_id: &ActionId,
        project_path: Option<String>,
    ) -> Result<()> {
        let action = self.action_mut(action_id)?;
        if action.kind.project_editor_application().is_none() {
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
        if !matches!(&action.kind, ActionKind::RunCommand) {
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
                environment: Default::default(),
                mounts: Vec::new(),
                ports: Vec::new(),
            });
        container.name = name;
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_container_image(
        &mut self,
        action_id: &ActionId,
        image: String,
    ) -> Result<()> {
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
                environment: Default::default(),
                mounts: Vec::new(),
                ports: Vec::new(),
            });
        container.image = (!image.trim().is_empty()).then(|| image.trim().to_owned());
        self.mark_dirty();
        Ok(())
    }

    pub fn set_action_compose_file(&mut self, action_id: &ActionId, file: String) -> Result<()> {
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
                compose_file: None,
                services: Vec::new(),
                up_command: None,
                down_command: None,
            });
        compose.compose_file = (!file.trim().is_empty()).then(|| file.trim().to_owned());
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
        if avd.trim().is_empty() {
            action.parameters.emulator = None;
            self.mark_dirty();
            return Ok(());
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

    pub fn duplicate_command_warning(&self) -> Option<String> {
        let group = self.duplicate_command_groups().into_iter().next()?;
        let directory = group
            .working_directory
            .as_deref()
            .unwrap_or("the default working directory");
        let action_names = group.action_names.join(", ");
        Some(format!(
            "The same command '{}' is configured for actions {} in '{}' with '{}' execution mode. Are you sure you want to continue saving?",
            group.command.display_line(),
            action_names,
            directory,
            execution_mode_label(group.execution_mode),
        ))
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
        self.handle_key_event_with_directory_catalog(key, None)
    }

    pub fn handle_key_event_with_directory_catalog(
        &mut self,
        key: KeyEvent,
        directory_catalog: Option<&dyn DirectoryCatalog>,
    ) -> EditorAction {
        self.handle_key_event_with_catalogs(key, directory_catalog, None)
    }

    pub fn handle_key_event_with_catalogs(
        &mut self,
        key: KeyEvent,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) -> EditorAction {
        if self.workspace_picker_open {
            return self.handle_workspace_picker_key(key.code);
        }
        if self.inspector_picker.is_some() {
            return self.handle_inspector_picker_key(key.code);
        }
        if self.input.is_some() {
            return self.handle_input_key(key.code, directory_catalog, file_catalog);
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
                    self.activate_selected_inspector_field(directory_catalog, file_catalog);
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

    fn activate_selected_inspector_field(
        &mut self,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) {
        let Some(field) = self.selected_inspector_field() else {
            self.notice = Some("No editable fields are available for this action.".to_owned());
            return;
        };
        match field {
            InspectorField::ActionLabel => self.begin_input(EditorField::ActionDisplayLabel),
            InspectorField::Application => self.open_application_picker(),
            InspectorField::ApplicationArguments => {
                self.begin_input(EditorField::ApplicationArguments)
            }
            InspectorField::ProjectPath => {
                self.begin_input_with_directory_catalog(EditorField::ProjectPath, directory_catalog)
            }
            InspectorField::WorkingDirectory => self.begin_input_with_directory_catalog(
                EditorField::WorkingDirectory,
                directory_catalog,
            ),
            InspectorField::Command => self.begin_input(EditorField::CommandProgram),
            InspectorField::ContainerName => self.begin_input(EditorField::ContainerName),
            InspectorField::ContainerImage => self.begin_input(EditorField::ContainerImage),
            InspectorField::ComposeFile => self.begin_input_with_catalogs(
                EditorField::ComposeFile,
                directory_catalog,
                file_catalog,
            ),
            InspectorField::EmulatorAvd => self.open_emulator_picker(),
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

    fn open_application_picker(&mut self) {
        if self.installed_applications.is_empty() {
            self.record_notice(
                self.application_observation_error
                    .clone()
                    .unwrap_or_else(|| "No installed applications were found.".to_owned()),
            );
            return;
        }

        let current = self
            .selected_action_spec()
            .and_then(|action| action.parameters.application.clone());
        let mut options = Vec::with_capacity(self.installed_applications.len() + 1);
        options.push(InspectorChoice {
            label: "No application selected".to_owned(),
            detail: Some("Clear the current application".to_owned()),
            value: InspectorChoiceValue::Application(None),
        });
        options.extend(
            self.installed_applications
                .iter()
                .map(|application| InspectorChoice {
                    label: application.name.clone(),
                    detail: Some(application.id.clone()),
                    value: InspectorChoiceValue::Application(Some(application.id.clone())),
                }),
        );
        let selected = options
            .iter()
            .position(|option| {
                matches!(&option.value, InspectorChoiceValue::Application(value) if value == &current)
            })
            .unwrap_or(0);
        self.inspector_picker = Some(InspectorPicker::Choices {
            field: InspectorField::Application,
            title: "Application".to_owned(),
            options,
            selected,
        });
    }

    fn open_emulator_picker(&mut self) {
        if self.available_android_virtual_devices.is_empty() {
            self.record_notice(self.android_observation_error.clone().unwrap_or_else(|| {
                "No Android Virtual Devices were found. Create an AVD and try again.".to_owned()
            }));
            return;
        }

        let current = self.selected_action_spec().and_then(|action| {
            action
                .parameters
                .emulator
                .as_ref()
                .map(|emulator| emulator.avd.clone())
        });
        let mut options = vec![InspectorChoice {
            label: "No Android Virtual Device selected".to_owned(),
            detail: Some("Leave the action unconfigured".to_owned()),
            value: InspectorChoiceValue::Emulator(None),
        }];
        options.extend(self.available_android_virtual_devices.iter().map(|device| {
            InspectorChoice {
                label: device.name.clone(),
                detail: Some("Android Virtual Device".to_owned()),
                value: InspectorChoiceValue::Emulator(Some(device.name.clone())),
            }
        }));
        let selected = options
            .iter()
            .position(|option| {
                matches!(&option.value, InspectorChoiceValue::Emulator(value) if value == &current)
            })
            .unwrap_or(0);
        self.inspector_picker = Some(InspectorPicker::Choices {
            field: InspectorField::EmulatorAvd,
            title: "Android Virtual Device".to_owned(),
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
                    InspectorChoiceValue::Application(application) => {
                        let result = self.selected_action_id().and_then(|action_id| {
                            self.set_action_application(&action_id, application)
                        });
                        self.record_error(result);
                    }
                    InspectorChoiceValue::Emulator(avd) => {
                        let result = self.selected_action_id().and_then(|action_id| {
                            self.set_action_emulator_avd(&action_id, avd.unwrap_or_default())
                        });
                        self.record_error(result);
                    }
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

    fn handle_input_key(
        &mut self,
        key: KeyCode,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) -> EditorAction {
        match key {
            KeyCode::Left => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                input.cursor = input.cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                input.cursor = input
                    .cursor
                    .saturating_add(1)
                    .min(input.value.chars().count());
            }
            KeyCode::Home => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                input.cursor = 0;
            }
            KeyCode::End => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                input.cursor = input.value.chars().count();
            }
            KeyCode::Up => self.move_path_suggestion(-1),
            KeyCode::Down => self.move_path_suggestion(1),
            KeyCode::Char(character) => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                if input.replace_on_next_char {
                    input.value.clear();
                    input.cursor = 0;
                    input.replace_on_next_char = false;
                }
                let byte_index = input
                    .value
                    .char_indices()
                    .nth(input.cursor)
                    .map_or(input.value.len(), |(index, _)| index);
                input.value.insert(byte_index, character);
                input.cursor = input.cursor.saturating_add(1);
                self.refresh_path_completion(directory_catalog, file_catalog);
            }
            KeyCode::Backspace => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                if input.cursor > 0 {
                    let start = input
                        .value
                        .char_indices()
                        .nth(input.cursor - 1)
                        .map_or(0, |(index, _)| index);
                    let end = input
                        .value
                        .char_indices()
                        .nth(input.cursor)
                        .map_or(input.value.len(), |(index, _)| index);
                    input.value.replace_range(start..end, "");
                    input.cursor -= 1;
                }
                self.refresh_path_completion(directory_catalog, file_catalog);
            }
            KeyCode::Delete => {
                let Some(input) = self.input.as_mut() else {
                    return EditorAction::None;
                };
                input.replace_on_next_char = false;
                if input.cursor < input.value.chars().count() {
                    let start = input
                        .value
                        .char_indices()
                        .nth(input.cursor)
                        .map_or(input.value.len(), |(index, _)| index);
                    let end = input
                        .value
                        .char_indices()
                        .nth(input.cursor.saturating_add(1))
                        .map_or(input.value.len(), |(index, _)| index);
                    input.value.replace_range(start..end, "");
                }
                self.refresh_path_completion(directory_catalog, file_catalog);
            }
            KeyCode::Tab => {
                self.complete_selected_path(directory_catalog, file_catalog);
            }
            KeyCode::Enter => {
                let selected_value = self
                    .input
                    .as_ref()
                    .and_then(|input| input.path_completion.as_ref())
                    .and_then(PathInputState::selected_value);
                let path_state = self.input.as_ref().and_then(|input| {
                    input.path_completion.as_ref().map(|completion| {
                        (input.value.is_empty(), completion.validation_error.clone())
                    })
                });
                if let Some(selected_value) = selected_value {
                    if let Some(mut input) = self.input.take() {
                        input.value = selected_value;
                        input.cursor = input.value.chars().count();
                        self.commit_input(input);
                    }
                } else if path_state.as_ref().is_some_and(|(is_empty, _)| *is_empty) {
                    let message = self
                        .input
                        .as_ref()
                        .map(|input| match input.field {
                            EditorField::ComposeFile => {
                                "Cannot apply Compose file: a file is required."
                            }
                            _ => "Cannot apply path: a directory is required.",
                        })
                        .unwrap_or("Cannot apply path: a directory is required.");
                    self.record_notice(message.to_owned());
                } else if let Some(error) = path_state.and_then(|(_, error)| error) {
                    let prefix = self
                        .input
                        .as_ref()
                        .map(|input| match input.field {
                            EditorField::ComposeFile => "Cannot apply Compose file",
                            _ => "Cannot apply path",
                        })
                        .unwrap_or("Cannot apply path");
                    self.record_notice(format!("{prefix}: {error}"));
                } else if let Some(input) = self.input.take() {
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
            EditorField::ApplicationArguments => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| {
                    CommandSpec::arguments_from_argv_line(&action_id, &value)
                        .map_err(WorkstateError::from)
                        .and_then(|arguments| {
                            self.set_action_application_arguments(&action_id, arguments)
                        })
                }),
            EditorField::WorkingDirectory => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_working_directory(&action_id, Some(value))),
            EditorField::ProjectPath => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_project_path(&action_id, Some(value))),
            EditorField::CommandProgram => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| {
                    CommandSpec::from_argv_line(&action_id, &value)
                        .map_err(WorkstateError::from)
                        .and_then(|command| self.set_command(&action_id, Some(command)))
                }),
            EditorField::ContainerName => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_container_name(&action_id, value)),
            EditorField::ContainerImage => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_container_image(&action_id, value)),
            EditorField::ComposeFile => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_compose_file(&action_id, value)),
            EditorField::EmulatorAvd => self
                .selected_action_spec()
                .map(|action| action.id.clone())
                .ok_or_else(|| WorkstateError::new(ErrorCategory::Ui, "no action is selected"))
                .and_then(|action_id| self.set_action_emulator_avd(&action_id, value)),
        };
        if let Err(error) = result {
            self.record_notice(error.to_string());
        } else {
            self.mark_dirty();
        }
    }

    pub fn begin_input(&mut self, field: EditorField) {
        self.begin_input_with_directory_catalog(field, None);
    }

    pub fn begin_input_with_directory_catalog(
        &mut self,
        field: EditorField,
        directory_catalog: Option<&dyn DirectoryCatalog>,
    ) {
        self.begin_input_with_catalogs(field, directory_catalog, None);
    }

    pub fn begin_input_with_catalogs(
        &mut self,
        field: EditorField,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) {
        let value = match field {
            EditorField::EnvironmentName => self.configuration.name.to_string(),
            EditorField::ActionDisplayLabel => self
                .selected_action_spec()
                .and_then(|action| action.display_label.clone())
                .unwrap_or_default(),
            EditorField::ApplicationArguments => self
                .selected_action_spec()
                .map(|action| application_arguments_input(&action.parameters.application_arguments))
                .unwrap_or_default(),
            EditorField::WorkingDirectory => self
                .selected_action_spec()
                .and_then(|action| action.working_directory.clone())
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
                        .map(CommandSpec::display_line)
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
            EditorField::ContainerImage => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .container
                        .as_ref()
                        .and_then(|container| container.image.clone())
                })
                .unwrap_or_default(),
            EditorField::ComposeFile => self
                .selected_action_spec()
                .and_then(|action| {
                    action
                        .parameters
                        .compose
                        .as_ref()
                        .and_then(|compose| compose.compose_file.clone())
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
        };
        let cursor = value.chars().count();
        self.input = Some(TextInput {
            field,
            value,
            cursor,
            replace_on_next_char: true,
            path_completion: is_path_completion_field(field).then_some(PathInputState::default()),
        });
        self.refresh_path_completion(directory_catalog, file_catalog);
    }

    fn move_path_suggestion(&mut self, offset: isize) {
        if let Some(input) = self.input.as_mut()
            && let Some(completion) = input.path_completion.as_mut()
        {
            completion.move_selection(offset);
        }
    }

    fn complete_selected_path(
        &mut self,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) {
        let Some((value, add_separator)) = self.input.as_ref().and_then(|input| {
            input.path_completion.as_ref().and_then(|completion| {
                completion
                    .selected_value()
                    .map(|value| (value, is_directory_field(input.field)))
            })
        }) else {
            return;
        };
        let value = if add_separator {
            append_directory_separator(value)
        } else {
            value
        };
        if let Some(input) = self.input.as_mut() {
            input.value = value;
            input.cursor = input.value.chars().count();
            input.replace_on_next_char = false;
        }
        self.refresh_path_completion(directory_catalog, file_catalog);
    }

    fn refresh_path_completion(
        &mut self,
        directory_catalog: Option<&dyn DirectoryCatalog>,
        file_catalog: Option<&dyn FileCatalog>,
    ) {
        let Some((field, value)) = self
            .input
            .as_ref()
            .filter(|input| is_path_completion_field(input.field))
            .map(|input| (input.field, input.value.clone()))
        else {
            return;
        };
        if value.is_empty() && is_directory_field(field) {
            if let Some(input) = self.input.as_mut()
                && let Some(path_completion) = input.path_completion.as_mut()
            {
                *path_completion = PathInputState::default();
            }
            return;
        }
        let completion = if is_directory_field(field) {
            let Some(directory_catalog) = directory_catalog else {
                return;
            };
            match directory_catalog.complete(&value) {
                Ok(completion) => completion,
                Err(error) => DirectoryCompletion {
                    suggestions: Vec::new(),
                    validation_error: Some(error.to_string()),
                },
            }
        } else {
            let Some(file_catalog) = file_catalog else {
                return;
            };
            let working_directory = self
                .selected_action_spec()
                .and_then(|action| action.working_directory.as_deref())
                .unwrap_or_default();
            match file_catalog.complete_yaml(working_directory, &value) {
                Ok(completion) => completion,
                Err(error) => DirectoryCompletion {
                    suggestions: Vec::new(),
                    validation_error: Some(error.to_string()),
                },
            }
        };
        if let Some(input) = self.input.as_mut()
            && let Some(path_completion) = input.path_completion.as_mut()
        {
            *path_completion = PathInputState::from_completion(completion, &value);
        }
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
        let raw_base = kind.key();
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

    fn duplicate_command_groups(&self) -> Vec<DuplicateCommandGroup> {
        let mut groups = Vec::new();
        for action in &self.configuration.actions {
            let (Some(command), Some(execution_mode)) =
                (&action.parameters.command, action.execution_mode)
            else {
                continue;
            };
            if !matches!(&action.kind, ActionKind::RunCommand) {
                continue;
            }
            let group_index = groups.iter().position(|group: &DuplicateCommandGroup| {
                group.command == *command
                    && group.working_directory == action.working_directory
                    && group.execution_mode == execution_mode
            });
            if let Some(group_index) = group_index {
                groups[group_index].action_names.push(action_label(action));
            } else {
                groups.push(DuplicateCommandGroup {
                    command: command.clone(),
                    working_directory: action.working_directory.clone(),
                    execution_mode,
                    action_names: vec![action_label(action)],
                });
            }
        }
        groups
            .into_iter()
            .filter(|group| group.action_names.len() > 1)
            .collect()
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

    fn application_label(&self, application_id: &str) -> String {
        self.installed_applications
            .iter()
            .find(|application| application.id == application_id)
            .map(|application| format!("{} ({})", application.name, application.id))
            .unwrap_or_else(|| application_id.to_owned())
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
        ActionKind::OpenProjectWithVsCode => "Open Project with VS Code".to_owned(),
        ActionKind::OpenProjectWithCursor => "Open Project with Cursor".to_owned(),
        ActionKind::RunCommand => "Run command".to_owned(),
        ActionKind::ConfigureTiling => "Configure tiling".to_owned(),
        ActionKind::StartContainer => "Start Docker container".to_owned(),
        ActionKind::StartCompose => "Start Docker Compose stack".to_owned(),
        ActionKind::StartAndroidEmulator => "Start Android Emulator".to_owned(),
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
            "application_arguments" => Some(InspectorField::ApplicationArguments),
            "project_path" => Some(InspectorField::ProjectPath),
            "working_directory" => Some(InspectorField::WorkingDirectory),
            "command" => Some(InspectorField::Command),
            "desktop_workspace" => Some(InspectorField::DesktopWorkspace),
            "container" | "container.name" => Some(InspectorField::ContainerName),
            "container.image" => Some(InspectorField::ContainerImage),
            "compose" | "compose.compose_file" => Some(InspectorField::ComposeFile),
            "emulator" | "emulator.avd" => Some(InspectorField::EmulatorAvd),
            _ => None,
        },
        DomainError::InvalidActionTimeout { .. } | DomainError::InvalidRetryPolicy { .. } => None,
        DomainError::InvalidExecutionMode { .. } => Some(InspectorField::ExecutionMode),
        DomainError::InvalidCommand { .. } => Some(InspectorField::Command),
        DomainError::InvalidReadinessCheck { .. } => None,
        _ => None,
    }
}

fn command_label(command: &CommandSpec) -> String {
    command.display_line()
}

fn application_arguments_input(arguments: &[String]) -> String {
    if arguments.is_empty() {
        return String::new();
    }
    let mut command = CommandSpec::new("application");
    command.arguments = arguments.to_vec();
    command
        .display_line()
        .strip_prefix("application")
        .unwrap_or_default()
        .trim_start()
        .to_owned()
}

fn application_arguments_label(arguments: &[String]) -> String {
    let value = application_arguments_input(arguments);
    if value.is_empty() {
        "not set".to_owned()
    } else {
        value
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

fn append_directory_separator(mut value: String) -> String {
    if !value.ends_with('/') && !value.ends_with('\\') {
        value.push('/');
    }
    value
}

fn is_directory_field(field: EditorField) -> bool {
    matches!(
        field,
        EditorField::ProjectPath | EditorField::WorkingDirectory
    )
}

fn is_path_completion_field(field: EditorField) -> bool {
    is_directory_field(field) || matches!(field, EditorField::ComposeFile)
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
        application::ports::{
            DesktopWorkspaceSnapshot, DirectoryCatalog, DirectoryCompletion, DirectorySuggestion,
            FileCatalog, InstalledApplication,
        },
        domain::{
            ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, ExecutionMode,
            TilingPreference, WorkspaceTarget,
        },
        error::Result,
        infrastructure::filesystem::local::LocalFileSystem,
    };

    use super::{
        EditorField, EditorMode, EditorPanel, EditorState, InspectorField, SaveOutcome,
        action_palette,
    };

    struct FakeDirectoryCatalog;

    impl DirectoryCatalog for FakeDirectoryCatalog {
        fn complete(&self, input: &str) -> Result<DirectoryCompletion> {
            let suggestions = match input {
                "~/" => vec![
                    DirectorySuggestion {
                        name: "Code".to_owned(),
                        value: "~/Code".to_owned(),
                    },
                    DirectorySuggestion {
                        name: "Documents".to_owned(),
                        value: "~/Documents".to_owned(),
                    },
                ],
                "~/Code" => vec![DirectorySuggestion {
                    name: "Code".to_owned(),
                    value: "~/Code".to_owned(),
                }],
                "~/Code/" => vec![
                    DirectorySuggestion {
                        name: "Workspace".to_owned(),
                        value: "~/Code/Workspace".to_owned(),
                    },
                    DirectorySuggestion {
                        name: "api".to_owned(),
                        value: "~/Code/api".to_owned(),
                    },
                ],
                "~/Code/Workspace/" => Vec::new(),
                _ => Vec::new(),
            };
            let validation_error = if matches!(
                input,
                "~/" | "~/Code"
                    | "~/Code/"
                    | "~/Code/Workspace"
                    | "~/Code/Workspace/"
                    | "~/Code/api"
            ) {
                None
            } else {
                Some("path does not exist".to_owned())
            };
            Ok(DirectoryCompletion {
                suggestions,
                validation_error,
            })
        }
    }

    struct FakeFileCatalog;

    impl FileCatalog for FakeFileCatalog {
        fn complete_yaml(
            &self,
            working_directory: &str,
            input: &str,
        ) -> Result<DirectoryCompletion> {
            assert_eq!(working_directory, "~/project");
            let suggestions = vec![
                DirectorySuggestion {
                    name: "compose.yaml".to_owned(),
                    value: "compose.yaml".to_owned(),
                },
                DirectorySuggestion {
                    name: "docker-compose.yml".to_owned(),
                    value: "docker-compose.yml".to_owned(),
                },
            ];
            Ok(DirectoryCompletion {
                validation_error: (!input.is_empty())
                    .then_some("the selected Compose file does not exist".to_owned()),
                suggestions,
            })
        }
    }

    fn send_path_key(editor: &mut EditorState, catalog: &dyn DirectoryCatalog, key: KeyCode) {
        editor.handle_key_event_with_directory_catalog(
            KeyEvent::new(key, KeyModifiers::NONE),
            Some(catalog),
        );
    }

    fn send_file_key(editor: &mut EditorState, catalog: &dyn FileCatalog, key: KeyCode) {
        editor.handle_key_event_with_catalogs(
            KeyEvent::new(key, KeyModifiers::NONE),
            None,
            Some(catalog),
        );
    }

    #[test]
    fn palette_contains_the_capability_oriented_mvp_actions() {
        let palette = action_palette();
        assert_eq!(palette.len(), 9);
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Open Project with Zed")
        );
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Open Project with VS Code")
        );
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Open Project with Cursor")
        );
        assert!(
            palette
                .iter()
                .any(|entry| entry.label == "Start Docker Compose stack")
        );
        assert!(
            palette
                .iter()
                .all(|entry| entry.label != "Create or select workspace")
        );
        assert!(palette.iter().all(|entry| entry.label != "Start service"));
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
        let first = editor.add_action_from_palette(2);
        assert!(first.is_ok());
        let Some(first) = first.ok() else {
            return;
        };
        let second = editor.add_action_from_palette(2);
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
            vec!["run-command -> run-command-2".to_owned()]
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
    fn duplicate_command_warning_matches_command_directory_and_execution_mode() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(mut first) = ActionSpec::new("install-api", ActionKind::RunCommand).ok() else {
            return;
        };
        first.display_label = Some("Install API dependencies".to_owned());
        first.working_directory = Some("~/code/notefinder/notefinder-api".to_owned());
        first.execution_mode = Some(ExecutionMode::RunOnce);
        first.parameters.command = Some(CommandSpec {
            program: "bun".to_owned(),
            arguments: vec!["i".to_owned()],
            shell: false,
            environment: Default::default(),
        });
        let mut second = first.clone();
        second.id = match crate::domain::ActionId::new("install-api-again") {
            Ok(id) => id,
            Err(_) => return,
        };
        second.display_label = Some("Install API dependencies again".to_owned());
        assert!(configuration.add_action(first).is_ok());
        assert!(configuration.add_action(second).is_ok());
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        let warning = editor.duplicate_command_warning();
        assert!(warning.is_some());
        assert!(warning.as_deref().is_some_and(|message| {
            message.contains("Install API dependencies")
                && message.contains("bun i")
                && message.contains("run once")
        }));

        if let Some(action) = editor.configuration.actions.get_mut(1) {
            action.execution_mode = Some(ExecutionMode::Background);
        } else {
            return;
        }
        assert!(editor.duplicate_command_warning().is_none());

        if let Some(action) = editor.configuration.actions.get_mut(1) {
            action.execution_mode = Some(ExecutionMode::RunOnce);
            action.working_directory = Some("~/code/notefinder/notefinder-app".to_owned());
        } else {
            return;
        }
        assert!(editor.duplicate_command_warning().is_none());

        if let Some(action) = editor.configuration.actions.get_mut(1) {
            action.working_directory = Some("~/code/notefinder/notefinder-api".to_owned());
            action.parameters.command = Some(CommandSpec {
                program: "bun".to_owned(),
                arguments: vec!["install".to_owned()],
                shell: false,
                environment: Default::default(),
            });
        } else {
            return;
        }
        assert!(editor.duplicate_command_warning().is_none());
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
        for character in "bun i".chars() {
            editor.handle_key(KeyCode::Char(character));
        }
        editor.handle_key(KeyCode::Enter);
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.command.as_ref())
                .map(|command| command.program.as_str()),
            Some("bun")
        );
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.command.as_ref())
                .map(|command| command.arguments.clone()),
            Some(vec!["i".to_owned()])
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
    fn project_editor_actions_share_the_project_inspector_and_target_their_editor() {
        let Some(mut configuration) = EnvironmentConfig::new("Editors").ok() else {
            return;
        };
        for (index, kind) in [
            ActionKind::OpenProject,
            ActionKind::OpenProjectWithVsCode,
            ActionKind::OpenProjectWithCursor,
        ]
        .into_iter()
        .enumerate()
        {
            let Some(action) = ActionSpec::new(format!("project-{index}"), kind).ok() else {
                return;
            };
            assert!(configuration.add_action(action).is_ok());
        }

        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let fields = vec![
            InspectorField::ActionLabel,
            InspectorField::ProjectPath,
            InspectorField::DesktopWorkspace,
            InspectorField::Dependencies,
        ];
        for (index, expected_application) in ["zed", "code", "cursor"].into_iter().enumerate() {
            editor.selected_action = Some(index);
            editor.selected_inspector = Some(0);
            assert_eq!(editor.inspector_fields(), fields);
            assert_eq!(
                editor
                    .selected_action_spec()
                    .and_then(|action| action.parameters.application.as_deref()),
                Some(expected_application)
            );
        }
    }

    #[test]
    fn application_field_uses_a_picker_backed_by_installed_applications() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create)
            .with_installed_applications(vec![
                InstalledApplication {
                    id: "org.example.Editor".to_owned(),
                    name: "Editor".to_owned(),
                },
                InstalledApplication {
                    id: "org.example.Browser".to_owned(),
                    name: "Browser".to_owned(),
                },
            ]);
        assert!(editor.add_action_from_palette(0).is_ok());
        editor.handle_key(KeyCode::Enter);
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Enter);
        assert!(matches!(
            editor.inspector_picker,
            Some(super::InspectorPicker::Choices {
                field: super::InspectorField::Application,
                ..
            })
        ));
        editor.handle_key(KeyCode::Down);
        editor.handle_key(KeyCode::Enter);

        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.application.as_deref()),
            Some("org.example.Browser")
        );
        assert_eq!(
            editor.inspector_field_value(super::InspectorField::Application),
            "Browser (org.example.Browser)"
        );
    }

    #[test]
    fn open_application_exposes_and_parses_custom_arguments() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let action_id = editor.add_action_from_palette(0);
        assert!(action_id.is_ok());
        assert_eq!(
            editor.inspector_fields(),
            vec![
                InspectorField::ActionLabel,
                InspectorField::Application,
                InspectorField::ApplicationArguments,
                InspectorField::WorkingDirectory,
                InspectorField::DesktopWorkspace,
                InspectorField::Dependencies,
            ]
        );

        editor.begin_input(EditorField::ApplicationArguments);
        for character in "--new-window \"my project\"".chars() {
            editor.handle_key(KeyCode::Char(character));
        }
        editor.handle_key(KeyCode::Enter);

        assert_eq!(
            editor
                .selected_action_spec()
                .map(|action| action.parameters.application_arguments.clone()),
            Some(vec!["--new-window".to_owned(), "my project".to_owned()])
        );
        assert_eq!(
            editor.inspector_field_value(InspectorField::ApplicationArguments),
            "--new-window 'my project'"
        );
    }

    #[test]
    fn directory_fields_support_navigation_completion_and_live_validation() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let catalog = FakeDirectoryCatalog;
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        send_path_key(&mut editor, &catalog, KeyCode::Enter);
        send_path_key(&mut editor, &catalog, KeyCode::Down);
        send_path_key(&mut editor, &catalog, KeyCode::Enter);
        send_path_key(&mut editor, &catalog, KeyCode::Char('~'));
        send_path_key(&mut editor, &catalog, KeyCode::Char('/'));
        send_path_key(&mut editor, &catalog, KeyCode::Tab);
        assert_eq!(
            editor.input.as_ref().map(|input| input.value.as_str()),
            Some("~/Code/")
        );
        send_path_key(&mut editor, &catalog, KeyCode::Char('x'));
        assert!(
            editor
                .input
                .as_ref()
                .and_then(|input| input.path_completion.as_ref())
                .and_then(|completion| completion.validation_error.as_ref())
                .is_some()
        );
        send_path_key(&mut editor, &catalog, KeyCode::Enter);
        assert!(editor.input.is_some());
        assert_eq!(
            editor.notice.as_deref(),
            Some("Cannot apply path: path does not exist")
        );
        send_path_key(&mut editor, &catalog, KeyCode::Backspace);
        send_path_key(&mut editor, &catalog, KeyCode::Down);
        send_path_key(&mut editor, &catalog, KeyCode::Up);
        send_path_key(&mut editor, &catalog, KeyCode::Enter);
        assert!(editor.input.is_none());
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.project_path.as_deref()),
            Some("~/Code/Workspace")
        );
    }

    #[test]
    fn compose_file_is_after_working_directory_and_uses_file_completion() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("compose", ActionKind::StartCompose).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        let Some(action_id) = editor
            .selected_action_spec()
            .map(|action| action.id.clone())
        else {
            return;
        };
        assert!(
            editor
                .set_action_working_directory(&action_id, Some("~/project".to_owned()))
                .is_ok()
        );
        assert_eq!(
            editor.inspector_fields(),
            vec![
                InspectorField::ActionLabel,
                InspectorField::WorkingDirectory,
                InspectorField::ComposeFile,
                InspectorField::Dependencies,
            ]
        );

        let catalog = FakeFileCatalog;
        editor.begin_input_with_catalogs(EditorField::ComposeFile, None, Some(&catalog));
        assert_eq!(
            editor
                .input
                .as_ref()
                .and_then(|input| input.path_completion.as_ref())
                .map(|completion| completion.suggestions.len()),
            Some(2)
        );
        send_file_key(&mut editor, &catalog, KeyCode::Down);
        send_file_key(&mut editor, &catalog, KeyCode::Enter);
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.compose.as_ref())
                .and_then(|compose| compose.compose_file.as_deref()),
            Some("docker-compose.yml")
        );
    }

    #[test]
    fn directory_tab_appends_a_separator_and_refreshes_nested_suggestions() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let catalog = FakeDirectoryCatalog;
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        editor.begin_input_with_directory_catalog(EditorField::ProjectPath, Some(&catalog));
        for character in "~/".chars() {
            send_path_key(&mut editor, &catalog, KeyCode::Char(character));
        }
        send_path_key(&mut editor, &catalog, KeyCode::Tab);
        assert_eq!(
            editor.input.as_ref().map(|input| input.value.as_str()),
            Some("~/Code/")
        );
        send_path_key(&mut editor, &catalog, KeyCode::Tab);
        assert_eq!(
            editor.input.as_ref().map(|input| input.value.as_str()),
            Some("~/Code/Workspace/")
        );
    }

    #[test]
    fn directory_enter_uses_the_selected_suggestion_instead_of_the_typed_prefix() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let catalog = FakeDirectoryCatalog;
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        editor.begin_input_with_directory_catalog(EditorField::ProjectPath, Some(&catalog));
        for character in "~/Code/".chars() {
            send_path_key(&mut editor, &catalog, KeyCode::Char(character));
        }
        send_path_key(&mut editor, &catalog, KeyCode::Down);
        send_path_key(&mut editor, &catalog, KeyCode::Up);
        send_path_key(&mut editor, &catalog, KeyCode::Enter);

        assert!(editor.input.is_none());
        assert_eq!(
            editor
                .selected_action_spec()
                .and_then(|action| action.parameters.project_path.as_deref()),
            Some("~/Code/Workspace")
        );
    }

    #[test]
    fn directory_fields_reject_invalid_paths_after_enter() {
        let Some(mut configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let Some(action) = ActionSpec::new("open-project", ActionKind::OpenProject).ok() else {
            return;
        };
        assert!(configuration.add_action(action).is_ok());
        let catalog = FakeDirectoryCatalog;
        let mut editor = EditorState::new(configuration, EditorMode::Create);

        editor.begin_input_with_directory_catalog(EditorField::ProjectPath, Some(&catalog));
        for character in "~/Missing".chars() {
            send_path_key(&mut editor, &catalog, KeyCode::Char(character));
        }
        send_path_key(&mut editor, &catalog, KeyCode::Enter);
        assert!(
            editor
                .input
                .as_ref()
                .and_then(|input| input.path_completion.as_ref())
                .and_then(|completion| completion.validation_error.as_ref())
                .is_some()
        );
        assert!(editor.input.is_some());
        assert_eq!(
            editor.notice.as_deref(),
            Some("Cannot apply path: path does not exist")
        );
    }

    #[test]
    fn text_fields_support_cursor_navigation_and_insertion() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        editor.begin_input(EditorField::EnvironmentName);
        assert_eq!(editor.input.as_ref().map(|input| input.cursor), Some(4));

        editor.handle_key(KeyCode::Left);
        editor.handle_key(KeyCode::Char('X'));
        assert_eq!(
            editor.input.as_ref().map(|input| input.value.as_str()),
            Some("BloXg")
        );
        assert_eq!(editor.input.as_ref().map(|input| input.cursor), Some(4));

        editor.handle_key(KeyCode::Backspace);
        editor.handle_key(KeyCode::Home);
        editor.handle_key(KeyCode::Char('A'));
        editor.handle_key(KeyCode::Delete);
        assert_eq!(
            editor.input.as_ref().map(|input| input.value.as_str()),
            Some("Alog")
        );
        assert_eq!(editor.input.as_ref().map(|input| input.cursor), Some(1));
        editor.handle_key(KeyCode::End);
        assert_eq!(editor.input.as_ref().map(|input| input.cursor), Some(4));
    }

    #[test]
    fn enter_and_escape_move_between_action_and_inspector_focus_and_control_s_saves() {
        let Some(configuration) = EnvironmentConfig::new("Blog").ok() else {
            return;
        };
        let mut editor = EditorState::new(configuration, EditorMode::Create);
        assert!(editor.add_action_from_palette(2).is_ok());
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
        assert!(editor.add_action_from_palette(2).is_ok());
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
