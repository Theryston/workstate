pub mod applications;
pub mod clock;
pub mod containers;
pub mod desktop;
pub mod directories;
pub mod docker;
pub mod editor;
pub mod emulator;
pub mod files;
pub mod filesystem;
pub mod persistence;
pub mod platform;
pub mod process;
pub mod terminal;
pub mod tmux;

pub use applications::{ApplicationCatalog, InstalledApplication};
pub use clock::{Clock, SystemClock};
pub use containers::ContainerBackend;
pub use desktop::{
    DesktopBackend, DesktopEnvironmentDetector, DesktopOperationOutcome, DesktopOperationStatus,
    DesktopSnapshot, DesktopWindowSnapshot, DesktopWorkspaceResolution, DesktopWorkspaceSnapshot,
    ensure_workspace, resolve_workspace_target, resolve_workspace_target_with_reservations,
};
pub use directories::{DirectoryCatalog, DirectoryCompletion, DirectorySuggestion};
pub use docker::{
    DockerActionContext, DockerBackend, DockerCheckReport, DockerCleanupRequest,
    DockerComposeObservation, DockerComposeRequest, DockerComposeServiceSnapshot,
    DockerComposeSnapshot, DockerContainerObservation, DockerContainerRequest,
    DockerContainerSnapshot, DockerContainerState, DockerEngineRequest, DockerEngineSnapshot,
    DockerEnsureOutcome, DockerHealthState, DockerMountSnapshot, DockerOperationStatus,
    DockerPortSnapshot,
};
pub use editor::{EditorBackend, EditorOpenOutcome, EditorOperationStatus, EditorWindowSnapshot};
pub use emulator::EmulatorBackend;
pub use files::FileCatalog;
pub use filesystem::FileSystem;
pub use persistence::{ConfigStore, StateStore};
pub use platform::{PlatformDetector, PlatformProbe};
pub use process::{
    BackgroundProcess, BoxFuture, ProcessOutput, ProcessOutputChunk, ProcessOutputSink,
    ProcessRequest, ProcessRunner, ProcessStream,
};
pub use terminal::TerminalBackend;
pub use tmux::{TmuxBackend, TmuxSessionSnapshot, TmuxWindowRequest, TmuxWindowSnapshot};
