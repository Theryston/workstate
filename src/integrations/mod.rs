pub mod cosmic;
pub mod registry;
pub mod zed;

pub use cosmic::CosmicBackend;
pub use registry::{ActionHandlerDescriptor, CapabilityAvailability, IntegrationRegistry};
pub use zed::ZedBackend;
