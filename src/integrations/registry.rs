use std::collections::{BTreeMap, BTreeSet};

use crate::{
    application::ports::platform::PlatformProbe,
    domain::ActionKind,
    error::{ErrorCategory, Result, WorkstateError},
    platform::detection::support::{
        CapabilityDescriptor, CapabilityId, SupportProfile, capability_descriptors,
    },
    platform::{DesktopEnvironment, DetectedPlatform},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAvailability {
    pub descriptor: CapabilityDescriptor,
    pub available: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionHandlerDescriptor {
    pub action_key: String,
    pub backend_id: String,
    pub required_capabilities: BTreeSet<CapabilityId>,
}

impl ActionHandlerDescriptor {
    pub fn new(
        action_key: impl Into<String>,
        backend_id: impl Into<String>,
        required_capabilities: impl IntoIterator<Item = CapabilityId>,
    ) -> Self {
        Self {
            action_key: action_key.into(),
            backend_id: backend_id.into(),
            required_capabilities: required_capabilities.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntegrationRegistry {
    profiles: BTreeMap<String, SupportProfile>,
    capabilities: BTreeMap<CapabilityId, CapabilityAvailability>,
    handlers: BTreeMap<String, ActionHandlerDescriptor>,
    backends: BTreeMap<CapabilityId, String>,
}

impl IntegrationRegistry {
    pub fn new() -> Self {
        let capabilities = capability_descriptors()
            .into_iter()
            .map(|descriptor| {
                let id = descriptor.id;
                (
                    id,
                    CapabilityAvailability {
                        descriptor,
                        available: false,
                        detail: None,
                    },
                )
            })
            .collect();

        let mut registry = Self {
            profiles: BTreeMap::new(),
            capabilities,
            handlers: BTreeMap::new(),
            backends: BTreeMap::new(),
        };
        registry.register_default_profile();
        registry.register_default_handlers();
        registry
    }

    pub fn from_platform<P>(platform: &DetectedPlatform, probe: &P) -> Result<Self>
    where
        P: PlatformProbe,
    {
        let mut registry = Self::new();
        registry.refresh_capabilities(platform, probe)?;
        Ok(registry)
    }

    pub fn for_detected_platform(platform: &DetectedPlatform) -> Self {
        let mut registry = Self::new();
        registry.refresh_base_capabilities(platform);
        registry
    }

    pub fn support_profiles(&self) -> Vec<SupportProfile> {
        self.profiles.values().cloned().collect()
    }

    pub fn capability_descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.capabilities
            .values()
            .map(|availability| availability.descriptor)
            .collect()
    }

    pub fn available_capability_descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.capabilities
            .values()
            .filter(|availability| availability.available)
            .map(|availability| availability.descriptor)
            .collect()
    }

    pub fn available_capabilities(&self) -> Vec<CapabilityAvailability> {
        self.capabilities
            .values()
            .filter(|availability| availability.available)
            .cloned()
            .collect()
    }

    pub fn capability(&self, id: CapabilityId) -> Option<&CapabilityAvailability> {
        self.capabilities.get(&id)
    }

    pub fn handler_for(&self, action: &ActionKind) -> Option<&ActionHandlerDescriptor> {
        self.handlers.get(&action.key())
    }

    pub fn register_profile(&mut self, profile: SupportProfile) -> Result<()> {
        if self.profiles.contains_key(&profile.id) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("support profile '{}' is already registered", profile.id),
            ));
        }

        self.profiles.insert(profile.id.clone(), profile);
        Ok(())
    }

    pub fn register_handler(&mut self, handler: ActionHandlerDescriptor) -> Result<()> {
        if self.handlers.contains_key(&handler.action_key) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "action handler '{}' is already registered",
                    handler.action_key
                ),
            ));
        }

        self.handlers.insert(handler.action_key.clone(), handler);
        Ok(())
    }

    pub fn register_capability(&mut self, descriptor: CapabilityDescriptor) -> Result<()> {
        if self.capabilities.contains_key(&descriptor.id) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "capability '{}' is already registered",
                    descriptor.id.as_str()
                ),
            ));
        }

        self.capabilities.insert(
            descriptor.id,
            CapabilityAvailability {
                descriptor,
                available: false,
                detail: None,
            },
        );
        Ok(())
    }

    pub fn set_capability_availability(
        &mut self,
        capability: CapabilityId,
        available: bool,
        detail: Option<String>,
    ) -> Result<()> {
        let Some(state) = self.capabilities.get_mut(&capability) else {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!("cannot update unknown capability '{}'", capability.as_str()),
            ));
        };
        state.available = available;
        state.detail = detail;
        Ok(())
    }

    pub fn register_backend(
        &mut self,
        capability: CapabilityId,
        backend_id: impl Into<String>,
    ) -> Result<()> {
        if !self.capabilities.contains_key(&capability) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "cannot register a backend for unknown capability '{}'",
                    capability.as_str()
                ),
            ));
        }
        if self.backends.contains_key(&capability) {
            return Err(WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "a backend for capability '{}' is already registered",
                    capability.as_str()
                ),
            ));
        }

        self.backends.insert(capability, backend_id.into());
        Ok(())
    }

    pub fn preflight(&self, platform: &DetectedPlatform) -> Result<()> {
        let matching_profile = self
            .profiles
            .values()
            .filter(|profile| profile.matches_identity(platform))
            .find(|profile| {
                self.missing_capabilities(&profile.required_base_capabilities)
                    .is_empty()
            });

        if matching_profile.is_some() {
            return Ok(());
        }

        let missing = self
            .profiles
            .values()
            .filter(|profile| profile.matches_identity(platform))
            .chain(self.profiles.values())
            .next()
            .map(|profile| self.missing_capabilities(&profile.required_base_capabilities))
            .unwrap_or_default();
        Err(self.unsupported_platform_error(platform, &missing))
    }

    pub fn require_capabilities<I>(&self, required: I) -> Result<()>
    where
        I: IntoIterator<Item = CapabilityId>,
    {
        for id in required {
            let Some(availability) = self.capabilities.get(&id) else {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    format!("capability '{}' is not registered", id.as_str()),
                )
                .with_context("capability", id.as_str()));
            };

            if !availability.available {
                let mut error = WorkstateError::new(
                    ErrorCategory::Integration,
                    format!("required capability '{}' is unavailable", id.as_str()),
                )
                .with_context("capability", id.as_str())
                .with_context("display_name", availability.descriptor.display_name);
                if let Some(detail) = &availability.detail {
                    error = error.with_context("detail", detail);
                }
                return Err(error);
            }
        }

        Ok(())
    }

    pub fn select_backend(
        &self,
        capability: CapabilityId,
        platform: &DetectedPlatform,
    ) -> Result<&str> {
        self.preflight(platform)?;
        self.require_capabilities([capability])?;
        self.handler_for_capability(capability).ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Integration,
                format!(
                    "no backend is registered for capability '{}'",
                    capability.as_str()
                ),
            )
            .with_context("capability", capability.as_str())
        })
    }

    pub fn backend_for(
        &self,
        capability: CapabilityId,
        platform: &DetectedPlatform,
    ) -> Result<&str> {
        self.select_backend(capability, platform)
    }

    fn register_default_profile(&mut self) {
        let profile = SupportProfile::initial();
        self.profiles.insert(profile.id.clone(), profile);
    }

    fn register_default_handlers(&mut self) {
        let handlers = [
            ActionHandlerDescriptor::new(
                "open_application",
                "desktop",
                [CapabilityId::DesktopWindows],
            ),
            ActionHandlerDescriptor::new("open_project", "desktop", [CapabilityId::DesktopWindows]),
            ActionHandlerDescriptor::new(
                "run_command",
                "terminal",
                [CapabilityId::BackgroundProcesses],
            ),
            ActionHandlerDescriptor::new(
                "configure_tiling",
                "desktop",
                [CapabilityId::DesktopTiling],
            ),
            ActionHandlerDescriptor::new("start_container", "docker", [CapabilityId::DockerEngine]),
            ActionHandlerDescriptor::new("start_compose", "docker", [CapabilityId::DockerCompose]),
            ActionHandlerDescriptor::new(
                "start_android_emulator",
                "android",
                [CapabilityId::AndroidEmulator, CapabilityId::Adb],
            ),
        ];

        for handler in handlers {
            self.handlers.insert(handler.action_key.clone(), handler);
        }

        let backends = [
            (CapabilityId::DesktopWorkspaces, "desktop"),
            (CapabilityId::DesktopWindows, "desktop"),
            (CapabilityId::DesktopTiling, "desktop"),
            (CapabilityId::TerminalSessions, "terminal"),
            (CapabilityId::BackgroundProcesses, "terminal"),
            (CapabilityId::DockerEngine, "docker"),
            (CapabilityId::DockerDesktop, "docker"),
            (CapabilityId::DockerCompose, "docker"),
            (CapabilityId::Zed, "zed"),
            (CapabilityId::AndroidEmulator, "android"),
            (CapabilityId::Adb, "android"),
        ];
        self.backends.extend(
            backends
                .into_iter()
                .map(|(capability, backend)| (capability, backend.to_owned())),
        );
    }

    fn refresh_capabilities<P>(&mut self, platform: &DetectedPlatform, probe: &P) -> Result<()>
    where
        P: PlatformProbe,
    {
        self.refresh_base_capabilities(platform);

        for id in [
            CapabilityId::DockerEngine,
            CapabilityId::DockerDesktop,
            CapabilityId::DockerCompose,
            CapabilityId::Zed,
            CapabilityId::AndroidEmulator,
            CapabilityId::Adb,
        ] {
            let descriptor = self
                .capabilities
                .get(&id)
                .map(|availability| availability.descriptor);
            let Some(descriptor) = descriptor else {
                continue;
            };
            let executable = descriptor.executable;
            let path = if id == CapabilityId::DockerCompose {
                probe
                    .executable("docker")?
                    .or(probe.executable("docker-compose")?)
            } else if id == CapabilityId::AndroidEmulator {
                crate::integrations::android::find_tool(
                    probe,
                    crate::integrations::android::AndroidTool::Emulator,
                )?
            } else if id == CapabilityId::Adb {
                crate::integrations::android::find_tool(
                    probe,
                    crate::integrations::android::AndroidTool::Adb,
                )?
            } else {
                executable
                    .map(|name| probe.executable(name))
                    .transpose()?
                    .flatten()
            };
            self.set_availability(
                id,
                path.is_some(),
                path.map(|value| format!("available at {}", value.display()))
                    .or_else(|| {
                        if id == CapabilityId::DockerCompose {
                            Some("Docker Compose executable was not found".to_owned())
                        } else {
                            executable.map(|name| format!("executable '{name}' was not found"))
                        }
                    }),
            );
        }

        Ok(())
    }

    fn refresh_base_capabilities(&mut self, platform: &DetectedPlatform) {
        let desktop_available = platform.desktop_environment == DesktopEnvironment::Cosmic;
        for id in [
            CapabilityId::DesktopWorkspaces,
            CapabilityId::DesktopWindows,
            CapabilityId::DesktopTiling,
        ] {
            self.set_availability(
                id,
                desktop_available,
                (!desktop_available).then(|| "COSMIC desktop support is unavailable".to_owned()),
            );
        }

        let terminal_available = platform.terminal.is_tmux_available();
        for id in [
            CapabilityId::TerminalSessions,
            CapabilityId::BackgroundProcesses,
        ] {
            self.set_availability(
                id,
                terminal_available,
                (!terminal_available).then(|| "tmux is unavailable".to_owned()),
            );
        }
    }

    fn set_availability(&mut self, id: CapabilityId, available: bool, detail: Option<String>) {
        if let Some(capability) = self.capabilities.get_mut(&id) {
            capability.available = available;
            capability.detail = detail;
        }
    }

    fn missing_capabilities(&self, required: &BTreeSet<CapabilityId>) -> Vec<CapabilityId> {
        required
            .iter()
            .copied()
            .filter(|id| {
                self.capabilities
                    .get(id)
                    .is_none_or(|availability| !availability.available)
            })
            .collect()
    }

    fn handler_for_capability(&self, capability: CapabilityId) -> Option<&str> {
        self.backends.get(&capability).map(String::as_str)
    }

    fn unsupported_platform_error(
        &self,
        platform: &DetectedPlatform,
        missing: &[CapabilityId],
    ) -> WorkstateError {
        let profiles = self
            .profiles
            .values()
            .map(|profile| profile.description.clone())
            .collect::<Vec<_>>();
        let mut error = WorkstateError::new(
            ErrorCategory::Platform,
            "This system is not supported by the current Workstate profiles",
        )
        .with_context("operating_system", platform.operating_system.to_string())
        .with_context("distribution", platform.distribution.to_string())
        .with_context(
            "desktop_environment",
            platform.desktop_environment.to_string(),
        )
        .with_context("terminal_capability", platform.terminal.to_string())
        .with_context("supported_profiles", profiles.join("; "));

        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            error = error.with_context("missing_capabilities", names);
        }

        error
    }
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}
