use std::collections::BTreeSet;

use crate::application::ports::{AndroidDeviceSnapshot, AndroidDeviceState, AndroidVirtualDevice};

use super::errors;

pub fn parse_avd_list(bytes: &[u8]) -> crate::error::Result<Vec<AndroidVirtualDevice>> {
    let output = String::from_utf8(bytes.to_vec())
        .map_err(|error| errors::malformed("list-avds", error.to_string()))?;
    let mut names = BTreeSet::new();
    for line in output.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        if name.chars().any(char::is_control) {
            return Err(errors::malformed(
                "list-avds",
                "AVD name contains control characters",
            ));
        }
        names.insert(name.to_owned());
    }
    names.into_iter().map(AndroidVirtualDevice::new).collect()
}

pub fn parse_devices(bytes: &[u8]) -> crate::error::Result<Vec<AndroidDeviceSnapshot>> {
    let output = String::from_utf8(bytes.to_vec())
        .map_err(|error| errors::malformed("adb-devices", error.to_string()))?;
    let mut devices = Vec::new();
    for line in output
        .lines()
        .skip_while(|line| line.trim() != "List of devices attached")
    {
        let line = line.trim();
        if line.is_empty() || line == "List of devices attached" {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(serial) = fields.next() else {
            continue;
        };
        let Some(state) = fields.next() else {
            return Err(errors::malformed(
                "adb-devices",
                format!("device '{serial}' has no connection state"),
            ));
        };
        if !is_emulator_serial(serial) {
            continue;
        }
        if serial.chars().any(char::is_control) {
            return Err(errors::malformed(
                "adb-devices",
                "device serial contains control characters",
            ));
        }
        devices.push(AndroidDeviceSnapshot {
            serial: serial.to_owned(),
            avd: None,
            state: parse_device_state(state, fields.collect::<Vec<_>>().join(" ").as_str()),
            boot_completed: false,
        });
    }
    Ok(devices)
}

pub fn parse_avd_name(bytes: &[u8]) -> crate::error::Result<Option<String>> {
    let output = String::from_utf8(bytes.to_vec())
        .map_err(|error| errors::malformed("avd-name", error.to_string()))?;
    for line in output.lines() {
        let value = line.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("ok") {
            continue;
        }
        if value.to_ascii_lowercase().starts_with("error") || value.chars().any(char::is_control) {
            continue;
        }
        return Ok(Some(value.to_owned()));
    }
    Ok(None)
}

pub fn parse_boot_property(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .any(|line| matches!(line.trim(), "1" | "true" | "TRUE"))
}

pub fn is_emulator_serial(serial: &str) -> bool {
    serial.starts_with("emulator-")
}

pub fn is_android_emulator_application(application: &str) -> bool {
    let normalized = application
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("emulator") || normalized.contains("qemu")
}

fn parse_device_state(value: &str, suffix: &str) -> AndroidDeviceState {
    let combined = if value.eq_ignore_ascii_case("no") && suffix.eq_ignore_ascii_case("permissions")
    {
        "no permissions"
    } else {
        value
    };
    match combined.to_ascii_lowercase().as_str() {
        "device" => AndroidDeviceState::Device,
        "offline" => AndroidDeviceState::Offline,
        "unauthorized" => AndroidDeviceState::Unauthorized,
        "no permissions" | "nopermissions" => AndroidDeviceState::NoPermissions,
        other => AndroidDeviceState::Unknown(other.to_owned()),
    }
}

pub fn state_label(state: &AndroidDeviceState) -> String {
    match state {
        AndroidDeviceState::Device => "device".to_owned(),
        AndroidDeviceState::Offline => "offline".to_owned(),
        AndroidDeviceState::Unauthorized => "unauthorized".to_owned(),
        AndroidDeviceState::NoPermissions => "no permissions".to_owned(),
        AndroidDeviceState::Unknown(value) => value.clone(),
    }
}

pub fn serials(devices: &[AndroidDeviceSnapshot]) -> BTreeSet<String> {
    devices.iter().map(|device| device.serial.clone()).collect()
}

#[cfg(test)]
mod tests {
    use crate::application::ports::AndroidDeviceState;

    use super::{parse_avd_list, parse_avd_name, parse_boot_property, parse_devices};

    #[test]
    fn parses_and_sorts_avd_names_without_duplicates() {
        let result = parse_avd_list(b"Pixel_API_35\n\nTablet_API_34\nPixel_API_35\n");
        assert!(result.is_ok());
        let Some(result) = result.ok() else {
            return;
        };
        assert_eq!(
            result.into_iter().map(|avd| avd.name).collect::<Vec<_>>(),
            vec!["Pixel_API_35", "Tablet_API_34"]
        );
    }

    #[test]
    fn parses_only_emulator_devices_and_preserves_states() {
        let result = parse_devices(
            b"List of devices attached\nemulator-5554\tdevice\nR58M123\tdevice\nemulator-5556\toffline\n",
        );
        assert!(result.is_ok());
        let Some(result) = result.ok() else {
            return;
        };
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].serial, "emulator-5554");
        assert_eq!(result[0].state, AndroidDeviceState::Device);
        assert_eq!(result[1].state, AndroidDeviceState::Offline);
    }

    #[test]
    fn parses_avd_name_and_boot_property() {
        assert_eq!(
            parse_avd_name(b"Pixel_API_35\nOK\n").ok(),
            Some(Some("Pixel_API_35".to_owned()))
        );
        assert!(parse_boot_property(b"0\n1\n"));
        assert!(!parse_boot_property(b"0\n"));
    }
}
