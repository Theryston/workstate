use std::{
    collections::BTreeSet,
    fmt::{self, Display, Formatter},
};

use serde::{Deserialize, Serialize};

use crate::platform::{DesktopEnvironment, DetectedPlatform, Distribution, OperatingSystem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    DesktopWorkspaces,
    DesktopWindows,
    DesktopTiling,
    TerminalSessions,
    BackgroundProcesses,
    DockerEngine,
    DockerDesktop,
    DockerCompose,
    Zed,
    VsCode,
    Cursor,
    AndroidEmulator,
    Adb,
}

impl CapabilityId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DesktopWorkspaces => "desktop_workspaces",
            Self::DesktopWindows => "desktop_windows",
            Self::DesktopTiling => "desktop_tiling",
            Self::TerminalSessions => "terminal_sessions",
            Self::BackgroundProcesses => "background_processes",
            Self::DockerEngine => "docker_engine",
            Self::DockerDesktop => "docker_desktop",
            Self::DockerCompose => "docker_compose",
            Self::Zed => "zed",
            Self::VsCode => "vs_code",
            Self::Cursor => "cursor",
            Self::AndroidEmulator => "android_emulator",
            Self::Adb => "adb",
        }
    }
}

impl Display for CapabilityId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub display_name: &'static str,
    pub description: &'static str,
    pub executable: Option<&'static str>,
}

pub fn capability_descriptors() -> Vec<CapabilityDescriptor> {
    vec![
        CapabilityDescriptor {
            id: CapabilityId::DesktopWorkspaces,
            display_name: "Desktop workspaces",
            description: "Create, select, and target desktop workspaces",
            executable: None,
        },
        CapabilityDescriptor {
            id: CapabilityId::DesktopWindows,
            display_name: "Desktop windows",
            description: "Discover, open, and position application windows",
            executable: None,
        },
        CapabilityDescriptor {
            id: CapabilityId::DesktopTiling,
            display_name: "Desktop tiling",
            description: "Read, enable, and restore desktop tiling",
            executable: None,
        },
        CapabilityDescriptor {
            id: CapabilityId::TerminalSessions,
            display_name: "Terminal sessions",
            description: "Create and manage persistent terminal multiplexer sessions",
            executable: None,
        },
        CapabilityDescriptor {
            id: CapabilityId::BackgroundProcesses,
            display_name: "Background processes",
            description: "Keep environment processes running after setup exits",
            executable: None,
        },
        CapabilityDescriptor {
            id: CapabilityId::DockerEngine,
            display_name: "Docker Engine",
            description: "Start and inspect Docker containers",
            executable: Some("docker"),
        },
        CapabilityDescriptor {
            id: CapabilityId::DockerDesktop,
            display_name: "Docker Desktop",
            description: "Start and inspect Docker Desktop",
            executable: Some("docker-desktop"),
        },
        CapabilityDescriptor {
            id: CapabilityId::DockerCompose,
            display_name: "Docker Compose",
            description: "Start and inspect Docker Compose projects",
            executable: Some("docker"),
        },
        CapabilityDescriptor {
            id: CapabilityId::Zed,
            display_name: "Zed",
            description: "Open projects in the Zed editor",
            executable: Some("zed"),
        },
        CapabilityDescriptor {
            id: CapabilityId::VsCode,
            display_name: "VS Code",
            description: "Open projects in Visual Studio Code",
            executable: Some("code"),
        },
        CapabilityDescriptor {
            id: CapabilityId::Cursor,
            display_name: "Cursor",
            description: "Open projects in Cursor",
            executable: Some("cursor"),
        },
        CapabilityDescriptor {
            id: CapabilityId::AndroidEmulator,
            display_name: "Android Emulator",
            description: "Start and inspect Android virtual devices",
            executable: Some("emulator"),
        },
        CapabilityDescriptor {
            id: CapabilityId::Adb,
            display_name: "Android Debug Bridge",
            description: "Inspect and control Android devices",
            executable: Some("adb"),
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystemPredicate {
    Linux,
    Any,
}

impl OperatingSystemPredicate {
    pub fn matches(self, operating_system: &OperatingSystem) -> bool {
        match self {
            Self::Linux => matches!(operating_system, OperatingSystem::Linux),
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistributionPredicate {
    PopOs,
    Ubuntu,
    Any,
}

impl DistributionPredicate {
    pub fn matches(self, distribution: &Distribution) -> bool {
        match self {
            Self::PopOs => distribution.is_pop_os(),
            Self::Ubuntu => distribution.is_ubuntu(),
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPredicate {
    Cosmic,
    Gnome,
    Kde,
    Any,
}

impl DesktopPredicate {
    pub fn matches(self, desktop_environment: &DesktopEnvironment) -> bool {
        match self {
            Self::Cosmic => desktop_environment.is_cosmic(),
            Self::Gnome => matches!(desktop_environment, DesktopEnvironment::Gnome),
            Self::Kde => matches!(desktop_environment, DesktopEnvironment::Kde),
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportProfile {
    pub id: String,
    pub description: String,
    pub operating_system: OperatingSystemPredicate,
    pub distribution: DistributionPredicate,
    pub desktop: DesktopPredicate,
    pub required_base_capabilities: BTreeSet<CapabilityId>,
}

impl SupportProfile {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        operating_system: OperatingSystemPredicate,
        distribution: DistributionPredicate,
        desktop: DesktopPredicate,
        required_base_capabilities: impl IntoIterator<Item = CapabilityId>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            operating_system,
            distribution,
            desktop,
            required_base_capabilities: required_base_capabilities.into_iter().collect(),
        }
    }

    pub fn pop_os_cosmic() -> Self {
        Self::new(
            "pop-os-cosmic",
            "Pop!_OS + COSMIC",
            OperatingSystemPredicate::Linux,
            DistributionPredicate::PopOs,
            DesktopPredicate::Cosmic,
            [
                CapabilityId::DesktopWorkspaces,
                CapabilityId::DesktopWindows,
                CapabilityId::DesktopTiling,
                CapabilityId::TerminalSessions,
                CapabilityId::BackgroundProcesses,
            ],
        )
    }

    pub fn initial() -> Self {
        Self::pop_os_cosmic()
    }

    pub fn matches_identity(&self, platform: &DetectedPlatform) -> bool {
        self.operating_system.matches(&platform.operating_system)
            && self.distribution.matches(&platform.distribution)
            && self.desktop.matches(&platform.desktop_environment)
    }

    pub fn required_capabilities(&self) -> &BTreeSet<CapabilityId> {
        &self.required_base_capabilities
    }
}

pub type CompatibilityProfile = SupportProfile;
