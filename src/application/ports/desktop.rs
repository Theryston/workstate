use crate::{error::Result, platform::DesktopEnvironment};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopSnapshot;

pub trait DesktopBackend: Send + Sync {
    fn snapshot(&self) -> Result<DesktopSnapshot>;
}

pub trait DesktopEnvironmentDetector: Send + Sync {
    fn detect(&self) -> Result<DesktopEnvironment>;
}
