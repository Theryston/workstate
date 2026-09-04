use crate::{
    application::ports::platform::{PlatformDetector, PlatformProbe},
    error::Result,
    platform::{DetectedPlatform, OperatingSystem, TerminalCapability},
    platform::{desktop::CosmicDetector, linux::detect_distribution},
};

pub struct RuntimePlatformDetector<P> {
    probe: P,
}

impl<P> RuntimePlatformDetector<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }

    pub fn probe(&self) -> &P {
        &self.probe
    }
}

impl<P> PlatformDetector for RuntimePlatformDetector<P>
where
    P: PlatformProbe,
{
    fn detect(&self) -> Result<DetectedPlatform> {
        let operating_system = OperatingSystem::from_runtime(&self.probe.operating_system()?);
        let distribution = if operating_system.is_linux() {
            detect_distribution(&self.probe)?
        } else {
            crate::platform::Distribution::unknown()
        };
        let desktop_environment = CosmicDetector::detect_with_probe(&self.probe)?;
        let terminal = match self.probe.executable("tmux")? {
            Some(executable) => TerminalCapability::tmux(executable),
            None => TerminalCapability::unavailable_tmux(),
        };

        Ok(DetectedPlatform {
            operating_system,
            distribution,
            desktop_environment,
            terminal,
        })
    }
}
