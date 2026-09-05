pub mod command;
pub mod cosmic;
pub mod registry;
pub mod tmux;
pub mod zed;

pub use command::CommandActionHandler;
pub use cosmic::CosmicBackend;
pub use registry::{ActionHandlerDescriptor, CapabilityAvailability, IntegrationRegistry};
pub use tmux::TmuxProcessBackend;
pub use zed::ZedBackend;
