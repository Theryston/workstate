#[path = "fakes/mod.rs"]
mod fakes;

use std::{
    collections::VecDeque,
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tempfile::tempdir;
use workstate::{
    application::{
        planner::{ActionHandler, CancellationToken, ObservationStatus},
        ports::{
            BackgroundProcess, BoxFuture, DesktopBackend, DesktopOperationOutcome, DesktopSnapshot,
            DesktopWindowSnapshot, DesktopWorkspaceSnapshot, EditorBackend, FileSystem,
            ProcessOutput, ProcessRequest, ProcessRunner, ensure_workspace,
            resolve_workspace_target,
        },
        reconciliation::{InMemoryEventSink, ReconciliationEngine, RunRequest, SchedulerOptions},
    },
    domain::{
        ActionKind, ActionSpec, EnvironmentConfig, OwnershipStatus, ResourceIdentity, ResourceKind,
        ResourceRecord, TilingPreference, Timeout, WorkspaceId, WorkspaceReference, WorkspaceSpec,
        WorkspaceTarget,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::filesystem::local::LocalFileSystem,
    integrations::{
        CosmicBackend,
        cosmic::WorkspaceHandler,
        zed::{ZedBackend, ZedCommand, ZedProjectHandler},
    },
};

use fakes::fake_desktop::FakeDesktop;

type TestResult = std::result::Result<(), Box<dyn Error>>;

#[derive(Clone)]
struct HomeOverrideFileSystem {
    home: PathBuf,
    local: LocalFileSystem,
}

impl HomeOverrideFileSystem {
    fn new(home: PathBuf) -> Self {
        Self {
            home,
            local: LocalFileSystem,
        }
    }
}

impl FileSystem for HomeOverrideFileSystem {
    fn home_directory(&self) -> Result<PathBuf> {
        Ok(self.home.clone())
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        self.local.exists(path)
    }

    fn is_directory(&self, path: &Path) -> Result<bool> {
        self.local.is_directory(path)
    }

    fn create_directory_all(&self, path: &Path) -> Result<()> {
        self.local.create_directory_all(path)
    }

    fn list_directories(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.local.list_directories(path)
    }

    fn list_files(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.local.list_files(path)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.local.read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> Result<()> {
        self.local.write(path, contents)
    }

    fn sync(&self, path: &Path) -> Result<()> {
        self.local.sync(path)
    }

    fn rename(&self, source: &Path, target: &Path) -> Result<()> {
        self.local.rename(source, target)
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        self.local.canonicalize(path)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.local.remove(path)
    }
}

#[derive(Clone)]
struct FixtureProcessRunner {
    workspaces: Vec<u8>,
    windows: Vec<u8>,
    calls: Arc<Mutex<Vec<ProcessRequest>>>,
    launch: Option<(FakeDesktop, PathBuf)>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl FixtureProcessRunner {
    fn for_cosmic(workspaces: &[u8], windows: &[u8]) -> Self {
        Self {
            workspaces: workspaces.to_vec(),
            windows: windows.to_vec(),
            calls: Arc::new(Mutex::new(Vec::new())),
            launch: None,
            stopped: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn for_zed_launch(desktop: FakeDesktop, project_path: PathBuf) -> Self {
        Self {
            workspaces: Vec::new(),
            windows: Vec::new(),
            calls: Arc::new(Mutex::new(Vec::new())),
            launch: Some((desktop, project_path)),
            stopped: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn for_zed_launch_without_project_metadata(desktop: FakeDesktop) -> Self {
        Self::for_zed_launch(desktop, PathBuf::new())
    }

    fn calls(&self) -> Result<Vec<ProcessRequest>> {
        self.calls.lock().map(|calls| calls.clone()).map_err(|_| {
            WorkstateError::new(ErrorCategory::Runtime, "fake process call lock failed")
        })
    }

    fn stopped(&self) -> Result<Vec<String>> {
        self.stopped
            .lock()
            .map(|stopped| stopped.clone())
            .map_err(|_| {
                WorkstateError::new(ErrorCategory::Runtime, "fake process stop lock failed")
            })
    }
}

impl ProcessRunner for FixtureProcessRunner {
    fn run<'a>(&'a self, request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        let calls = Arc::clone(&self.calls);
        let workspaces = self.workspaces.clone();
        let windows = self.windows.clone();
        Box::pin(async move {
            calls
                .lock()
                .map(|mut calls| calls.push(request.clone()))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake process call lock failed")
                })?;
            let stdout = if request
                .arguments
                .iter()
                .any(|argument| argument == "get-workspaces")
            {
                workspaces
            } else {
                windows
            };
            Ok(ProcessOutput {
                status: Some(0),
                stdout,
                stderr: Vec::new(),
            })
        })
    }

    fn start_background<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        let calls = Arc::clone(&self.calls);
        let launch = self.launch.clone();
        Box::pin(async move {
            calls
                .lock()
                .map(|mut calls| calls.push(request))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake process call lock failed")
                })?;
            if let Some((desktop, project_path)) = launch {
                desktop.add_window(DesktopWindowSnapshot {
                    identity: "zed-launched".to_owned(),
                    application: Some("dev.zed.Zed".to_owned()),
                    title: Some("Launched project".to_owned()),
                    project_path: (!project_path.as_os_str().is_empty())
                        .then(|| project_path.display().to_string()),
                    workspace_identity: Some("main".to_owned()),
                    focused: false,
                })?;
            }
            BackgroundProcess::new("fake-zed-process")
        })
    }

    fn stop_background<'a>(&'a self, process: BackgroundProcess) -> BoxFuture<'a, Result<()>> {
        let stopped = Arc::clone(&self.stopped);
        Box::pin(async move {
            stopped
                .lock()
                .map(|mut stopped| stopped.push(process.identity))
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "fake process stop lock failed")
                })
        })
    }
}

#[derive(Clone)]
struct ConcurrentZedLaunchRunner {
    desktop: FakeDesktop,
    active_launches: Arc<AtomicUsize>,
    max_active_launches: Arc<AtomicUsize>,
    sequence: Arc<AtomicUsize>,
}

impl ConcurrentZedLaunchRunner {
    fn new(desktop: FakeDesktop) -> Self {
        Self {
            desktop,
            active_launches: Arc::new(AtomicUsize::new(0)),
            max_active_launches: Arc::new(AtomicUsize::new(0)),
            sequence: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn max_active_launches(&self) -> usize {
        self.max_active_launches.load(Ordering::SeqCst)
    }
}

impl ProcessRunner for ConcurrentZedLaunchRunner {
    fn run<'a>(&'a self, _request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        Box::pin(async {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        })
    }

    fn start_background<'a>(
        &'a self,
        _request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        let desktop = self.desktop.clone();
        let active_launches = Arc::clone(&self.active_launches);
        let max_active_launches = Arc::clone(&self.max_active_launches);
        let sequence = Arc::clone(&self.sequence);
        Box::pin(async move {
            let active = active_launches.fetch_add(1, Ordering::SeqCst) + 1;
            max_active_launches.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            let index = sequence.fetch_add(1, Ordering::SeqCst);
            let add_result = desktop.add_window(DesktopWindowSnapshot {
                identity: format!("zed-launched-{index}"),
                application: Some("dev.zed.Zed".to_owned()),
                title: Some("Launched project".to_owned()),
                project_path: None,
                workspace_identity: Some("main".to_owned()),
                focused: false,
            });
            tokio::task::yield_now().await;
            active_launches.fetch_sub(1, Ordering::SeqCst);
            add_result?;
            BackgroundProcess::new(format!("fake-zed-process-{index}"))
        })
    }

    fn stop_background<'a>(&'a self, _process: BackgroundProcess) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct RefreshingDesktop {
    snapshots: Arc<Mutex<VecDeque<DesktopSnapshot>>>,
}

impl RefreshingDesktop {
    fn new(snapshots: Vec<DesktopSnapshot>) -> Self {
        Self {
            snapshots: Arc::new(Mutex::new(snapshots.into_iter().collect())),
        }
    }
}

impl DesktopBackend for RefreshingDesktop {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>> {
        let snapshots = Arc::clone(&self.snapshots);
        Box::pin(async move {
            snapshots
                .lock()
                .map_err(|_| {
                    WorkstateError::new(ErrorCategory::Runtime, "refreshing desktop lock failed")
                })?
                .pop_front()
                .ok_or_else(|| {
                    WorkstateError::new(
                        ErrorCategory::Runtime,
                        "refreshing desktop has no snapshot left",
                    )
                })
        })
    }
}

fn workspace(
    identity: &str,
    name: &str,
    position: u32,
    focused: bool,
    tiling_enabled: bool,
) -> DesktopWorkspaceSnapshot {
    DesktopWorkspaceSnapshot {
        identity: identity.to_owned(),
        name: Some(name.to_owned()),
        position: Some(position),
        focused,
        tiling_enabled: Some(tiling_enabled),
    }
}

fn snapshot_with_workspaces(workspaces: Vec<DesktopWorkspaceSnapshot>) -> DesktopSnapshot {
    DesktopSnapshot {
        workspaces,
        windows: Vec::new(),
    }
}

#[test]
fn workspace_resolution_is_exact_and_deterministic() -> TestResult {
    let snapshot = DesktopSnapshot {
        workspaces: vec![
            workspace("main", "Main", 0, true, true),
            workspace("code", "Code", 1, false, false),
            workspace("empty", "Empty", 2, false, false),
        ],
        windows: vec![DesktopWindowSnapshot {
            identity: "terminal".to_owned(),
            application: Some("terminal".to_owned()),
            title: Some("Shell".to_owned()),
            project_path: None,
            workspace_identity: Some("code".to_owned()),
            focused: false,
        }],
    };
    let exact = resolve_workspace_target(
        &snapshot,
        &WorkspaceTarget::Existing {
            reference: WorkspaceReference::Name("Code".to_owned()),
        },
    )?;
    assert_eq!(
        exact.workspace.as_ref().map(|item| item.identity.as_str()),
        Some("code")
    );
    let next_empty = resolve_workspace_target(&snapshot, &WorkspaceTarget::NextEmpty)?;
    assert_eq!(
        next_empty
            .workspace
            .as_ref()
            .map(|item| item.identity.as_str()),
        Some("main")
    );
    let none = resolve_workspace_target(&snapshot, &WorkspaceTarget::None)?;
    assert!(none.workspace.is_none());
    Ok(())
}

#[test]
fn duplicate_workspace_names_are_rejected() -> TestResult {
    let snapshot = snapshot_with_workspaces(vec![
        workspace("one", "Code", 0, true, false),
        workspace("two", "Code", 1, false, false),
    ]);
    let result = resolve_workspace_target(
        &snapshot,
        &WorkspaceTarget::Existing {
            reference: WorkspaceReference::Name("Code".to_owned()),
        },
    );
    assert!(result.is_err());
    let error = result.err().ok_or("missing ambiguity error")?;
    assert!(error.message.contains("ambiguous"));
    Ok(())
}

#[test]
fn current_workspace_uses_the_focused_window() -> TestResult {
    let snapshot = DesktopSnapshot {
        workspaces: vec![
            workspace("main", "Main", 0, true, true),
            workspace("code", "Code", 1, false, false),
        ],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-window".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Code".to_owned()),
            project_path: None,
            workspace_identity: Some("code".to_owned()),
            focused: true,
        }],
    };
    let current = resolve_workspace_target(&snapshot, &WorkspaceTarget::Current)?;
    assert_eq!(
        current
            .workspace
            .as_ref()
            .map(|item| item.identity.as_str()),
        Some("code")
    );
    Ok(())
}

#[tokio::test]
async fn create_workspace_is_observed_and_marked_owned() -> TestResult {
    let desktop = FakeDesktop::new(snapshot_with_workspaces(vec![workspace(
        "main", "Main", 0, true, true,
    )]));
    let resolution = ensure_workspace(
        &desktop,
        WorkspaceTarget::Create {
            name: "Services".to_owned(),
        },
        CancellationToken::new(),
        Duration::from_millis(100),
    )
    .await?;
    assert_eq!(
        resolution.status,
        workstate::application::ports::DesktopOperationStatus::Created
    );
    assert_eq!(
        resolution
            .workspace
            .as_ref()
            .map(|item| item.identity.as_str()),
        Some("fake-services")
    );
    assert_eq!(desktop.calls()?, vec!["create-workspace:Services"]);
    Ok(())
}

#[tokio::test]
async fn missing_workspace_is_refreshed_once_before_failing_or_succeeding() -> TestResult {
    let initial = snapshot_with_workspaces(Vec::new());
    let refreshed =
        snapshot_with_workspaces(vec![workspace("services", "Services", 0, false, false)]);
    let desktop = RefreshingDesktop::new(vec![initial, refreshed]);
    let resolution = ensure_workspace(
        &desktop,
        WorkspaceTarget::Existing {
            reference: WorkspaceReference::Name("Services".to_owned()),
        },
        CancellationToken::new(),
        Duration::from_millis(100),
    )
    .await?;
    assert_eq!(
        resolution
            .workspace
            .as_ref()
            .map(|item| item.identity.as_str()),
        Some("services")
    );
    Ok(())
}

#[test]
fn cosmic_output_is_parsed_at_one_typed_boundary() -> TestResult {
    let runner = FixtureProcessRunner::for_cosmic(
        include_bytes!("fixtures/cosmic/workspaces.json"),
        include_bytes!("fixtures/cosmic/windows.json"),
    );
    let backend = CosmicBackend::new(Arc::new(runner.clone()));
    let snapshot = tokio::runtime::Runtime::new()?.block_on(backend.observe())?;
    assert_eq!(snapshot.workspaces.len(), 3);
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(snapshot.windows[0].workspace_identity.as_deref(), Some("2"));
    let calls = runner.calls()?;
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .any(|call| call.arguments == ["--json", "get-workspaces"])
    );
    assert!(
        calls
            .iter()
            .any(|call| call.arguments == ["--json", "get-toplevels"])
    );
    Ok(())
}

#[test]
fn unknown_cosmic_tiling_does_not_reject_the_workspace_snapshot() -> TestResult {
    let snapshot = workstate::integrations::cosmic::models::decode_snapshot(
        include_bytes!("fixtures/cosmic/workspaces_unknown_tiling.json"),
        br#"[]"#,
    )?;
    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(snapshot.workspaces[0].tiling_enabled, None);
    Ok(())
}

#[test]
fn empty_cosmic_window_application_is_treated_as_missing() -> TestResult {
    let snapshot = workstate::integrations::cosmic::models::decode_snapshot(
        include_bytes!("fixtures/cosmic/workspaces.json"),
        br#"[
            {
                "identifier": "window-without-application",
                "app_id": "",
                "title": "Desktop surface",
                "state": [],
                "workspaces": ["1"]
            }
        ]"#,
    )?;
    assert_eq!(snapshot.windows[0].application, None);
    Ok(())
}

#[tokio::test]
async fn zed_expands_home_relative_project_paths_before_validation() -> TestResult {
    let home = tempdir()?;
    let file_system = HomeOverrideFileSystem::new(home.path().to_path_buf());
    let project_path = home.path().join("Projects/blog");
    file_system.create_directory_all(&project_path)?;
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: Vec::new(),
    });
    let runner = FixtureProcessRunner::for_zed_launch(desktop.clone(), project_path.clone());
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(file_system),
    )
    .with_command(ZedCommand::new("zed-test"))
    .with_timing(Duration::from_millis(1), Duration::from_millis(100));

    let outcome = backend
        .open_project(PathBuf::from("~/Projects/blog"), CancellationToken::new())
        .await?;
    assert!(outcome.owned);
    let calls = runner.calls()?;
    let request = calls.first().ok_or("missing Zed launch request")?;
    assert_eq!(
        request.arguments,
        vec!["-n".to_owned(), project_path.display().to_string()]
    );
    assert_eq!(request.working_directory, Some(project_path));
    Ok(())
}

#[test]
fn malformed_cosmic_output_is_rejected() -> TestResult {
    let result = workstate::integrations::cosmic::models::decode_snapshot(
        include_bytes!("fixtures/cosmic/malformed.json"),
        include_bytes!("fixtures/cosmic/windows.json"),
    );
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn zed_reuses_only_a_stable_project_identity() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-existing".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Example".to_owned()),
            project_path: Some(project_path.display().to_string()),
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let runner = FixtureProcessRunner::for_zed_launch(desktop.clone(), project_path.clone());
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(LocalFileSystem),
    )
    .with_command(ZedCommand::new("zed-test"));
    let outcome = backend
        .open_project(project_path, CancellationToken::new())
        .await?;
    assert!(!outcome.owned);
    assert_eq!(
        outcome.status,
        workstate::application::ports::EditorOperationStatus::Reused
    );
    assert!(runner.calls()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn zed_marks_a_new_window_owned_only_after_observation() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: Vec::new(),
    });
    let runner = FixtureProcessRunner::for_zed_launch(desktop.clone(), project_path.clone());
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(LocalFileSystem),
    )
    .with_command(ZedCommand::new("zed-test"))
    .with_timing(Duration::from_millis(1), Duration::from_millis(100));
    let outcome = backend
        .open_project(project_path.clone(), CancellationToken::new())
        .await?;
    assert!(outcome.owned);
    assert_eq!(
        outcome.status,
        workstate::application::ports::EditorOperationStatus::Launched
    );
    let calls = runner.calls()?;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, "zed-test");
    assert_eq!(
        calls[0].arguments,
        vec!["-n".to_owned(), project_path.display().to_string()]
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_zed_projects_are_correlated_by_their_project_key() -> TestResult {
    let directory = tempdir()?;
    let first_project = directory.path().join("api");
    let second_project = directory.path().join("app");
    std::fs::create_dir_all(&first_project)?;
    std::fs::create_dir_all(&second_project)?;
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: Vec::new(),
    });
    let runner = ConcurrentZedLaunchRunner::new(desktop.clone());
    let backend = Arc::new(
        ZedBackend::new(
            Arc::new(runner.clone()),
            Arc::new(desktop),
            Arc::new(LocalFileSystem),
        )
        .with_command(ZedCommand::new("zed-test"))
        .with_timing(Duration::from_millis(1), Duration::from_millis(250)),
    );

    let first_future =
        EditorBackend::open_project(backend.as_ref(), first_project, CancellationToken::new());
    let second_future =
        EditorBackend::open_project(backend.as_ref(), second_project, CancellationToken::new());
    let (first, second) = tokio::join!(first_future, second_future);
    let first = first?;
    let second = second?;

    assert!(first.owned);
    assert!(second.owned);
    assert_ne!(first.window.identity, second.window.identity);
    assert_eq!(runner.max_active_launches(), 1);
    Ok(())
}

#[tokio::test]
async fn zed_correlates_a_new_window_without_project_metadata() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: Vec::new(),
    });
    let runner = FixtureProcessRunner::for_zed_launch_without_project_metadata(desktop.clone());
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(LocalFileSystem),
    )
    .with_timing(Duration::from_millis(1), Duration::from_millis(100));
    let outcome = backend
        .open_project(project_path, CancellationToken::new())
        .await?;
    assert!(outcome.owned);
    assert_eq!(outcome.window.identity, "zed-launched");
    assert_eq!(
        outcome.status,
        workstate::application::ports::EditorOperationStatus::Launched
    );
    Ok(())
}

#[tokio::test]
async fn a_title_collision_does_not_trigger_zed_reuse() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-title-only".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some(project_path.display().to_string()),
            project_path: None,
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let runner = FixtureProcessRunner::for_zed_launch(desktop.clone(), project_path.clone());
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(LocalFileSystem),
    )
    .with_timing(Duration::from_millis(1), Duration::from_millis(100));
    let outcome = backend
        .open_project(project_path, CancellationToken::new())
        .await?;
    assert!(outcome.owned);
    assert_eq!(runner.calls()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn tiling_handler_records_and_restores_a_mutation() -> TestResult {
    let desktop = FakeDesktop::new(snapshot_with_workspaces(vec![workspace(
        "code", "Code", 0, true, false,
    )]));
    let mut action = ActionSpec::new("tile-code", ActionKind::ConfigureTiling)?;
    action.desktop_workspace = Some(WorkspaceId::new("code")?);
    action.resolved_workspace_target = Some(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier("code".to_owned()),
    });
    action.resolved_tiling = Some(TilingPreference::Enabled);
    let handler = WorkspaceHandler::tiling(Arc::new(desktop.clone()));
    let observation = handler.observe(&action, CancellationToken::new()).await?;
    assert_eq!(observation.status, ObservationStatus::RequiresChange);
    let result = handler.apply(&action, CancellationToken::new()).await?;
    assert!(result.changed);
    assert_eq!(result.mutations.len(), 1);
    assert_eq!(
        desktop
            .state()?
            .workspace("code")
            .and_then(|item| item.tiling_enabled),
        Some(true)
    );
    handler
        .compensate(&action, &result, CancellationToken::new())
        .await?;
    assert_eq!(
        desktop
            .state()?
            .workspace("code")
            .and_then(|item| item.tiling_enabled),
        Some(false)
    );
    Ok(())
}

#[tokio::test]
async fn already_enabled_tiling_creates_no_mutation() -> TestResult {
    let desktop = FakeDesktop::new(snapshot_with_workspaces(vec![workspace(
        "code", "Code", 0, true, true,
    )]));
    let mut action = ActionSpec::new("tile-code", ActionKind::ConfigureTiling)?;
    action.desktop_workspace = Some(WorkspaceId::new("code")?);
    action.resolved_workspace_target = Some(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier("code".to_owned()),
    });
    action.resolved_tiling = Some(TilingPreference::Enabled);
    let handler = WorkspaceHandler::tiling(Arc::new(desktop.clone()));
    let observation = handler.observe(&action, CancellationToken::new()).await?;
    assert_eq!(observation.status, ObservationStatus::AlreadyCorrect);
    let result = handler.apply(&action, CancellationToken::new()).await?;
    assert!(!result.changed);
    assert!(result.mutations.is_empty());
    assert!(desktop.calls()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn zed_handler_moves_a_launched_window_and_rolls_it_back() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![
            workspace("main", "Main", 0, true, true),
            workspace("code", "Code", 1, false, false),
        ],
        windows: Vec::new(),
    });
    let runner = FixtureProcessRunner::for_zed_launch(desktop.clone(), project_path.clone());
    let editor = Arc::new(ZedBackend::new(
        Arc::new(runner),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop.clone()));
    desktop.fail_next_move();
    let mut action = ActionSpec::new("open-code", ActionKind::OpenProject)?;
    action.parameters.application = Some("dev.zed.Zed".to_owned());
    action.parameters.project_path = Some(project_path.display().to_string());
    action.desktop_workspace = Some(WorkspaceId::new("code")?);
    action.resolved_workspace_target = Some(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier("code".to_owned()),
    });
    let result = handler.apply(&action, CancellationToken::new()).await?;
    assert!(result.changed);
    assert_eq!(result.mutations.len(), 1);
    assert_eq!(
        desktop
            .state()?
            .window("zed-launched")
            .and_then(|item| item.workspace_identity.clone()),
        Some("code".to_owned())
    );
    handler
        .compensate(&action, &result, CancellationToken::new())
        .await?;
    assert!(desktop.state()?.window("zed-launched").is_none());
    Ok(())
}

#[tokio::test]
async fn reused_zed_windows_are_not_moved_back_after_manual_relocation() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![
            workspace("main", "Main", 0, true, true),
            workspace("code", "Code", 1, false, false),
        ],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-existing".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Code".to_owned()),
            project_path: Some(project_path.display().to_string()),
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let runner = FixtureProcessRunner::for_cosmic(&[], &[]);
    let editor = Arc::new(ZedBackend::new(
        Arc::new(runner),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop.clone()));
    let mut action = ActionSpec::new("reuse-code", ActionKind::OpenProject)?;
    action.parameters.application = Some("zed".to_owned());
    action.parameters.project_path = Some(project_path.display().to_string());
    action.desktop_workspace = Some(WorkspaceId::new("code")?);
    action.resolved_workspace_target = Some(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier("code".to_owned()),
    });
    let result = handler.apply(&action, CancellationToken::new()).await?;
    assert_eq!(result.mutations.len(), 1);
    desktop.move_window("zed-existing", "main").await?;
    handler
        .compensate(&action, &result, CancellationToken::new())
        .await?;
    assert_eq!(
        desktop
            .state()?
            .window("zed-existing")
            .and_then(|item| item.workspace_identity.clone()),
        Some("main".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn zed_observation_matches_the_project_key_across_workspaces() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![
            workspace("main", "Main", 0, true, true),
            workspace("target", "Target", 1, false, false),
        ],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-existing".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Project".to_owned()),
            project_path: Some(project_path.display().to_string()),
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let editor = Arc::new(ZedBackend::new(
        Arc::new(FixtureProcessRunner::for_cosmic(&[], &[])),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop.clone()));
    let mut action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    action.parameters.application = Some("zed".to_owned());
    action.parameters.project_path = Some(project_path.display().to_string());
    action.desktop_workspace = Some(WorkspaceId::new("target")?);
    action.resolved_workspace_target = Some(WorkspaceTarget::Existing {
        reference: WorkspaceReference::Identifier("target".to_owned()),
    });

    let observation = handler.observe(&action, CancellationToken::new()).await?;

    assert_eq!(observation.status, ObservationStatus::AlreadyCorrect);
    assert_eq!(observation.resources.len(), 1);
    assert!(desktop.calls()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn reconciliation_observes_zed_before_resolving_next_empty() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-existing".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Project".to_owned()),
            project_path: Some(project_path.display().to_string()),
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let editor = Arc::new(
        ZedBackend::new(
            Arc::new(FixtureProcessRunner::for_cosmic(&[], &[])),
            Arc::new(desktop.clone()),
            Arc::new(LocalFileSystem),
        )
        .with_command(ZedCommand::new("zed-test")),
    );
    let mut handlers = workstate::application::planner::ActionHandlerRegistry::new();
    handlers.register(ZedProjectHandler::new(editor, Arc::new(desktop.clone())))?;

    let mut integrations = workstate::integrations::IntegrationRegistry::new();
    integrations.set_capability_availability(
        workstate::platform::CapabilityId::DesktopWindows,
        true,
        None,
    )?;
    integrations.set_capability_availability(workstate::platform::CapabilityId::Zed, true, None)?;

    let mut configuration = EnvironmentConfig::new("Repeated Zed")?;
    configuration
        .workspaces
        .push(WorkspaceSpec::new("editor", WorkspaceTarget::NextEmpty)?);
    let mut action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    action.parameters.application = Some("zed".to_owned());
    action.parameters.project_path = Some(project_path.display().to_string());
    action.desktop_workspace = Some(WorkspaceId::new("editor")?);
    configuration.actions.push(action);

    let engine = ReconciliationEngine::with_clock_and_desktop(
        &integrations,
        Arc::new(handlers),
        Arc::new(workstate::application::planner::NoopReadinessCheckRunner),
        Arc::new(desktop),
        Arc::new(workstate::application::ports::SystemClock),
        SchedulerOptions::new(1, Duration::from_secs(1), Duration::from_secs(1))?,
    );
    let events = Arc::new(InMemoryEventSink::default());
    let plan = engine
        .prepare(
            &configuration,
            &RunRequest::new("run-1", false)?,
            CancellationToken::new(),
            events,
        )
        .await?;

    assert_eq!(
        plan.entries().next().map(|entry| entry.classification),
        Some(workstate::application::planner::PlanClassification::AlreadyCorrect)
    );
    Ok(())
}

#[tokio::test]
async fn zed_observation_reuses_persisted_identity_without_project_metadata() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-existing".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Project".to_owned()),
            project_path: None,
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let runner = FixtureProcessRunner::for_cosmic(&[], &[]);
    let editor = Arc::new(ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop));
    let mut action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    action.parameters.application = Some("zed".to_owned());
    action.parameters.project_path = Some(project_path.display().to_string());
    let mut persisted = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-existing")?,
        OwnershipStatus::CreatedByEnvironment,
    );
    persisted.integration_metadata.insert(
        "project_path".to_owned(),
        project_path.display().to_string(),
    );

    let observation = handler
        .observe_with_resources(&action, &[persisted], CancellationToken::new())
        .await?;

    assert_eq!(observation.status, ObservationStatus::AlreadyCorrect);
    assert_eq!(observation.resources.len(), 1);
    assert!(runner.calls()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn stop_closes_owned_zed_windows_and_preserves_shared_windows() -> TestResult {
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![
            DesktopWindowSnapshot {
                identity: "zed-owned".to_owned(),
                application: Some("dev.zed.Zed".to_owned()),
                title: Some("Owned".to_owned()),
                project_path: None,
                workspace_identity: Some("main".to_owned()),
                focused: false,
            },
            DesktopWindowSnapshot {
                identity: "zed-shared".to_owned(),
                application: Some("dev.zed.Zed".to_owned()),
                title: Some("Shared".to_owned()),
                project_path: None,
                workspace_identity: Some("main".to_owned()),
                focused: false,
            },
        ],
    });
    let editor = Arc::new(ZedBackend::new(
        Arc::new(FixtureProcessRunner::for_cosmic(&[], &[])),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop.clone()));
    let action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    let owned = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-owned")?,
        OwnershipStatus::CreatedByCurrentRun,
    );
    let shared = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-shared")?,
        OwnershipStatus::Shared,
    );
    handler
        .stop(&action, &[owned, shared], CancellationToken::new())
        .await?;
    let snapshot = desktop.state()?;
    assert!(snapshot.window("zed-owned").is_none());
    assert!(snapshot.window("zed-shared").is_some());
    Ok(())
}

#[tokio::test]
async fn zed_cleanup_uses_persisted_window_identity_without_project_metadata() -> TestResult {
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: vec![DesktopWindowSnapshot {
            identity: "zed-owned".to_owned(),
            application: Some("dev.zed.Zed".to_owned()),
            title: Some("Owned".to_owned()),
            project_path: None,
            workspace_identity: Some("main".to_owned()),
            focused: false,
        }],
    });
    let editor = Arc::new(ZedBackend::new(
        Arc::new(FixtureProcessRunner::for_cosmic(&[], &[])),
        Arc::new(desktop.clone()),
        Arc::new(LocalFileSystem),
    ));
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop.clone()));
    let action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    let resource = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-owned")?,
        OwnershipStatus::CreatedByCurrentRun,
    );

    let observation = handler
        .observe_for_cleanup(
            &action,
            std::slice::from_ref(&resource),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(observation.resources, vec![resource.clone()]);

    handler
        .stop(&action, &[resource], CancellationToken::new())
        .await?;
    assert!(desktop.state()?.window("zed-owned").is_none());
    Ok(())
}

#[derive(Clone)]
struct StickyCloseDesktop {
    snapshot: DesktopSnapshot,
}

impl DesktopBackend for StickyCloseDesktop {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, Result<DesktopSnapshot>> {
        let snapshot = self.snapshot.clone();
        Box::pin(async move { Ok(snapshot) })
    }

    fn close_window<'a>(
        &'a self,
        window_identity: &'a str,
    ) -> BoxFuture<'a, Result<DesktopOperationOutcome>> {
        Box::pin(async move {
            Ok(DesktopOperationOutcome::changed(Some(
                window_identity.to_owned(),
            )))
        })
    }
}

#[tokio::test]
async fn zed_stop_fails_when_cosmic_does_not_remove_the_window() -> TestResult {
    let desktop = StickyCloseDesktop {
        snapshot: DesktopSnapshot {
            workspaces: vec![workspace("main", "Main", 0, true, true)],
            windows: vec![DesktopWindowSnapshot {
                identity: "zed-stuck".to_owned(),
                application: Some("dev.zed.Zed".to_owned()),
                title: Some("Stuck".to_owned()),
                project_path: None,
                workspace_identity: Some("main".to_owned()),
                focused: false,
            }],
        },
    };
    let editor = Arc::new(
        ZedBackend::new(
            Arc::new(FixtureProcessRunner::for_cosmic(&[], &[])),
            Arc::new(desktop.clone()),
            Arc::new(LocalFileSystem),
        )
        .with_timing(Duration::from_millis(1), Duration::from_millis(1)),
    );
    let handler = ZedProjectHandler::new(editor, Arc::new(desktop));
    let mut action = ActionSpec::new("open-project", ActionKind::OpenProject)?;
    action.timeout = Some(Timeout::new(1)?);
    let resource = ResourceRecord::new(
        ResourceIdentity::new(ResourceKind::DesktopWindow, "zed-stuck")?,
        OwnershipStatus::CreatedByCurrentRun,
    );

    let result = handler
        .stop(&action, &[resource], CancellationToken::new())
        .await;
    assert!(result.is_err());
    let error = result.err().ok_or("missing close verification error")?;
    assert!(error.message.contains("did not close"));
    Ok(())
}

#[tokio::test]
async fn zed_timeout_is_typed_and_cleans_the_handoff() -> TestResult {
    let directory = tempdir()?;
    let project_path = directory.path().to_path_buf();
    let desktop = FakeDesktop::new(DesktopSnapshot {
        workspaces: vec![workspace("main", "Main", 0, true, true)],
        windows: Vec::new(),
    });
    let mut runner = FixtureProcessRunner::for_cosmic(&[], &[]);
    runner.launch = None;
    let runner = runner;
    let backend = ZedBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        Arc::new(LocalFileSystem),
    )
    .with_timing(Duration::from_millis(1), Duration::from_millis(5));
    let result = backend
        .open_project(project_path, CancellationToken::new())
        .await;
    assert!(result.is_err());
    let error = result.err().ok_or("missing timeout error")?;
    assert_eq!(error.category, ErrorCategory::Integration);
    assert!(error.message.contains("did not become observable"));
    assert_eq!(runner.stopped()?.len(), 1);
    Ok(())
}
