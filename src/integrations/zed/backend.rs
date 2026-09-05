use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    application::{
        planner::CancellationToken,
        ports::{
            BoxFuture, Clock, DesktopBackend, EditorBackend, EditorOpenOutcome,
            EditorOperationStatus, EditorWindowSnapshot, FileSystem, ProcessRequest, ProcessRunner,
            SystemClock,
        },
        timeouts::DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
    },
    error::Result,
    infrastructure::filesystem::PathResolver,
};

use super::errors::ZedError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZedCommand {
    program: String,
    new_window_flag: Option<String>,
}

impl ZedCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            new_window_flag: Some("-n".to_owned()),
        }
    }

    pub fn without_new_window_flag(mut self) -> Self {
        self.new_window_flag = None;
        self
    }

    pub fn with_new_window_flag(mut self, flag: impl Into<String>) -> Self {
        self.new_window_flag = Some(flag.into());
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self, project_path: &Path) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(flag) = &self.new_window_flag {
            arguments.push(flag.clone());
        }
        arguments.push(project_path.display().to_string());
        arguments
    }
}

impl Default for ZedCommand {
    fn default() -> Self {
        Self::new("zed")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectEditorKind {
    Zed,
    VsCode,
    Cursor,
}

impl ProjectEditorKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Zed => "Zed",
            Self::VsCode => "VS Code",
            Self::Cursor => "Cursor",
        }
    }

    pub const fn application_id(self) -> &'static str {
        match self {
            Self::Zed => "zed",
            Self::VsCode => "code",
            Self::Cursor => "cursor",
        }
    }

    pub const fn executable(self) -> &'static str {
        self.application_id()
    }

    pub const fn new_window_flag(self) -> &'static str {
        match self {
            Self::Zed => "-n",
            Self::VsCode | Self::Cursor => "--new-window",
        }
    }

    pub fn matches_application(self, application: &str) -> bool {
        let normalized = normalize_application(application);
        match self {
            Self::Zed => matches!(normalized.as_str(), "zed" | "devzedzed" | "devzed"),
            Self::VsCode => matches!(
                normalized.as_str(),
                "code"
                    | "codeoss"
                    | "comvisualstudiocode"
                    | "comvisualstudiocodeoss"
                    | "vscode"
                    | "visualstudiocode"
            ),
            Self::Cursor => matches!(
                normalized.as_str(),
                "cursor" | "comtodesktop230313mzl4w4u92" | "appimagekitcursor"
            ),
        }
    }

    fn title_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::Zed => &["Zed"],
            Self::VsCode => &["Visual Studio Code", "Code - OSS", "Code"],
            Self::Cursor => &["Cursor"],
        }
    }

    fn title_with_known_editor_suffix(self, title: &str) -> Option<String> {
        const SEPARATORS: [&str; 4] = [" — ", " – ", " - ", ": "];
        let title = title.trim();

        for suffix in self.title_suffixes() {
            for separator in SEPARATORS {
                let marker = format!("{separator}{suffix}");
                if let Some(component) = title.strip_suffix(&marker) {
                    let component = component.trim();
                    if !component.is_empty() {
                        return Some(component.to_owned());
                    }
                }
            }
        }
        None
    }

    fn project_title_component(self, title: &str) -> Option<String> {
        let title = self
            .title_with_known_editor_suffix(title)
            .unwrap_or_else(|| title.trim().to_owned());
        let separators = [" — ", " – ", " - ", ": "];
        let split_index = match self {
            Self::Zed => separators
                .iter()
                .filter_map(|separator| title.find(separator))
                .min(),
            Self::VsCode | Self::Cursor => separators
                .iter()
                .filter_map(|separator| title.rfind(separator).map(|index| index + separator.len()))
                .max(),
        };
        let component = split_index
            .and_then(|index| match self {
                Self::Zed => title.get(..index),
                Self::VsCode | Self::Cursor => title.get(index..),
            })
            .unwrap_or(title.as_str())
            .trim();
        (!component.is_empty()).then(|| component.to_owned())
    }

    fn title_matches_project_name(self, title: &str, project_name: &str) -> bool {
        self.project_title_component(title)
            .is_some_and(|component| component == project_name)
    }

    fn has_known_editor_suffix(self, title: &str) -> bool {
        self.title_with_known_editor_suffix(title).is_some()
    }
}

fn normalize_application(application: &str) -> String {
    let application = application.rsplit('/').next().unwrap_or(application);
    let application = application.strip_suffix(".desktop").unwrap_or(application);
    application
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Clone)]
pub struct ZedBackend {
    runner: Arc<dyn ProcessRunner>,
    desktop: Arc<dyn DesktopBackend>,
    file_system: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    launch_lock: Arc<tokio::sync::Mutex<()>>,
    editor_kind: ProjectEditorKind,
    command: ZedCommand,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl ZedBackend {
    pub fn new(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
    ) -> Self {
        Self::with_clock(runner, desktop, file_system, Arc::new(SystemClock))
    }

    pub fn with_clock(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_clock_for_editor(runner, desktop, file_system, clock, ProjectEditorKind::Zed)
    }

    pub fn for_editor(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
        editor_kind: ProjectEditorKind,
    ) -> Self {
        Self::with_clock_for_editor(
            runner,
            desktop,
            file_system,
            Arc::new(SystemClock),
            editor_kind,
        )
    }

    pub fn with_clock_for_editor(
        runner: Arc<dyn ProcessRunner>,
        desktop: Arc<dyn DesktopBackend>,
        file_system: Arc<dyn FileSystem>,
        clock: Arc<dyn Clock>,
        editor_kind: ProjectEditorKind,
    ) -> Self {
        Self {
            runner,
            desktop,
            file_system,
            clock,
            launch_lock: Arc::new(tokio::sync::Mutex::new(())),
            editor_kind,
            command: ZedCommand::new(editor_kind.executable())
                .with_new_window_flag(editor_kind.new_window_flag()),
            poll_interval: Duration::from_millis(25),
            poll_timeout: DEFAULT_EXTERNAL_OPERATION_TIMEOUT,
        }
    }

    pub fn with_command(mut self, command: ZedCommand) -> Self {
        self.command = command;
        self
    }

    pub fn with_timing(mut self, poll_interval: Duration, poll_timeout: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.poll_timeout = poll_timeout;
        self
    }

    pub fn command(&self) -> &ZedCommand {
        &self.command
    }

    pub const fn editor_kind(&self) -> ProjectEditorKind {
        self.editor_kind
    }

    pub fn project_path_matches(&self, window: &EditorWindowSnapshot, project_path: &Path) -> bool {
        if !self
            .editor_kind
            .matches_application(window.application.as_str())
        {
            return false;
        }
        if let Some(observed_path) = window.project_path.as_deref() {
            return self.observed_project_path_matches(observed_path, project_path);
        }

        let Some(title) = window.title.as_deref() else {
            return false;
        };
        let Some(project_name) = project_path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if self
            .editor_kind
            .title_matches_project_name(title, project_name)
        {
            return true;
        }

        self.editor_kind.has_known_editor_suffix(title)
            && self
                .editor_kind
                .project_title_component(title)
                .and_then(|component| self.normalize_observed_project_path(Path::new(&component)))
                .is_some_and(|observed_path| observed_path == project_path)
    }

    pub fn matching_project_window(
        &self,
        windows: &[EditorWindowSnapshot],
        project_path: &Path,
    ) -> Result<Option<EditorWindowSnapshot>> {
        let mut matches = windows
            .iter()
            .filter(|window| {
                window.project_path.is_some() && self.project_path_matches(window, project_path)
            })
            .cloned()
            .collect::<Vec<_>>();
        if matches.is_empty() {
            matches = windows
                .iter()
                .filter(|window| {
                    window.project_path.is_none() && self.project_path_matches(window, project_path)
                })
                .cloned()
                .collect::<Vec<_>>();
        }
        match matches.as_slice() {
            [] => Ok(None),
            [window] => Ok(Some(window.clone())),
            _ => Err(ZedError::AmbiguousProject {
                editor: self.editor_kind.display_name().to_owned(),
                project: project_path.display().to_string(),
                matches: matches.len(),
            }
            .into_workstate()),
        }
    }

    fn observed_project_path_matches(&self, observed: &Path, requested: &Path) -> bool {
        if observed == requested {
            return true;
        }

        self.normalize_observed_project_path(observed)
            .is_some_and(|path| path == requested)
    }

    fn normalize_observed_project_path(&self, observed: &Path) -> Option<PathBuf> {
        let expanded = self.expand_project_path(observed).ok()?;
        self.file_system
            .canonicalize(&expanded)
            .ok()
            .or(Some(expanded))
    }

    pub fn resolve_project_path(&self, project_path: &Path) -> Result<PathBuf> {
        let project_path = self.expand_project_path(project_path)?;
        if !project_path.is_absolute() {
            return Err(ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: "the configured project path must be absolute".to_owned(),
            }
            .into_workstate());
        }
        let exists = self.file_system.exists(&project_path).map_err(|source| {
            operation_error(
                self.editor_kind.display_name(),
                "inspect-project-path",
                source,
            )
        })?;
        if !exists {
            return Err(ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: format!(
                    "the configured project path does not exist: {}",
                    project_path.display()
                ),
            }
            .into_workstate());
        }
        let directory = self
            .file_system
            .is_directory(&project_path)
            .map_err(|source| {
                operation_error(
                    self.editor_kind.display_name(),
                    "inspect-project-path",
                    source,
                )
            })?;
        if !directory {
            return Err(ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: format!(
                    "the configured project path is not a directory: {}",
                    project_path.display()
                ),
            }
            .into_workstate());
        }
        self.file_system
            .canonicalize(&project_path)
            .map_err(|source| {
                operation_error(
                    self.editor_kind.display_name(),
                    "resolve-project-path",
                    source,
                )
            })
    }

    fn expand_project_path(&self, project_path: &Path) -> Result<PathBuf> {
        if project_path.is_absolute() {
            return Ok(project_path.to_path_buf());
        }

        let Some(raw) = project_path.to_str() else {
            return Err(ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: "the configured project path must be absolute".to_owned(),
            }
            .into_workstate());
        };
        let is_home_relative =
            raw == "~" || raw.starts_with("~/") || raw == "$HOME" || raw.starts_with("$HOME/");
        if !is_home_relative {
            return Err(ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: "the configured project path must be absolute or start with ~/ or $HOME/"
                    .to_owned(),
            }
            .into_workstate());
        }

        let home = self.file_system.home_directory().map_err(|source| {
            operation_error(
                self.editor_kind.display_name(),
                "resolve-project-home",
                source,
            )
        })?;
        let resolver = PathResolver::new(home, self.file_system.as_ref()).map_err(|source| {
            operation_error(
                self.editor_kind.display_name(),
                "resolve-project-path",
                source,
            )
        })?;
        resolver.expand(raw).map_err(|error| {
            ZedError::InvalidProjectPath {
                editor: self.editor_kind.display_name().to_owned(),
                detail: error.message,
            }
            .into_workstate()
        })
    }

    async fn observe_editor_projects(&self) -> Result<Vec<EditorWindowSnapshot>> {
        let snapshot = self.desktop.snapshot().await?;
        Ok(snapshot
            .windows
            .into_iter()
            .filter_map(|window| {
                let application = window.application?;
                if !self.editor_kind.matches_application(&application) {
                    return None;
                }
                Some(EditorWindowSnapshot {
                    identity: window.identity,
                    application,
                    title: window.title,
                    project_path: window.project_path.map(PathBuf::from),
                    workspace_identity: window.workspace_identity,
                })
            })
            .collect())
    }

    async fn open_project_inner(
        &self,
        project_path: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<EditorOpenOutcome> {
        cancellation.check()?;
        let project_path = self.resolve_project_path(&project_path)?;
        let before = self.observe_editor_projects().await?;
        if let Some(window) = self.matching_project_window(&before, &project_path)? {
            return Ok(EditorOpenOutcome {
                status: EditorOperationStatus::Reused,
                window,
                owned: false,
                process_identity: None,
            });
        }

        let _launch_guard = self.launch_lock.lock().await;
        let before = self.observe_editor_projects().await?;
        if let Some(window) = self.matching_project_window(&before, &project_path)? {
            return Ok(EditorOpenOutcome {
                status: EditorOperationStatus::Reused,
                window,
                owned: false,
                process_identity: None,
            });
        }

        let process = self
            .runner
            .start_background(ProcessRequest {
                program: self.command.program().to_owned(),
                arguments: self.command.arguments(&project_path),
                working_directory: Some(project_path.clone()),
                environment: Vec::new(),
            })
            .await
            .map_err(|source| {
                operation_error(self.editor_kind.display_name(), "launch-project", source)
            })?;
        let before_ids = before
            .iter()
            .map(|window| window.identity.clone())
            .collect::<BTreeSet<_>>();
        let project = project_path.clone();
        let launched_at = self.clock.monotonic_now();
        let wait = async {
            loop {
                cancellation.check()?;
                if self.clock.elapsed_since(launched_at) >= self.poll_timeout {
                    return Err(ZedError::WindowTimeout {
                        editor: self.editor_kind.display_name().to_owned(),
                        project: project.display().to_string(),
                    }
                    .into_workstate());
                }
                let observed = self.observe_editor_projects().await?;
                if let Some(window) = self.matching_project_window(&observed, &project_path)? {
                    let owned = !before_ids.contains(&window.identity);
                    return Ok(EditorOpenOutcome {
                        status: if owned {
                            EditorOperationStatus::Launched
                        } else {
                            EditorOperationStatus::Reused
                        },
                        window,
                        owned,
                        process_identity: owned.then_some(process.identity.clone()),
                    });
                }
                let new_windows = observed
                    .iter()
                    .filter(|window| !before_ids.contains(&window.identity))
                    .cloned()
                    .collect::<Vec<_>>();
                if new_windows.len() == 1 {
                    let window = new_windows.into_iter().next().ok_or_else(|| {
                        ZedError::OperationFailed {
                            editor: self.editor_kind.display_name().to_owned(),
                            operation: "observe-launched-project".to_owned(),
                            detail: format!(
                                "the newly launched {} window disappeared before it could be recorded",
                                self.editor_kind.display_name()
                            ),
                        }
                        .into_workstate()
                    })?;
                    return Ok(EditorOpenOutcome {
                        status: EditorOperationStatus::Launched,
                        window,
                        owned: true,
                        process_identity: Some(process.identity.clone()),
                    });
                }
                if new_windows.len() > 1 {
                    return Err(ZedError::AmbiguousProject {
                        editor: self.editor_kind.display_name().to_owned(),
                        project: project.display().to_string(),
                        matches: new_windows.len(),
                    }
                    .into_workstate());
                }
                tokio::time::sleep(self.poll_interval).await;
            }
        };
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(ZedError::OperationFailed {
                editor: self.editor_kind.display_name().to_owned(),
                operation: "wait-for-project-window".to_owned(),
                detail: "the operation was cancelled".to_owned(),
            }.into_workstate()),
            result = tokio::time::timeout(self.poll_timeout, wait) => match result {
                Ok(result) => result,
                Err(_) => Err(ZedError::WindowTimeout {
                    editor: self.editor_kind.display_name().to_owned(),
                    project: project_path.display().to_string(),
                }.into_workstate()),
            },
        };
        if let Err(error) = result {
            let cleanup = self.runner.stop_background(process).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => {
                    Err(error.with_context("launched_process_cleanup", cleanup_error.render()))
                }
            };
        }
        result
    }
}

impl EditorBackend for ZedBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn observe_projects<'a>(&'a self) -> BoxFuture<'a, Result<Vec<EditorWindowSnapshot>>> {
        Box::pin(async move { self.observe_editor_projects().await })
    }

    fn open_project<'a>(
        &'a self,
        project_path: PathBuf,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<EditorOpenOutcome>> {
        Box::pin(async move { self.open_project_inner(project_path, cancellation).await })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<crate::application::ports::DesktopOperationOutcome>> {
        Box::pin(async move { self.desktop.close_window(window_identity).await })
    }
}

pub fn is_zed_application(application: &str) -> bool {
    ProjectEditorKind::Zed.matches_application(application)
}

fn operation_error(
    editor: &str,
    operation: &str,
    source: crate::error::WorkstateError,
) -> crate::error::WorkstateError {
    ZedError::OperationFailed {
        editor: editor.to_owned(),
        operation: operation.to_owned(),
        detail: source.render(),
    }
    .into_workstate()
}

#[cfg(test)]
mod tests {
    use super::{ProjectEditorKind, ZedCommand};
    use std::path::Path;

    #[test]
    fn project_editor_profiles_use_their_native_launch_commands() {
        assert_eq!(ProjectEditorKind::Zed.executable(), "zed");
        assert_eq!(ProjectEditorKind::VsCode.executable(), "code");
        assert_eq!(ProjectEditorKind::Cursor.executable(), "cursor");
        assert_eq!(ProjectEditorKind::Zed.new_window_flag(), "-n");
        assert_eq!(ProjectEditorKind::VsCode.new_window_flag(), "--new-window");
        assert_eq!(ProjectEditorKind::Cursor.new_window_flag(), "--new-window");
    }

    #[test]
    fn project_editor_profiles_match_common_linux_window_application_ids() {
        assert!(ProjectEditorKind::VsCode.matches_application("code"));
        assert!(ProjectEditorKind::VsCode.matches_application("com.visualstudio.code.desktop"));
        assert!(ProjectEditorKind::Cursor.matches_application("cursor.desktop"));
        assert!(ProjectEditorKind::Cursor.matches_application("com.todesktop.230313mzl4w4u92"));
        assert!(!ProjectEditorKind::Cursor.matches_application("code"));
    }

    #[test]
    fn project_editor_commands_can_override_the_window_flag() {
        let command = ZedCommand::new("code").with_new_window_flag("--new-window");
        assert_eq!(
            command.arguments(Path::new("/home/user/project")),
            vec!["--new-window".to_owned(), "/home/user/project".to_owned()]
        );
    }
}
