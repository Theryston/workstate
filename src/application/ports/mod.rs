pub mod clock;
pub mod containers;
pub mod desktop;
pub mod editor;
pub mod emulator;
pub mod filesystem;
pub mod persistence;
pub mod platform;
pub mod process;
pub mod terminal;

pub use clock::{Clock, SystemClock};
pub use containers::ContainerBackend;
pub use desktop::{
    DesktopBackend, DesktopEnvironmentDetector, DesktopSnapshot, DesktopWindowSnapshot,
    DesktopWorkspaceSnapshot,
};
pub use editor::EditorBackend;
pub use emulator::EmulatorBackend;
pub use filesystem::FileSystem;
pub use persistence::{ConfigStore, StateStore};
pub use platform::{PlatformDetector, PlatformProbe};
pub use process::{
    BackgroundProcess, BoxFuture, ProcessOutput, ProcessOutputChunk, ProcessRequest, ProcessRunner,
    ProcessStream,
};
pub use terminal::TerminalBackend;
