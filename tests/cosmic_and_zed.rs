#[path = "fakes/mod.rs"]
mod fakes;

use std::{
    collections::VecDeque,
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use tempfile::tempdir;
use workstate::{
    application::{
        planner::{ActionHandler, CancellationToken, ObservationStatus},
        ports::{
            BackgroundProcess, BoxFuture, DesktopBackend, DesktopSnapshot, DesktopWindowSnapshot,
            DesktopWorkspaceSnapshot, EditorBackend, ProcessOutput, ProcessRequest, ProcessRunner,
            ensure_workspace, resolve_workspace_target,
        },
    },
    domain::{
        ActionKind, ActionSpec, OwnershipStatus, ResourceIdentity, ResourceKind, ResourceRecord,
        TilingPreference, WorkspaceId, WorkspaceReference, WorkspaceTarget,
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
                    project_path: Some(project_path.display().to_string()),
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
