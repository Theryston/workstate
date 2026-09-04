use std::path::{Path, PathBuf};

use crate::{error::Result, platform::DetectedPlatform};

pub trait PlatformProbe: Send + Sync {
    fn operating_system(&self) -> Result<String>;
    fn read_text(&self, path: &Path) -> Result<Option<String>>;
    fn environment(&self, name: &str) -> Result<Option<String>>;
    fn executable(&self, name: &str) -> Result<Option<PathBuf>>;
}

pub trait PlatformDetector: Send + Sync {
    fn detect(&self) -> Result<DetectedPlatform>;
}
