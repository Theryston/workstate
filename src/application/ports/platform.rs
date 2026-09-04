use crate::{error::Result, platform::PlatformInfo};

pub trait PlatformDetector: Send + Sync {
    fn detect(&self) -> Result<PlatformInfo>;
}
