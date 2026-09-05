use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use workstate::{
    AppContext, AppDependencies, ErrorCategory, WorkstateError,
    application::ports::{PlatformDetector, PlatformProbe},
    domain::ActionKind,
    integrations::{ActionHandlerDescriptor, IntegrationRegistry},
    platform::{
        DesktopEnvironment, DetectedPlatform, Distribution, OperatingSystem, TerminalCapability,
        detection::detector::RuntimePlatformDetector,
        detection::support::{
            CapabilityDescriptor, CapabilityId, DesktopPredicate, DistributionPredicate,
            OperatingSystemPredicate, SupportProfile,
        },
        linux::OS_RELEASE_PATH,
    },
};

#[derive(Clone, Default)]
struct FakeProbe {
    operating_system: String,
    files: BTreeMap<PathBuf, String>,
    environment: BTreeMap<String, String>,
    executables: BTreeMap<String, PathBuf>,
}

impl FakeProbe {
    fn supported() -> Self {
        let mut probe = Self {
            operating_system: "LiNuX".to_owned(),
            ..Self::default()
        };
        probe.files.insert(
            PathBuf::from(OS_RELEASE_PATH),
            "ID=PoP\nNAME=\"Pop!_OS\"\nVERSION_ID=\"24.04\"\nID_LIKE=ubuntu\n".to_owned(),
        );
        probe
            .environment
            .insert("XDG_CURRENT_DESKTOP".to_owned(), "CoSmIc".to_owned());
        probe
            .executables
            .insert("tmux".to_owned(), PathBuf::from("/usr/bin/tmux"));
        probe
    }

    fn with_distribution(mut self, contents: &str) -> Self {
        self.files
            .insert(PathBuf::from(OS_RELEASE_PATH), contents.to_owned());
        self
    }

    fn with_desktop(mut self, desktop: &str) -> Self {
        self.environment
            .insert("XDG_CURRENT_DESKTOP".to_owned(), desktop.to_owned());
        self
    }

    fn without_tmux(mut self) -> Self {
        self.executables.remove("tmux");
        self
    }
}

impl PlatformProbe for FakeProbe {
    fn operating_system(&self) -> workstate::Result<String> {
        Ok(self.operating_system.clone())
    }

    fn read_text(&self, path: &Path) -> workstate::Result<Option<String>> {
        Ok(self.files.get(path).cloned())
    }

    fn environment(&self, name: &str) -> workstate::Result<Option<String>> {
        Ok(self.environment.get(name).cloned())
    }

    fn executable(&self, name: &str) -> workstate::Result<Option<PathBuf>> {
        Ok(self.executables.get(name).cloned())
    }
}

fn detect(probe: &FakeProbe) -> Option<DetectedPlatform> {
    RuntimePlatformDetector::new(probe.clone()).detect().ok()
}

fn supported_platform() -> DetectedPlatform {
    DetectedPlatform {
        operating_system: OperatingSystem::Linux,
        distribution: Distribution::PopOs { version: None },
        desktop_environment: DesktopEnvironment::Cosmic,
        terminal: TerminalCapability::tmux(PathBuf::from("tmux")),
    }
}

struct StaticDetector {
    platform: DetectedPlatform,
}

impl PlatformDetector for StaticDetector {
    fn detect(&self) -> workstate::Result<DetectedPlatform> {
        Ok(self.platform.clone())
    }
}

#[test]
fn supported_pop_os_cosmic_tmux_is_detected_and_accepted() {
    let probe = FakeProbe::supported();
    let Some(platform) = detect(&probe) else {
        return;
    };

    assert_eq!(platform.operating_system, OperatingSystem::Linux);
    assert_eq!(
        platform.distribution,
        Distribution::PopOs {
            version: Some("24.04".to_owned())
        }
    );
    assert_eq!(platform.desktop_environment, DesktopEnvironment::Cosmic);
    assert!(platform.terminal.is_tmux_available());

    let registry = IntegrationRegistry::from_platform(&platform, &probe);
    assert!(registry.is_ok());
    let Some(registry) = registry.ok() else {
        return;
    };
    assert!(registry.preflight(&platform).is_ok());
}

#[test]
fn ubuntu_gnome_is_rejected_with_structured_platform_diagnostics() {
    let probe = FakeProbe::supported()
        .with_distribution("ID=ubuntu\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\n")
        .with_desktop("GNOME");
    let Some(platform) = detect(&probe) else {
        return;
    };
    let Some(registry) = IntegrationRegistry::from_platform(&platform, &probe).ok() else {
        return;
    };

    let result = registry.preflight(&platform);
    assert!(result.is_err());
    let Some(error) = result.err() else {
        return;
    };
    assert_eq!(error.category, ErrorCategory::Platform);
    assert_eq!(
        error.context.get("operating_system"),
        Some(&"Linux".to_owned())
    );
    assert_eq!(
        error.context.get("distribution"),
        Some(&"Ubuntu 24.04".to_owned())
    );
    assert_eq!(
        error.context.get("desktop_environment"),
        Some(&"GNOME".to_owned())
    );
    assert!(
        error
            .context
            .get("supported_profiles")
            .is_some_and(|value| value.contains("Linux + Pop!_OS + COSMIC + tmux"))
    );
    let rendered = error.render();
    assert!(rendered.contains("Operating system: Linux"));
    assert!(rendered.contains("Currently supported:"));
}

#[test]
fn missing_distribution_metadata_becomes_an_unknown_value() {
    let mut probe = FakeProbe::supported();
    probe.files.clear();
    let Some(platform) = detect(&probe) else {
        return;
    };

    assert!(matches!(
        platform.distribution,
        Distribution::Unknown { .. }
    ));
    let Some(registry) = IntegrationRegistry::from_platform(&platform, &probe).ok() else {
        return;
    };
    assert!(registry.preflight(&platform).is_err());
}

#[test]
fn unknown_desktop_environment_is_not_assumed_to_be_cosmic() {
    let probe = FakeProbe::supported().with_desktop("WaylandMystery");
    let Some(platform) = detect(&probe) else {
        return;
    };

    assert!(matches!(
        platform.desktop_environment,
        DesktopEnvironment::Other { .. }
    ));
    let Some(registry) = IntegrationRegistry::from_platform(&platform, &probe).ok() else {
        return;
    };
    assert!(registry.preflight(&platform).is_err());
}

#[test]
fn missing_tmux_is_reported_as_a_missing_base_capability() {
    let probe = FakeProbe::supported().without_tmux();
    let Some(platform) = detect(&probe) else {
        return;
    };
    assert!(!platform.terminal.is_tmux_available());
    let Some(registry) = IntegrationRegistry::from_platform(&platform, &probe).ok() else {
        return;
    };

    let result = registry.preflight(&platform);
    assert!(result.is_err());
    let Some(error) = result.err() else {
        return;
    };
    assert!(
        error
            .context
            .get("missing_capabilities")
            .is_some_and(|value| value.contains("terminal_sessions"))
    );
}

#[test]
fn case_normalization_is_applied_to_platform_metadata() {
    let probe =
        FakeProbe::supported().with_distribution("id=POP_OS\nname=\"POP!_OS\"\nversion_id=\"\"\n");
    let Some(platform) = detect(&probe) else {
        return;
    };

    assert!(platform.distribution.is_pop_os());
    assert_eq!(platform.desktop_environment, DesktopEnvironment::Cosmic);
}

#[test]
fn missing_tool_errors_are_scoped_to_the_requested_capability() {
    let probe = FakeProbe::supported();
    let Some(platform) = detect(&probe) else {
        return;
    };
    let Some(registry) = IntegrationRegistry::from_platform(&platform, &probe).ok() else {
        return;
    };

    let result = registry.require_capabilities([CapabilityId::Zed]);
    assert!(result.is_err());
    let Some(error) = result.err() else {
        return;
    };
    assert_eq!(error.category, ErrorCategory::Integration);
    assert_eq!(error.context.get("capability"), Some(&"zed".to_owned()));
    assert!(!error.context.contains_key("missing_capabilities"));
}

#[test]
fn registry_supports_lookup_and_additive_registration() {
    let mut registry = IntegrationRegistry::new();
    let Some(handler) = registry.handler_for(&ActionKind::OpenProject) else {
        return;
    };
    assert_eq!(handler.backend_id, "desktop");

    let profile = SupportProfile::new(
        "linux-ubuntu-gnome-tmux",
        "Linux + Ubuntu + GNOME + tmux",
        OperatingSystemPredicate::Linux,
        DistributionPredicate::Ubuntu,
        DesktopPredicate::Gnome,
        [CapabilityId::TerminalSessions],
    );
    assert!(registry.register_profile(profile).is_ok());
    assert!(
        registry
            .register_handler(ActionHandlerDescriptor::new("preview", "preview", [],))
            .is_ok()
    );

    let descriptor = CapabilityDescriptor {
        id: CapabilityId::Adb,
        display_name: "ADB duplicate",
        description: "duplicate capability",
        executable: Some("adb"),
    };
    assert!(registry.register_capability(descriptor).is_err());
}

#[test]
fn preflight_blocks_the_runner_before_mutable_dependencies_are_used() {
    let platform = DetectedPlatform {
        operating_system: OperatingSystem::Linux,
        distribution: Distribution::Ubuntu { version: None },
        desktop_environment: DesktopEnvironment::Gnome,
        terminal: TerminalCapability::unavailable_tmux(),
    };
    let mut dependencies = AppDependencies::with_noop_dependencies();
    dependencies.platform_detector = Arc::new(StaticDetector {
        platform: platform.clone(),
    });
    dependencies.integration_registry =
        Arc::new(IntegrationRegistry::for_detected_platform(&platform));
    let context = AppContext::new(dependencies);

    let runtime = tokio::runtime::Runtime::new();
    assert!(runtime.is_ok());
    let Some(runtime) = runtime.ok() else {
        return;
    };
    let result = runtime.block_on(workstate::run(context));
    assert!(result.is_err());
    let Some(error): Option<WorkstateError> = result.err() else {
        return;
    };
    assert_eq!(error.category, ErrorCategory::Platform);
}

#[test]
fn compatible_preflight_has_no_user_facing_message() {
    let platform = supported_platform();
    let registry = IntegrationRegistry::for_detected_platform(&platform);

    assert!(registry.preflight(&platform).is_ok());
}
