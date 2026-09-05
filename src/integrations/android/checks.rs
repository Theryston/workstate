use std::{collections::BTreeSet, time::Duration};

use crate::{
    application::{
        planner::CancellationToken,
        ports::{AndroidDeviceSnapshot, DesktopBackend, DesktopSnapshot, DesktopWindowSnapshot},
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{adb::AdbClient, errors::AndroidError, models};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn wait_for_device(
    adb: &AdbClient,
    avd: &str,
    before_serials: &BTreeSet<String>,
    timeout: Duration,
    poll_interval: Duration,
    cancellation: CancellationToken,
) -> Result<AndroidDeviceSnapshot> {
    validate_timeout(timeout, "Android device readiness")?;
    cancellation.check()?;
    let deadline = tokio::time::Instant::now() + timeout;
    let interval = normalized_poll_interval(poll_interval);
    let mut last_serial = None;
    let mut last_state = "missing".to_owned();
    let mut last_boot_completed = false;

    loop {
        cancellation.check()?;
        let devices = adb.observe_emulators(&cancellation).await?;
        let candidates = matching_devices(&devices, avd, before_serials);
        if candidates.len() > 1 {
            return Err(AndroidError::AmbiguousAvd {
                avd: avd.to_owned(),
                serials: candidates
                    .iter()
                    .map(|device| device.serial.clone())
                    .collect(),
            }
            .into_workstate());
        }
        if let Some(device) = candidates.into_iter().next() {
            last_serial = Some(device.serial.clone());
            last_state = models::state_label(&device.state);
            last_boot_completed = device.boot_completed;
            if device.is_ready() {
                return Ok((*device).clone());
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AndroidError::DeviceTimeout {
                avd: avd.to_owned(),
                serial: last_serial,
                last_state,
                boot_completed: last_boot_completed,
            }
            .into_workstate());
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(cancellation_error("Android device readiness"));
            }
            _ = tokio::time::sleep(remaining.min(interval)) => {}
        }
    }
}

pub async fn wait_for_window(
    desktop: &dyn DesktopBackend,
    avd: &str,
    serial: &str,
    timeout: Duration,
    poll_interval: Duration,
    cancellation: CancellationToken,
) -> Result<DesktopWindowSnapshot> {
    validate_timeout(timeout, "Android Emulator window readiness")?;
    cancellation.check()?;
    let deadline = tokio::time::Instant::now() + timeout;
    let interval = normalized_poll_interval(poll_interval);
    loop {
        cancellation.check()?;
        let snapshot = desktop.snapshot().await?;
        if let Some(window) = find_matching_window(&snapshot, avd, serial)? {
            return Ok(window);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AndroidError::WindowTimeout {
                avd: avd.to_owned(),
                serial: serial.to_owned(),
            }
            .into_workstate());
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(cancellation_error("Android Emulator window readiness"));
            }
            _ = tokio::time::sleep(remaining.min(interval)) => {}
        }
    }
}

pub fn find_matching_window(
    snapshot: &DesktopSnapshot,
    avd: &str,
    serial: &str,
) -> Result<Option<DesktopWindowSnapshot>> {
    match matching_window(snapshot, avd, serial)? {
        WindowMatch::One(window) => Ok(Some(window)),
        WindowMatch::None => Ok(None),
    }
}

pub async fn wait_for_device_absence(
    adb: &AdbClient,
    serial: &str,
    timeout: Duration,
    poll_interval: Duration,
    cancellation: CancellationToken,
) -> Result<()> {
    validate_timeout(timeout, "Android Emulator cleanup")?;
    cancellation.check()?;
    let deadline = tokio::time::Instant::now() + timeout;
    let interval = normalized_poll_interval(poll_interval);
    loop {
        cancellation.check()?;
        let devices = adb.observe_emulators(&cancellation).await?;
        if !devices.iter().any(|device| device.serial == serial) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                format!("Android Emulator '{serial}' did not stop before the timeout"),
            )
            .with_context("serial", serial.to_owned()));
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(cancellation_error("Android Emulator cleanup"));
            }
            _ = tokio::time::sleep(remaining.min(interval)) => {}
        }
    }
}

fn matching_devices<'a>(
    devices: &'a [AndroidDeviceSnapshot],
    avd: &str,
    _before_serials: &BTreeSet<String>,
) -> Vec<&'a AndroidDeviceSnapshot> {
    devices
        .iter()
        .filter(|device| device.avd.as_deref() == Some(avd))
        .collect::<Vec<_>>()
}

enum WindowMatch {
    None,
    One(DesktopWindowSnapshot),
}

fn matching_window(snapshot: &DesktopSnapshot, avd: &str, serial: &str) -> Result<WindowMatch> {
    let emulator_windows = snapshot
        .windows
        .iter()
        .filter(|window| {
            window
                .application
                .as_deref()
                .is_some_and(models::is_android_emulator_application)
        })
        .cloned()
        .collect::<Vec<_>>();
    let preferred = emulator_windows
        .iter()
        .filter(|window| window_matches_runtime(window, avd, serial))
        .cloned()
        .collect::<Vec<_>>();
    match preferred.as_slice() {
        [window] => return Ok(WindowMatch::One(window.clone())),
        [] => {}
        _ => {
            return Err(AndroidError::AmbiguousWindow {
                avd: avd.to_owned(),
                serial: serial.to_owned(),
                matches: preferred.len(),
            }
            .into_workstate());
        }
    }
    Ok(WindowMatch::None)
}

fn window_matches_runtime(window: &DesktopWindowSnapshot, avd: &str, serial: &str) -> bool {
    window.identity == serial
        || window
            .title
            .as_deref()
            .is_some_and(|title| title.contains(serial) || title.contains(avd))
}

fn normalized_poll_interval(value: Duration) -> Duration {
    if value.is_zero() {
        DEFAULT_POLL_INTERVAL
    } else {
        value
    }
}

fn validate_timeout(timeout: Duration, operation: &str) -> Result<()> {
    if timeout.is_zero() {
        return Err(WorkstateError::new(
            ErrorCategory::Runtime,
            format!("{operation} timeout must be greater than zero"),
        ));
    }
    Ok(())
}

fn cancellation_error(operation: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Runtime,
        format!("operation was cancelled during {operation}"),
    )
    .with_context("cancelled", "true")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator_window(
        identity: &str,
        title: Option<&str>,
        workspace_identity: Option<&str>,
    ) -> DesktopWindowSnapshot {
        DesktopWindowSnapshot {
            identity: identity.to_owned(),
            application: Some("qemu-system-x86_64".to_owned()),
            title: title.map(str::to_owned),
            project_path: None,
            workspace_identity: workspace_identity.map(str::to_owned),
            focused: false,
        }
    }

    #[test]
    fn does_not_match_an_unrelated_emulator_window() {
        let snapshot = DesktopSnapshot {
            windows: vec![emulator_window(
                "window-other",
                Some("Other_API_35"),
                Some("workspace-1"),
            )],
            ..DesktopSnapshot::default()
        };

        let result = find_matching_window(&snapshot, "Pixel_API_35", "emulator-5554");

        assert!(result.is_ok());
        let Some(window) = result.ok() else {
            return;
        };
        assert!(window.is_none());
    }

    #[test]
    fn matches_a_window_by_the_configured_avd_title() {
        let snapshot = DesktopSnapshot {
            windows: vec![emulator_window(
                "window-pixel",
                Some("Pixel_API_35:5554"),
                Some("workspace-1"),
            )],
            ..DesktopSnapshot::default()
        };

        let result = find_matching_window(&snapshot, "Pixel_API_35", "emulator-5554");

        assert!(result.is_ok());
        let Some(window) = result.ok() else {
            return;
        };
        assert_eq!(
            window.map(|value| value.identity),
            Some("window-pixel".to_owned())
        );
    }

    #[test]
    fn rejects_multiple_windows_matching_the_same_emulator() {
        let snapshot = DesktopSnapshot {
            windows: vec![
                emulator_window("window-1", Some("Pixel_API_35"), None),
                emulator_window("window-2", Some("Pixel_API_35"), None),
            ],
            ..DesktopSnapshot::default()
        };

        let result = find_matching_window(&snapshot, "Pixel_API_35", "emulator-5554");

        assert!(result.is_err());
    }
}
