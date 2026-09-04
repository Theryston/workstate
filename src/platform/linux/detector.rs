use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::{
    application::ports::platform::PlatformProbe,
    error::{ErrorCategory, Result, WorkstateError},
    platform::{Distribution, compact_token, normalize_token, safe_display},
};

pub const OS_RELEASE_PATH: &str = "/etc/os-release";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsReleaseMetadata {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version_id: Option<String>,
    pub id_like: Option<String>,
}

pub fn parse_os_release(contents: &str) -> OsReleaseMetadata {
    let fields: BTreeMap<String, String> = contents.lines().filter_map(parse_field).collect();

    OsReleaseMetadata {
        id: fields.get("ID").cloned(),
        name: fields.get("NAME").cloned(),
        version_id: fields.get("VERSION_ID").cloned(),
        id_like: fields.get("ID_LIKE").cloned(),
    }
}

pub fn detect_distribution<P>(probe: &P) -> Result<Distribution>
where
    P: PlatformProbe,
{
    let metadata = probe.read_text(Path::new(OS_RELEASE_PATH))?;
    let Some(contents) = metadata else {
        return Ok(Distribution::Unknown {
            value: "distribution metadata unavailable".to_owned(),
        });
    };

    Ok(distribution_from_metadata(&parse_os_release(&contents)))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxDetector;

impl LinuxDetector {
    pub fn detect_distribution<P>(&self, probe: &P) -> Result<Distribution>
    where
        P: PlatformProbe,
    {
        detect_distribution(probe)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlatformProbe;

impl PlatformProbe for SystemPlatformProbe {
    fn operating_system(&self) -> Result<String> {
        Ok(env::consts::OS.to_owned())
    }

    fn read_text(&self, path: &Path) -> Result<Option<String>> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(WorkstateError::with_source(
                ErrorCategory::Platform,
                format!("could not read platform metadata at {}", path.display()),
                source,
            )),
        }
    }

    fn environment(&self, name: &str) -> Result<Option<String>> {
        Ok(env::var_os(name).map(|value| value.to_string_lossy().into_owned()))
    }

    fn executable(&self, name: &str) -> Result<Option<PathBuf>> {
        Ok(executable_in_path(name))
    }
}

fn parse_field(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_ascii_uppercase();
    if key.is_empty()
        || !key.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return None;
    }

    let value = value.trim();
    let value = match value.as_bytes() {
        [b'"', rest @ .., b'"'] | [b'\'', rest @ .., b'\''] => {
            String::from_utf8_lossy(rest).into_owned()
        }
        _ => value.to_owned(),
    };

    Some((key, safe_display(&value)))
}

fn distribution_from_metadata(metadata: &OsReleaseMetadata) -> Distribution {
    let id = metadata
        .id
        .as_deref()
        .map(normalize_token)
        .unwrap_or_default();
    let name = metadata
        .name
        .as_deref()
        .map(safe_display)
        .filter(|value| value != "unknown");
    let version = metadata
        .version_id
        .as_deref()
        .map(safe_display)
        .filter(|value| value != "unknown");

    if is_pop_os(&id, name.as_deref()) {
        return Distribution::PopOs { version };
    }

    if id == "ubuntu" {
        return Distribution::Ubuntu { version };
    }

    if id.is_empty() {
        return Distribution::Unknown {
            value: name.unwrap_or_else(|| "unknown distribution".to_owned()),
        };
    }

    Distribution::Other {
        id: safe_display(&id),
        name,
        version,
    }
}

fn is_pop_os(id: &str, name: Option<&str>) -> bool {
    matches!(id, "pop" | "pop_os" | "pop-os" | "pop!_os")
        || name.is_some_and(|value| compact_token(value) == "popos")
}

fn executable_in_path(name: &str) -> Option<PathBuf> {
    let candidate_name = Path::new(name);
    if candidate_name.is_absolute() {
        return is_executable_file(candidate_name).then(|| candidate_name.to_path_buf());
    }

    let path_value = env::var_os("PATH")?;
    for directory in env::split_paths(&path_value) {
        let candidate = directory.join(candidate_name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}
