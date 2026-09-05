use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::ports::{
        android::EmulatorBackend,
        applications::ApplicationCatalog,
        clock::{Clock, SystemClock},
        containers::ContainerBackend,
        desktop::{DesktopBackend, DesktopSnapshot},
        directories::DirectoryCatalog,
        editor::EditorBackend,
        files::FileCatalog,
        filesystem::FileSystem,
        persistence::{ConfigStore, StateStore},
        platform::{PlatformDetector, PlatformProbe},
        process::{BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner},
        terminal::TerminalBackend,
        tmux::TmuxBackend,
    },
    application::{
        planner::{ActionHandlerRegistry, NoopReadinessCheckRunner, ReadinessCheckRunner},
        reconciliation::{LifecycleEngine, ReconciliationEngine, SchedulerOptions},
    },
    domain::{EnvironmentConfig, EnvironmentSlug, RuntimeState},
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::{
        filesystem::{LocalDirectoryCatalog, LocalFileCatalog, local::LocalFileSystem},
        persistence::{TomlConfigStore, TomlStateStore, WorkstatePaths},
        process::TokioProcessRunner,
    },
    integrations::{
        CosmicBackend, DockerProcessBackend, IntegrationRegistry, ProjectEditorKind,
        TmuxProcessBackend, ZedBackend,
        android::{AndroidBackend, AndroidTool, find_tool},
    },
    platform::{
        DesktopEnvironment, DetectedPlatform, Distribution, OperatingSystem, TerminalCapability,
    },
    platform::{
        detection::RuntimePlatformDetector,
        linux::{LinuxApplicationCatalog, SystemPlatformProbe},
    },
};

pub struct AppDependencies {
    pub config_store: Arc<dyn ConfigStore>,
    pub state_store: Arc<dyn StateStore>,
    pub file_system: Arc<dyn FileSystem>,
    pub process_runner: Arc<dyn ProcessRunner>,
    pub directory_catalog: Arc<dyn DirectoryCatalog>,
    pub file_catalog: Arc<dyn FileCatalog>,
    pub application_catalog: Arc<dyn ApplicationCatalog>,
    pub clock: Arc<dyn Clock>,
    pub platform_detector: Arc<dyn PlatformDetector>,
    pub desktop_backend: Arc<dyn DesktopBackend>,
    pub terminal_backend: Arc<dyn TerminalBackend>,
    pub tmux_backend: Arc<dyn TmuxBackend>,
    pub container_backend: Arc<dyn ContainerBackend>,
    pub editor_backend: Arc<dyn EditorBackend>,
    pub emulator_backend: Arc<dyn EmulatorBackend>,
    pub integration_registry: Arc<IntegrationRegistry>,
}

impl AppDependencies {
    pub fn with_noop_dependencies() -> Self {
        let platform = supported_test_platform();
        Self {
            config_store: Arc::new(UnavailableBackend::new("configuration store")),
            state_store: Arc::new(UnavailableBackend::new("state store")),
            file_system: Arc::new(UnavailableBackend::new("filesystem")),
            process_runner: Arc::new(UnavailableBackend::new("process runner")),
            directory_catalog: Arc::new(UnavailableBackend::new("directory catalog")),
            file_catalog: Arc::new(UnavailableBackend::new("file catalog")),
            application_catalog: Arc::new(UnavailableBackend::new("application catalog")),
            clock: Arc::new(SystemClock),
            platform_detector: Arc::new(StaticPlatformDetector {
                platform: platform.clone(),
            }),
            desktop_backend: Arc::new(UnavailableBackend::new("desktop backend")),
            terminal_backend: Arc::new(UnavailableBackend::new("terminal backend")),
            tmux_backend: Arc::new(UnavailableBackend::new("tmux backend")),
            container_backend: Arc::new(UnavailableBackend::new("container backend")),
            editor_backend: Arc::new(UnavailableBackend::new("editor backend")),
            emulator_backend: Arc::new(UnavailableBackend::new("emulator backend")),
            integration_registry: Arc::new(IntegrationRegistry::for_detected_platform(&platform)),
        }
    }
}

pub struct AppContext {
    config_store: Arc<dyn ConfigStore>,
    state_store: Arc<dyn StateStore>,
    file_system: Arc<dyn FileSystem>,
    process_runner: Arc<dyn ProcessRunner>,
    directory_catalog: Arc<dyn DirectoryCatalog>,
    file_catalog: Arc<dyn FileCatalog>,
    application_catalog: Arc<dyn ApplicationCatalog>,
    clock: Arc<dyn Clock>,
    platform_detector: Arc<dyn PlatformDetector>,
    desktop_backend: Arc<dyn DesktopBackend>,
    terminal_backend: Arc<dyn TerminalBackend>,
    tmux_backend: Arc<dyn TmuxBackend>,
    container_backend: Arc<dyn ContainerBackend>,
    editor_backend: Arc<dyn EditorBackend>,
    emulator_backend: Arc<dyn EmulatorBackend>,
    integration_registry: Arc<IntegrationRegistry>,
    action_handlers: Arc<ActionHandlerRegistry>,
    readiness_runner: Arc<dyn ReadinessCheckRunner>,
}

impl AppContext {
    pub fn new(dependencies: AppDependencies) -> Self {
        Self {
            config_store: dependencies.config_store,
            state_store: dependencies.state_store,
            file_system: dependencies.file_system,
            process_runner: dependencies.process_runner,
            directory_catalog: dependencies.directory_catalog,
            file_catalog: dependencies.file_catalog,
            application_catalog: dependencies.application_catalog,
            clock: dependencies.clock,
            platform_detector: dependencies.platform_detector,
            desktop_backend: dependencies.desktop_backend,
            terminal_backend: dependencies.terminal_backend,
            tmux_backend: dependencies.tmux_backend,
            container_backend: dependencies.container_backend,
            editor_backend: dependencies.editor_backend,
            emulator_backend: dependencies.emulator_backend,
            integration_registry: dependencies.integration_registry,
            action_handlers: Arc::new(ActionHandlerRegistry::new()),
            readiness_runner: Arc::new(NoopReadinessCheckRunner),
        }
    }

    pub fn with_noop_dependencies() -> Self {
        Self::new(AppDependencies::with_noop_dependencies())
    }

    pub fn with_config_root(mut self, root: PathBuf) -> Result<Self> {
        let paths = WorkstatePaths::from_root(root)?;
        self.config_store = Arc::new(TomlConfigStore::new(
            Arc::clone(&self.file_system),
            paths.clone(),
        ));
        self.state_store = Arc::new(TomlStateStore::new(Arc::clone(&self.file_system), paths));
        Ok(self)
    }

    pub fn with_action_handlers(mut self, handlers: Arc<ActionHandlerRegistry>) -> Self {
        self.action_handlers = handlers;
        self
    }

    pub fn with_readiness_runner(mut self, runner: Arc<dyn ReadinessCheckRunner>) -> Self {
        self.readiness_runner = runner;
        self
    }

    pub fn bootstrap() -> Result<Self> {
        initialize_tracing()?;
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let paths = WorkstatePaths::from_file_system(file_system.as_ref())?;
        let config_store: Arc<dyn ConfigStore> = Arc::new(TomlConfigStore::new(
            Arc::clone(&file_system),
            paths.clone(),
        ));
        let state_store: Arc<dyn StateStore> =
            Arc::new(TomlStateStore::new(Arc::clone(&file_system), paths));
        let directory_catalog: Arc<dyn DirectoryCatalog> =
            Arc::new(LocalDirectoryCatalog::new(Arc::clone(&file_system))?);
        let file_catalog: Arc<dyn FileCatalog> =
            Arc::new(LocalFileCatalog::new(Arc::clone(&file_system))?);
        let platform_detector = RuntimePlatformDetector::new(SystemPlatformProbe);
        let detected_platform = platform_detector.detect()?;
        let integration_registry =
            IntegrationRegistry::from_platform(&detected_platform, &SystemPlatformProbe)?;

        let process_runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner);
        let platform_probe = SystemPlatformProbe;
        let emulator_program = find_tool(&platform_probe, AndroidTool::Emulator)?
            .unwrap_or_else(|| PathBuf::from("emulator"));
        let adb_program =
            find_tool(&platform_probe, AndroidTool::Adb)?.unwrap_or_else(|| PathBuf::from("adb"));
        let docker_program = platform_probe
            .executable("docker")?
            .unwrap_or_else(|| PathBuf::from("docker"));
        let docker_desktop_program = platform_probe.executable("docker-desktop")?;
        let docker_compose_program = platform_probe.executable("docker-compose")?;
        let docker_process_backend = Arc::new(DockerProcessBackend::new_for_platform(
            Arc::clone(&process_runner),
            Arc::clone(&file_system),
            docker_program,
            docker_desktop_program,
            docker_compose_program,
            detected_platform.operating_system.is_linux(),
        )?);
        let container_backend: Arc<dyn ContainerBackend> = docker_process_backend.clone();
        let tmux_executable = match &detected_platform.terminal {
            TerminalCapability::Tmux { executable } => executable.clone(),
            _ => PathBuf::from("tmux"),
        };
        let tmux_process_backend = Arc::new(TmuxProcessBackend::new(
            Arc::clone(&process_runner),
            tmux_executable,
        )?);
        let tmux_backend: Arc<dyn TmuxBackend> = tmux_process_backend.clone();
        let application_catalog: Arc<dyn ApplicationCatalog> =
            if detected_platform.operating_system.is_linux() {
                Arc::new(LinuxApplicationCatalog::new())
            } else {
                Arc::new(UnavailableBackend::new("application catalog"))
            };
        let supported_desktop = detected_platform.operating_system.is_linux()
            && detected_platform.distribution.is_pop_os()
            && detected_platform.desktop_environment.is_cosmic();
        let (desktop_backend, editor_backend, emulator_backend, action_handlers) =
            if supported_desktop {
                let cosmic = Arc::new(CosmicBackend::new(Arc::clone(&process_runner)));
                let desktop: Arc<dyn DesktopBackend> = cosmic;
                let zed = Arc::new(ZedBackend::new(
                    Arc::clone(&process_runner),
                    Arc::clone(&desktop),
                    Arc::clone(&file_system),
                ));
                let vs_code = Arc::new(ZedBackend::for_editor(
                    Arc::clone(&process_runner),
                    Arc::clone(&desktop),
                    Arc::clone(&file_system),
                    ProjectEditorKind::VsCode,
                ));
                let cursor = Arc::new(ZedBackend::for_editor(
                    Arc::clone(&process_runner),
                    Arc::clone(&desktop),
                    Arc::clone(&file_system),
                    ProjectEditorKind::Cursor,
                ));
                let mut handlers = ActionHandlerRegistry::new();
                crate::integrations::cosmic::register_handlers(
                    &mut handlers,
                    Arc::clone(&desktop),
                )?;
                crate::integrations::application::register_handlers(
                    &mut handlers,
                    Arc::clone(&application_catalog),
                    Arc::clone(&desktop),
                    Arc::clone(&file_system),
                )?;
                crate::integrations::zed::register_handlers(
                    &mut handlers,
                    Arc::clone(&zed),
                    Arc::clone(&desktop),
                )?;
                crate::integrations::zed::register_editor_handler(
                    &mut handlers,
                    vs_code,
                    Arc::clone(&desktop),
                )?;
                crate::integrations::zed::register_editor_handler(
                    &mut handlers,
                    cursor,
                    Arc::clone(&desktop),
                )?;
                crate::integrations::command::register_handlers(
                    &mut handlers,
                    Arc::clone(&process_runner),
                    Arc::clone(&tmux_backend),
                    Arc::clone(&file_system),
                )?;
                crate::integrations::docker::register_handlers(
                    &mut handlers,
                    docker_process_backend.clone(),
                    Arc::clone(&file_system),
                )?;
                let android = Arc::new(AndroidBackend::new(
                    Arc::clone(&process_runner),
                    Arc::clone(&desktop),
                    emulator_program,
                    adb_program,
                )?);
                crate::integrations::android::emulator::register_handlers(
                    &mut handlers,
                    android.clone(),
                    Arc::clone(&desktop),
                )?;
                (
                    desktop,
                    zed as Arc<dyn EditorBackend>,
                    android as Arc<dyn EmulatorBackend>,
                    Arc::new(handlers),
                )
            } else {
                (
                    Arc::new(UnavailableBackend::new("desktop backend")) as Arc<dyn DesktopBackend>,
                    Arc::new(UnavailableBackend::new("editor backend")) as Arc<dyn EditorBackend>,
                    Arc::new(UnavailableBackend::new("emulator backend"))
                        as Arc<dyn EmulatorBackend>,
                    Arc::new(ActionHandlerRegistry::new()),
                )
            };

        let terminal_backend: Arc<dyn TerminalBackend> = tmux_process_backend;
        let context = Self::new(AppDependencies {
            config_store,
            state_store,
            file_system,
            process_runner,
            directory_catalog,
            file_catalog,
            application_catalog,
            clock: Arc::new(SystemClock),
            platform_detector: Arc::new(platform_detector),
            desktop_backend,
            terminal_backend,
            tmux_backend,
            container_backend,
            editor_backend,
            emulator_backend,
            integration_registry: Arc::new(integration_registry),
        });
        Ok(context.with_action_handlers(action_handlers))
    }

    pub fn config_store(&self) -> &dyn ConfigStore {
        self.config_store.as_ref()
    }

    pub fn state_store(&self) -> &dyn StateStore {
        self.state_store.as_ref()
    }

    pub fn file_system(&self) -> &dyn FileSystem {
        self.file_system.as_ref()
    }

    pub fn process_runner(&self) -> &dyn ProcessRunner {
        self.process_runner.as_ref()
    }

    pub fn directory_catalog(&self) -> &dyn DirectoryCatalog {
        self.directory_catalog.as_ref()
    }

    pub fn file_catalog(&self) -> &dyn FileCatalog {
        self.file_catalog.as_ref()
    }

    pub fn application_catalog(&self) -> &dyn ApplicationCatalog {
        self.application_catalog.as_ref()
    }

    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub fn platform_detector(&self) -> &dyn PlatformDetector {
        self.platform_detector.as_ref()
    }

    pub fn desktop_backend(&self) -> &dyn DesktopBackend {
        self.desktop_backend.as_ref()
    }

    pub fn terminal_backend(&self) -> &dyn TerminalBackend {
        self.terminal_backend.as_ref()
    }

    pub fn tmux_backend(&self) -> &dyn TmuxBackend {
        self.tmux_backend.as_ref()
    }

    pub fn container_backend(&self) -> &dyn ContainerBackend {
        self.container_backend.as_ref()
    }

    pub fn editor_backend(&self) -> &dyn EditorBackend {
        self.editor_backend.as_ref()
    }

    pub fn emulator_backend(&self) -> &dyn EmulatorBackend {
        self.emulator_backend.as_ref()
    }

    pub fn integration_registry(&self) -> &IntegrationRegistry {
        self.integration_registry.as_ref()
    }

    pub fn action_handler_registry(&self) -> Arc<ActionHandlerRegistry> {
        Arc::clone(&self.action_handlers)
    }

    pub fn readiness_runner(&self) -> Arc<dyn ReadinessCheckRunner> {
        Arc::clone(&self.readiness_runner)
    }

    pub fn reconciliation_engine(&self, options: SchedulerOptions) -> ReconciliationEngine<'_> {
        ReconciliationEngine::with_clock_and_desktop(
            self.integration_registry.as_ref(),
            Arc::clone(&self.action_handlers),
            Arc::clone(&self.readiness_runner),
            Arc::clone(&self.desktop_backend),
            Arc::clone(&self.clock),
            options,
        )
    }

    pub fn lifecycle_engine(&self, options: SchedulerOptions) -> LifecycleEngine<'_> {
        LifecycleEngine::new(
            self.integration_registry.as_ref(),
            Arc::clone(&self.action_handlers),
            Arc::clone(&self.readiness_runner),
            Arc::clone(&self.clock),
            Arc::clone(&self.config_store),
            Arc::clone(&self.state_store),
            options,
        )
        .with_desktop_backend(Arc::clone(&self.desktop_backend))
    }

    pub fn preflight(&self) -> Result<()> {
        let platform = self.platform_detector.detect()?;
        self.integration_registry.preflight(&platform)
    }
}

fn initialize_tracing() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::WARN.into())
        .from_env_lossy();

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .try_init()
        .map_err(|source| {
            WorkstateError::with_boxed_source(
                ErrorCategory::Runtime,
                "could not initialize diagnostic logging",
                source,
            )
        })
}

struct StaticPlatformDetector {
    platform: DetectedPlatform,
}

impl PlatformDetector for StaticPlatformDetector {
    fn detect(&self) -> Result<DetectedPlatform> {
        Ok(self.platform.clone())
    }
}

fn supported_test_platform() -> DetectedPlatform {
    DetectedPlatform {
        operating_system: OperatingSystem::Linux,
        distribution: Distribution::PopOs { version: None },
        desktop_environment: DesktopEnvironment::Cosmic,
        terminal: TerminalCapability::tmux(PathBuf::from("tmux")),
    }
}

struct UnavailableBackend {
    capability: &'static str,
}

impl UnavailableBackend {
    const fn new(capability: &'static str) -> Self {
        Self { capability }
    }

    fn error(&self, category: ErrorCategory, operation: &str) -> WorkstateError {
        WorkstateError::new(
            category,
            format!(
                "{capability} is not configured for this foundation context: {operation}",
                capability = self.capability
            ),
        )
    }
}

impl FileSystem for UnavailableBackend {
    fn home_directory(&self) -> Result<PathBuf> {
        Err(self.error(ErrorCategory::Persistence, "home directory lookup"))
    }

    fn exists(&self, _path: &Path) -> Result<bool> {
        Err(self.error(ErrorCategory::Persistence, "path lookup"))
    }

    fn is_directory(&self, _path: &Path) -> Result<bool> {
        Err(self.error(ErrorCategory::Persistence, "directory type lookup"))
    }

    fn create_directory_all(&self, _path: &Path) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "directory creation"))
    }

    fn list_directories(&self, _path: &Path) -> Result<Vec<PathBuf>> {
        Err(self.error(ErrorCategory::Persistence, "directory listing"))
    }

    fn list_files(&self, _path: &Path) -> Result<Vec<PathBuf>> {
        Err(self.error(ErrorCategory::Persistence, "file listing"))
    }

    fn read(&self, _path: &Path) -> Result<Vec<u8>> {
        Err(self.error(ErrorCategory::Persistence, "file read"))
    }

    fn write(&self, _path: &Path, _contents: &[u8]) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "file write"))
    }

    fn sync(&self, _path: &Path) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "file synchronization"))
    }

    fn rename(&self, _source: &Path, _target: &Path) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "file replacement"))
    }

    fn canonicalize(&self, _path: &Path) -> Result<PathBuf> {
        Err(self.error(ErrorCategory::Persistence, "path canonicalization"))
    }

    fn remove(&self, _path: &Path) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "path removal"))
    }
}

impl FileCatalog for UnavailableBackend {
    fn complete_yaml(
        &self,
        _working_directory: &str,
        _input: &str,
    ) -> Result<crate::application::ports::DirectoryCompletion> {
        Err(self.error(ErrorCategory::Persistence, "YAML file completion"))
    }
}

impl ConfigStore for UnavailableBackend {
    fn load(&self, _environment: &EnvironmentSlug) -> Result<Option<EnvironmentConfig>> {
        Err(self.error(ErrorCategory::Persistence, "configuration load"))
    }

    fn create(&self, _configuration: &EnvironmentConfig) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "configuration creation"))
    }

    fn save(&self, _configuration: &EnvironmentConfig) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "configuration save"))
    }

    fn delete(&self, _environment: &EnvironmentSlug) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "configuration deletion"))
    }

    fn list(&self) -> Result<Vec<EnvironmentSlug>> {
        Err(self.error(ErrorCategory::Persistence, "configuration listing"))
    }
}

impl StateStore for UnavailableBackend {
    fn load(&self, _environment: &EnvironmentSlug) -> Result<Option<RuntimeState>> {
        Err(self.error(ErrorCategory::Persistence, "runtime state load"))
    }

    fn save(&self, _state: &RuntimeState) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "runtime state save"))
    }

    fn delete(&self, _environment: &EnvironmentSlug) -> Result<()> {
        Err(self.error(ErrorCategory::Persistence, "runtime state deletion"))
    }
}

impl ProcessRunner for UnavailableBackend {
    fn run<'a>(&'a self, _request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        let error = self.error(ErrorCategory::Process, "process execution");
        Box::pin(async move { Err(error) })
    }
}

impl ApplicationCatalog for UnavailableBackend {
    fn list(&self) -> Result<Vec<crate::application::ports::InstalledApplication>> {
        Err(self.error(ErrorCategory::Platform, "application discovery"))
    }
}

impl DirectoryCatalog for UnavailableBackend {
    fn complete(&self, _input: &str) -> Result<crate::application::ports::DirectoryCompletion> {
        Err(self.error(ErrorCategory::Platform, "directory completion"))
    }
}

impl PlatformDetector for UnavailableBackend {
    fn detect(&self) -> Result<DetectedPlatform> {
        Err(self.error(ErrorCategory::Platform, "platform detection"))
    }
}

impl DesktopBackend for UnavailableBackend {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>> {
        let error = self.error(ErrorCategory::Platform, "desktop observation");
        Box::pin(async move { Err(error) })
    }
}

impl TerminalBackend for UnavailableBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(false)
    }
}

impl TmuxBackend for UnavailableBackend {
    fn observe<'a>(
        &'a self,
    ) -> crate::application::ports::BoxFuture<
        'a,
        Result<Vec<crate::application::ports::TmuxSessionSnapshot>>,
    > {
        let error = self.error(ErrorCategory::Integration, "tmux observation");
        Box::pin(async move { Err(error) })
    }

    fn create_session<'a>(
        &'a self,
        _session_name: &'a str,
        _window: crate::application::ports::TmuxWindowRequest,
    ) -> crate::application::ports::BoxFuture<
        'a,
        Result<crate::application::ports::TmuxSessionSnapshot>,
    > {
        let error = self.error(ErrorCategory::Integration, "tmux session creation");
        Box::pin(async move { Err(error) })
    }

    fn create_window<'a>(
        &'a self,
        _session_name: &'a str,
        _window: crate::application::ports::TmuxWindowRequest,
    ) -> crate::application::ports::BoxFuture<
        'a,
        Result<crate::application::ports::TmuxSessionSnapshot>,
    > {
        let error = self.error(ErrorCategory::Integration, "tmux window creation");
        Box::pin(async move { Err(error) })
    }

    fn kill_window<'a>(
        &'a self,
        _session_name: &'a str,
        _window_identity: &'a str,
    ) -> crate::application::ports::BoxFuture<'a, Result<()>> {
        let error = self.error(ErrorCategory::Integration, "tmux window cleanup");
        Box::pin(async move { Err(error) })
    }

    fn kill_session<'a>(
        &'a self,
        _session_name: &'a str,
    ) -> crate::application::ports::BoxFuture<'a, Result<()>> {
        let error = self.error(ErrorCategory::Integration, "tmux session cleanup");
        Box::pin(async move { Err(error) })
    }
}

impl ContainerBackend for UnavailableBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(false)
    }
}

impl EditorBackend for UnavailableBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(false)
    }
}

impl EmulatorBackend for UnavailableBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(false)
    }
}
