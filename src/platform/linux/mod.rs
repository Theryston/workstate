pub mod applications;
pub mod detector;

pub use applications::LinuxApplicationCatalog;
pub use detector::{
    LinuxDetector, OS_RELEASE_PATH, OsReleaseMetadata, SystemPlatformProbe, detect_distribution,
    parse_os_release,
};
