#[path = "fakes/fake_android.rs"]
mod fake_android;
#[path = "fakes/fake_desktop.rs"]
mod fake_desktop;
#[path = "fakes/fake_process.rs"]
mod fake_process;

use std::{error::Error, path::PathBuf, sync::Arc, time::Duration};

use fake_android::FakeAndroid;
use fake_desktop::FakeDesktop;
use fake_process::FakeProcessRunner;
use workstate::{
    application::{
        planner::{ActionHandler, CancellationToken, ObservationStatus},
        ports::{
            AndroidDeviceState, AndroidVirtualDevice, BoxFuture, DesktopBackend, DesktopSnapshot,
            EmulatorObservation, EmulatorOperationStatus, EmulatorRuntimeSnapshot, ProcessOutput,
        },
    },
    domain::{
        ActionKind, ActionSpec, EmulatorSpec, EnvironmentConfig, EnvironmentSlug, OwnershipStatus,
    },
    integrations::android::{AndroidBackend, AndroidEmulatorActionHandler, models},
    ui::{EditorMode, EditorState},
};

type TestResult = std::result::Result<(), Box<dyn Error>>;

struct EmptyDesktop;

impl DesktopBackend for EmptyDesktop {
    fn snapshot<'a>(&'a self) -> BoxFuture<'a, workstate::Result<DesktopSnapshot>> {
        Box::pin(async { Ok(DesktopSnapshot::default()) })
    }
}

#[test]
fn fixture_parsers_keep_only_emulator_devices_and_sort_avds() -> TestResult {
    let avds = models::parse_avd_list(include_bytes!("fixtures/android/avds.txt"))?;
    assert_eq!(
        avds.into_iter()
            .map(|device| device.name)
            .collect::<Vec<_>>(),
        vec!["Pixel_API_35", "Tablet_API_34"]
    );

    let devices = models::parse_devices(include_bytes!("fixtures/android/adb-devices.txt"))?;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].serial, "emulator-5554");
    assert_eq!(devices[0].state, AndroidDeviceState::Device);
    Ok(())
}

#[test]
fn editor_keeps_android_virtual_devices_in_deterministic_order() -> TestResult {
    let configuration = EnvironmentConfig::new("Android tests")?;
    let state = EditorState::new(configuration, EditorMode::Create)
        .with_available_android_virtual_devices(vec![
            AndroidVirtualDevice::new("Tablet_API_34")?,
            AndroidVirtualDevice::new("Pixel_API_35")?,
        ]);
    assert_eq!(
        state
            .available_android_virtual_devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Pixel_API_35", "Tablet_API_34"]
    );
    Ok(())
}

#[tokio::test]
async fn matching_running_emulator_is_reused_without_claiming_ownership() -> TestResult {
    let fake = FakeAndroid::with_avds([AndroidVirtualDevice::new("Pixel_API_35")?]);
    fake.set_observation(EmulatorObservation::Present(EmulatorRuntimeSnapshot {
        avd: "Pixel_API_35".to_owned(),
        serial: "emulator-5554".to_owned(),
        state: AndroidDeviceState::Device,
        boot_completed: true,
        process_identity: None,
        window_identity: None,
        workspace_identity: None,
    }))?;
    let mut action = ActionSpec::new("android", ActionKind::StartAndroidEmulator)?;
    action.parameters.emulator = Some(EmulatorSpec {
        avd: "Pixel_API_35".to_owned(),
        arguments: Vec::new(),
    });
    action.resolved_environment = Some(EnvironmentSlug::new("android-tests")?);
    let handler = AndroidEmulatorActionHandler::new(Arc::new(fake.clone()), Arc::new(EmptyDesktop));

    let observation = handler.observe(&action, CancellationToken::new()).await?;

    assert_eq!(observation.status, ObservationStatus::AlreadyCorrect);
    assert_eq!(observation.resources.len(), 1);
    assert_eq!(
        observation.resources[0].ownership,
        OwnershipStatus::ReusedExisting
    );
    Ok(())
}

#[tokio::test]
async fn a_new_emulator_is_started_directly_without_tmux() -> TestResult {
    let runner = FakeProcessRunner::with_responses([
        output("Pixel_API_35\n"),
        output("List of devices attached\n"),
        output("List of devices attached\n"),
        output("List of devices attached\nemulator-5554\tdevice\n"),
        output("Pixel_API_35\nOK\n"),
        output("1\n"),
    ]);
    let desktop = FakeDesktop::new(DesktopSnapshot::default());
    let backend = AndroidBackend::new(
        Arc::new(runner.clone()),
        Arc::new(desktop),
        PathBuf::from("emulator"),
        PathBuf::from("adb"),
    )?;
    let request = workstate::application::ports::EmulatorRequest {
        context: workstate::application::ports::EmulatorActionContext {
            action_id: workstate::domain::ActionId::new("android")?,
            environment: EnvironmentSlug::new("android-tests")?,
            cleanup_policy: workstate::domain::CleanupPolicy::OwnedOnly,
        },
        specification: EmulatorSpec {
            avd: "Pixel_API_35".to_owned(),
            arguments: Vec::new(),
        },
        workspace_target: None,
        timeout: Duration::from_millis(100),
        poll_interval: Duration::from_millis(1),
    };

    let outcome = workstate::application::ports::EmulatorBackend::ensure(
        &backend,
        request,
        CancellationToken::new(),
    )
    .await?;
    let process_requests = runner.requests()?;
    let background_requests = runner.background_requests()?;

    assert_eq!(outcome.status, EmulatorOperationStatus::Started);
    assert_eq!(outcome.resources.len(), 1);
    assert_eq!(
        outcome.resources[0].ownership,
        OwnershipStatus::CreatedByCurrentRun
    );
    assert_eq!(background_requests.len(), 1);
    assert_eq!(process_requests.len(), 6);
    assert_eq!(background_requests[0].program, "emulator");
    assert_eq!(
        background_requests[0].arguments,
        vec!["-avd", "Pixel_API_35"]
    );
    assert!(background_requests[0].program != "tmux");
    Ok(())
}

fn output(stdout: &str) -> ProcessOutput {
    ProcessOutput {
        status: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}
