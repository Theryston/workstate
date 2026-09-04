use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime},
};

use crate::{
    application::ports::{
        clock::Clock,
        containers::ContainerBackend,
        desktop::{DesktopBackend, DesktopSnapshot},
        editor::EditorBackend,
        emulator::EmulatorBackend,
        filesystem::FileSystem,
        persistence::{ConfigStore, StateStore},
        platform::PlatformDetector,
        process::{BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner},
        terminal::TerminalBackend,
    },
    domain::{EnvironmentConfig, EnvironmentSlug, RuntimeState},
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::{
        filesystem::local::LocalFileSystem,
        persistence::{TomlConfigStore, TomlStateStore, WorkstatePaths},
    },
    integrations::IntegrationRegistry,
    platform::PlatformInfo,
};

pub struct AppDependencies {
    pub config_store: Arc<dyn ConfigStore>,
    pub state_store: Arc<dyn StateStore>,
    pub file_system: Arc<dyn FileSystem>,
    pub process_runner: Arc<dyn ProcessRunner>,
    pub clock: Arc<dyn Clock>,
    pub platform_detector: Arc<dyn PlatformDetector>,
    pub desktop_backend: Arc<dyn DesktopBackend>,
    pub terminal_backend: Arc<dyn TerminalBackend>,
    pub container_backend: Arc<dyn ContainerBackend>,
    pub editor_backend: Arc<dyn EditorBackend>,
    pub emulator_backend: Arc<dyn EmulatorBackend>,
    pub integration_registry: Arc<IntegrationRegistry>,
}

impl AppDependencies {
    pub fn with_noop_dependencies() -> Self {
        Self {
            config_store: Arc::new(UnavailableBackend::new("configuration store")),
            state_store: Arc::new(UnavailableBackend::new("state store")),
            file_system: Arc::new(UnavailableBackend::new("filesystem")),
            process_runner: Arc::new(UnavailableBackend::new("process runner")),
            clock: Arc::new(SystemClock),
            platform_detector: Arc::new(UnavailableBackend::new("platform detector")),
            desktop_backend: Arc::new(UnavailableBackend::new("desktop backend")),
            terminal_backend: Arc::new(UnavailableBackend::new("terminal backend")),
            container_backend: Arc::new(UnavailableBackend::new("container backend")),
            editor_backend: Arc::new(UnavailableBackend::new("editor backend")),
            emulator_backend: Arc::new(UnavailableBackend::new("emulator backend")),
            integration_registry: Arc::new(IntegrationRegistry::new()),
        }
    }
}

pub struct AppContext {
    config_store: Arc<dyn ConfigStore>,
    state_store: Arc<dyn StateStore>,
    file_system: Arc<dyn FileSystem>,
    process_runner: Arc<dyn ProcessRunner>,
    clock: Arc<dyn Clock>,
    platform_detector: Arc<dyn PlatformDetector>,
    desktop_backend: Arc<dyn DesktopBackend>,
    terminal_backend: Arc<dyn TerminalBackend>,
    container_backend: Arc<dyn ContainerBackend>,
    editor_backend: Arc<dyn EditorBackend>,
    emulator_backend: Arc<dyn EmulatorBackend>,
    integration_registry: Arc<IntegrationRegistry>,
}

impl AppContext {
    pub fn new(dependencies: AppDependencies) -> Self {
        Self {
            config_store: dependencies.config_store,
            state_store: dependencies.state_store,
            file_system: dependencies.file_system,
            process_runner: dependencies.process_runner,
            clock: dependencies.clock,
            platform_detector: dependencies.platform_detector,
            desktop_backend: dependencies.desktop_backend,
            terminal_backend: dependencies.terminal_backend,
            container_backend: dependencies.container_backend,
            editor_backend: dependencies.editor_backend,
            emulator_backend: dependencies.emulator_backend,
            integration_registry: dependencies.integration_registry,
        }
    }

    pub fn with_noop_dependencies() -> Self {
        Self::new(AppDependencies::with_noop_dependencies())
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

        Ok(Self::new(AppDependencies {
            config_store,
            state_store,
            file_system,
            process_runner: Arc::new(UnavailableBackend::new("process runner")),
            clock: Arc::new(SystemClock),
            platform_detector: Arc::new(UnavailableBackend::new("platform detector")),
            desktop_backend: Arc::new(UnavailableBackend::new("desktop backend")),
            terminal_backend: Arc::new(UnavailableBackend::new("terminal backend")),
            container_backend: Arc::new(UnavailableBackend::new("container backend")),
            editor_backend: Arc::new(UnavailableBackend::new("editor backend")),
            emulator_backend: Arc::new(UnavailableBackend::new("emulator backend")),
            integration_registry: Arc::new(IntegrationRegistry::new()),
        }))
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

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn monotonic_now(&self) -> Instant {
        Instant::now()
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

impl PlatformDetector for UnavailableBackend {
    fn detect(&self) -> Result<PlatformInfo> {
        Err(self.error(ErrorCategory::Platform, "platform detection"))
    }
}

impl DesktopBackend for UnavailableBackend {
    fn snapshot(&self) -> Result<DesktopSnapshot> {
        Err(self.error(ErrorCategory::Platform, "desktop observation"))
    }
}

impl TerminalBackend for UnavailableBackend {
    fn is_available(&self) -> Result<bool> {
        Ok(false)
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
