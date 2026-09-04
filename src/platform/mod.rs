use std::{
    fmt::{self, Display, Formatter},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

pub mod desktop;
pub mod detection;
pub mod linux;

pub use desktop::{CosmicDetector, CosmicProbeDetector};
pub use detection::RuntimePlatformDetector;
pub use detection::{CapabilityDescriptor, CapabilityId, SupportProfile};
pub use linux::{LinuxDetector, SystemPlatformProbe};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    Linux,
    Windows,
    MacOs,
    Unknown { value: String },
}

impl OperatingSystem {
    pub fn from_runtime(value: &str) -> Self {
        match normalize_token(value).as_str() {
            "linux" => Self::Linux,
            "windows" | "win32" => Self::Windows,
            "macos" | "mac_os" | "darwin" => Self::MacOs,
            _ => Self::Unknown {
                value: safe_display(value),
            },
        }
    }

    pub fn is_linux(&self) -> bool {
        matches!(self, Self::Linux)
    }
}

impl Display for OperatingSystem {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Linux => formatter.write_str("Linux"),
            Self::Windows => formatter.write_str("Windows"),
            Self::MacOs => formatter.write_str("macOS"),
            Self::Unknown { value } => formatter.write_str(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distribution {
    PopOs {
        version: Option<String>,
    },
    Ubuntu {
        version: Option<String>,
    },
    Other {
        id: String,
        name: Option<String>,
        version: Option<String>,
    },
    Unknown {
        value: String,
    },
}

impl Distribution {
    pub fn unknown() -> Self {
        Self::Unknown {
            value: "unknown".to_owned(),
        }
    }

    pub fn is_pop_os(&self) -> bool {
        matches!(self, Self::PopOs { .. })
    }

    pub fn is_ubuntu(&self) -> bool {
        matches!(self, Self::Ubuntu { .. })
    }
}

impl Display for Distribution {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::PopOs { version } => write_versioned(formatter, "Pop!_OS", version.as_deref()),
            Self::Ubuntu { version } => write_versioned(formatter, "Ubuntu", version.as_deref()),
            Self::Other { id, name, version } => {
                let label = name.as_deref().unwrap_or(id);
                write_versioned(formatter, label, version.as_deref())
            }
            Self::Unknown { value } => formatter.write_str(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEnvironment {
    Cosmic,
    Gnome,
    Kde,
    Other { value: String },
    Unknown { value: String },
}

impl DesktopEnvironment {
    pub fn unknown() -> Self {
        Self::Unknown {
            value: "unknown".to_owned(),
        }
    }

    pub fn is_cosmic(&self) -> bool {
        matches!(self, Self::Cosmic)
    }
}

impl Display for DesktopEnvironment {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cosmic => formatter.write_str("COSMIC"),
            Self::Gnome => formatter.write_str("GNOME"),
            Self::Kde => formatter.write_str("KDE"),
            Self::Other { value } | Self::Unknown { value } => formatter.write_str(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCapability {
    Tmux { executable: PathBuf },
    Unavailable { name: String },
    Unknown { value: String },
}

impl TerminalCapability {
    pub fn tmux(executable: PathBuf) -> Self {
        Self::Tmux { executable }
    }

    pub fn unavailable_tmux() -> Self {
        Self::Unavailable {
            name: "tmux".to_owned(),
        }
    }

    pub fn is_tmux_available(&self) -> bool {
        matches!(self, Self::Tmux { .. })
    }
}

impl Display for TerminalCapability {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tmux { .. } => formatter.write_str("tmux (available)"),
            Self::Unavailable { name } => write!(formatter, "{name} (unavailable)"),
            Self::Unknown { value } => formatter.write_str(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedPlatform {
    pub operating_system: OperatingSystem,
    pub distribution: Distribution,
    pub desktop_environment: DesktopEnvironment,
    pub terminal: TerminalCapability,
}

impl DetectedPlatform {
    pub fn unknown() -> Self {
        Self {
            operating_system: OperatingSystem::Unknown {
                value: "unknown".to_owned(),
            },
            distribution: Distribution::unknown(),
            desktop_environment: DesktopEnvironment::unknown(),
            terminal: TerminalCapability::Unknown {
                value: "unknown".to_owned(),
            },
        }
    }
}

pub type PlatformInfo = DetectedPlatform;

pub(crate) fn normalize_token(value: &str) -> String {
    safe_display(value).trim().to_ascii_lowercase()
}

pub(crate) fn compact_token(value: &str) -> String {
    normalize_token(value)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

pub(crate) fn safe_display(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect();
    let sanitized = sanitized.trim();

    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn write_versioned(
    formatter: &mut Formatter<'_>,
    label: &str,
    version: Option<&str>,
) -> fmt::Result {
    match version.filter(|value| !value.is_empty()) {
        Some(version) => write!(formatter, "{label} {version}"),
        None => formatter.write_str(label),
    }
}
