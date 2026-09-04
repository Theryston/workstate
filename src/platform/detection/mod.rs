pub mod detector;
pub mod support;

pub use detector::RuntimePlatformDetector;
pub use support::{
    CapabilityDescriptor, CapabilityId, CompatibilityProfile, DesktopPredicate,
    DistributionPredicate, OperatingSystemPredicate, SupportProfile,
};
