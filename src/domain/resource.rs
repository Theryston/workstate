use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use super::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Environment,
    Application,
    DesktopWindow,
    DesktopWorkspace,
    DockerContainer,
    DockerCompose,
    DockerDesktop,
    DockerEngine,
    TmuxSession,
    TmuxWindow,
    AndroidEmulator,
    Process,
    Custom,
}

impl Display for ResourceKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Environment => "environment",
            Self::Application => "application",
            Self::DesktopWindow => "desktop_window",
            Self::DesktopWorkspace => "desktop_workspace",
            Self::DockerContainer => "docker_container",
            Self::DockerCompose => "docker_compose",
            Self::DockerDesktop => "docker_desktop",
            Self::DockerEngine => "docker_engine",
            Self::TmuxSession => "tmux_session",
            Self::TmuxWindow => "tmux_window",
            Self::AndroidEmulator => "android_emulator",
            Self::Process => "process",
            Self::Custom => "custom",
        };

        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResourceIdentity {
    pub kind: ResourceKind,
    pub stable_identity: String,
}

impl ResourceIdentity {
    pub fn new(
        kind: ResourceKind,
        stable_identity: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let stable_identity = stable_identity.into();

        if stable_identity.is_empty() || stable_identity.contains('\0') {
            return Err(DomainError::InvalidIdentifier {
                kind: "resource".to_owned(),
                value: stable_identity,
            });
        }

        Ok(Self {
            kind,
            stable_identity,
        })
    }
}

impl Display for ResourceIdentity {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.kind, self.stable_identity)
    }
}
