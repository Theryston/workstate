pub mod android;
pub mod application;
pub mod command;
pub mod cosmic;
pub mod docker;
pub mod registry;
pub mod tmux;
pub mod zed;

pub use android::{AdbClient, AndroidBackend, AndroidEmulatorActionHandler};
pub use application::ApplicationActionHandler;
pub use command::CommandActionHandler;
pub use cosmic::CosmicBackend;
pub use docker::{DockerActionHandler, DockerProcessBackend};
pub use registry::{ActionHandlerDescriptor, CapabilityAvailability, IntegrationRegistry};
pub use tmux::TmuxProcessBackend;
pub use zed::{ProjectEditorKind, ZedBackend};
