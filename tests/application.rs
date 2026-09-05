#[path = "fakes/mod.rs"]
mod fakes;

use std::{error::Error, sync::Arc};

use tempfile::tempdir;
use workstate::{
    application::{
        planner::{ActionHandler, CancellationToken, ObservationStatus},
        ports::{
            ApplicationCatalog, ApplicationLaunchSpec, DesktopSnapshot, DesktopWindowSnapshot,
            FileSystem, InstalledApplication,
        },
    },
    domain::{ActionKind, ActionSpec, OwnershipStatus},
    error::Result,
    infrastructure::filesystem::local::LocalFileSystem,
    integrations::ApplicationActionHandler,
};

use fakes::fake_desktop::FakeDesktop;

type TestResult = std::result::Result<(), Box<dyn Error>>;

#[derive(Clone)]
struct FakeApplicationCatalog;

impl ApplicationCatalog for FakeApplicationCatalog {
    fn list(&self) -> Result<Vec<InstalledApplication>> {
        Ok(vec![InstalledApplication {
            id: "org.example.Editor".to_owned(),
            name: "Editor".to_owned(),
        }])
    }

    fn launch_spec(&self, application_id: &str) -> Result<ApplicationLaunchSpec> {
        if application_id != "org.example.Editor" {
            return Err(workstate::WorkstateError::new(
                workstate::ErrorCategory::Platform,
                "the selected application is not available in the fake catalog",
            ));
        }
        Ok(ApplicationLaunchSpec {
            program: "editor".to_owned(),
            arguments: vec!["--desktop-entry".to_owned()],
        })
    }
}

fn application_action(working_directory: &str) -> Result<ActionSpec> {
    let mut action = ActionSpec::new("open-editor", ActionKind::OpenApplication)
        .map_err(workstate::WorkstateError::from)?;
    action.parameters.application = Some("org.example.Editor".to_owned());
    action.parameters.application_arguments =
        vec!["--profile".to_owned(), "work profile".to_owned()];
    action.working_directory = Some(working_directory.to_owned());
    Ok(action)
}

#[tokio::test]
async fn open_application_passes_arguments_reuses_windows_and_stops_owned_windows() -> TestResult {
    let directory = tempdir()?;
    let desktop = FakeDesktop::new(DesktopSnapshot::default());
    desktop.set_application_launch_window(DesktopWindowSnapshot {
        identity: "editor-window".to_owned(),
        application: Some("org.example.Editor".to_owned()),
        title: Some("Editor".to_owned()),
        project_path: None,
        workspace_identity: None,
        focused: false,
    })?;
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let handler = ApplicationActionHandler::new(
        Arc::new(FakeApplicationCatalog),
        Arc::new(desktop.clone()),
        file_system,
    );
    let action = application_action(directory.path().to_string_lossy().as_ref())?;
    handler.validate(&action)?;

    let first = handler.apply(&action, CancellationToken::new()).await?;
    assert!(first.changed);
    assert_eq!(first.resources.len(), 1);
    assert_eq!(
        first.resources[0].ownership,
        OwnershipStatus::CreatedByCurrentRun
    );
    let requests = desktop.application_requests()?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].program, "editor");
    assert_eq!(
        requests[0].arguments,
        vec![
            "--desktop-entry".to_owned(),
            "--profile".to_owned(),
            "work profile".to_owned()
        ]
    );

    let second = handler.apply(&action, CancellationToken::new()).await?;
    assert!(!second.changed);
    assert_eq!(
        second.resources[0].ownership,
        OwnershipStatus::ReusedExisting
    );
    assert_eq!(desktop.application_requests()?.len(), 1);

    let observation = handler.observe(&action, CancellationToken::new()).await?;
    assert_eq!(observation.status, ObservationStatus::AlreadyCorrect);

    handler
        .stop(&action, &first.resources, CancellationToken::new())
        .await?;
    assert!(desktop.state()?.windows.is_empty());
    assert!(desktop.stopped_applications()?.is_empty());
    Ok(())
}
