use crate::{
    application::ports::{DesktopEnvironmentDetector, PlatformProbe},
    error::Result,
    platform::{DesktopEnvironment, normalize_token, safe_display},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CosmicOperation {
    GetWorkspaces,
    GetWindows,
    SetTiling { workspace: String, enabled: bool },
    MoveWindow { window: String, workspace: String },
    CloseWindow { window: String },
    FocusWindow { window: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosmicCommand {
    program: String,
}

impl CosmicCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self, operation: &CosmicOperation) -> Vec<String> {
        let mut arguments = vec!["--json".to_owned()];
        match operation {
            CosmicOperation::GetWorkspaces => arguments.push("get-workspaces".to_owned()),
            CosmicOperation::GetWindows => arguments.push("get-toplevels".to_owned()),
            CosmicOperation::SetTiling { workspace, enabled } => {
                arguments.extend([
                    "workspace".to_owned(),
                    "set-tiling".to_owned(),
                    workspace.clone(),
                    if *enabled {
                        "enabled".to_owned()
                    } else {
                        "disabled".to_owned()
                    },
                ]);
            }
            CosmicOperation::MoveWindow { window, workspace } => {
                arguments.extend([
                    "window".to_owned(),
                    "move-to-workspace".to_owned(),
                    window.clone(),
                    workspace.clone(),
                ]);
            }
            CosmicOperation::CloseWindow { window } => {
                arguments.extend(["window".to_owned(), "close".to_owned(), window.clone()]);
            }
            CosmicOperation::FocusWindow { window } => {
                arguments.extend(["window".to_owned(), "activate".to_owned(), window.clone()]);
            }
        }
        arguments
    }
}

impl Default for CosmicCommand {
    fn default() -> Self {
        Self::new("cosmicmsg")
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CosmicDetector;

impl CosmicDetector {
    pub fn detect<P>(probe: &P) -> Result<DesktopEnvironment>
    where
        P: PlatformProbe,
    {
        Self::detect_with_probe(probe)
    }

    pub fn detect_with_probe<P>(probe: &P) -> Result<DesktopEnvironment>
    where
        P: PlatformProbe,
    {
        let mut observed_values = Vec::new();
        let mut cosmic_signal = false;
        let mut desktop_signal = None;

        for variable in DESKTOP_ENVIRONMENT_VARIABLES {
            let value = probe.environment(variable)?;
            let Some(value) = value else {
                continue;
            };
            if value.trim().is_empty() {
                continue;
            }

            let normalized = normalize_token(&value);
            if variable == "COSMIC_SESSION" && is_enabled_signal(&normalized) {
                cosmic_signal = true;
            }
            if normalized.contains("cosmic") {
                cosmic_signal = true;
            }

            if desktop_signal.is_none() {
                desktop_signal = classify_desktop(&normalized);
            }
            observed_values.push(safe_display(&value));
        }

        if cosmic_signal {
            return Ok(DesktopEnvironment::Cosmic);
        }
        if let Some(desktop) = desktop_signal {
            return Ok(desktop);
        }
        if let Some(value) = observed_values.into_iter().next() {
            return Ok(DesktopEnvironment::Other { value });
        }

        Ok(DesktopEnvironment::unknown())
    }
}

impl<P> DesktopEnvironmentDetector for CosmicProbeDetector<P>
where
    P: PlatformProbe,
{
    fn detect(&self) -> Result<DesktopEnvironment> {
        CosmicDetector::detect_with_probe(&self.probe)
    }
}

pub struct CosmicProbeDetector<P> {
    probe: P,
}

impl<P> CosmicProbeDetector<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }

    pub fn detect(&self) -> Result<DesktopEnvironment>
    where
        P: PlatformProbe,
    {
        CosmicDetector::detect_with_probe(&self.probe)
    }
}

const DESKTOP_ENVIRONMENT_VARIABLES: [&str; 4] = [
    "COSMIC_SESSION",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
];

fn classify_desktop(value: &str) -> Option<DesktopEnvironment> {
    if value.contains("gnome") {
        Some(DesktopEnvironment::Gnome)
    } else if value.contains("kde") || value.contains("plasma") {
        Some(DesktopEnvironment::Kde)
    } else {
        None
    }
}

fn is_enabled_signal(value: &str) -> bool {
    !matches!(value, "0" | "false" | "no" | "off")
}
