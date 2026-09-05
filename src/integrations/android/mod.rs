pub mod adb;
pub mod checks;
pub mod emulator;
pub mod errors;
pub mod models;

use std::path::{Path, PathBuf};

use crate::{application::ports::PlatformProbe, error::Result};

pub use adb::AdbClient;
pub use emulator::{AndroidBackend, AndroidEmulatorActionHandler};
pub use errors::AndroidError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidTool {
    Emulator,
    Adb,
}

impl AndroidTool {
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Emulator => "emulator",
            Self::Adb => "adb",
        }
    }

    fn sdk_relative_path(self) -> &'static Path {
        match self {
            Self::Emulator => Path::new("emulator/emulator"),
            Self::Adb => Path::new("platform-tools/adb"),
        }
    }
}

pub fn find_tool<P>(probe: &P, tool: AndroidTool) -> Result<Option<PathBuf>>
where
    P: PlatformProbe,
{
    if let Some(path) = probe.executable(tool.executable_name())? {
        return Ok(Some(path));
    }

    let mut roots = Vec::new();
    for variable in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = probe.environment(variable)?
            && !root.is_empty()
        {
            roots.push(PathBuf::from(root));
        }
    }
    if roots.is_empty()
        && let Some(home) = probe.environment("HOME")?
        && !home.is_empty()
    {
        roots.push(PathBuf::from(home).join("Android/Sdk"));
    }

    for root in roots {
        let candidate = root.join(tool.sdk_relative_path());
        let candidate_text = candidate.to_string_lossy().into_owned();
        if let Some(path) = probe.executable(&candidate_text)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
