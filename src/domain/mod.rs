pub mod action;
pub mod environment;
pub mod error;
pub mod graph;
pub mod ownership;
pub mod resource;
pub mod runtime_state;
pub mod workspace;

pub use action::{
    ActionId, ActionKind, ActionParameters, ActionSpec, CleanupPolicy, CommandSpec, ComposeSpec,
    ContainerMount, ContainerPort, ContainerSpec, CustomParameters, EmulatorSpec, ExecutionMode,
    ReadinessCheck, RetryPolicy, Timeout,
};
pub use environment::{
    CURRENT_SCHEMA_VERSION, EnvironmentConfig, EnvironmentName, EnvironmentSlug,
};
pub use error::DomainError;
pub use graph::ActionGraph;
pub use ownership::{
    CleanupStatus, CompensationOperation, MutationRecord, OwnershipStatus, ResourceRecord,
    RestorationStatus, RunStatus,
};
pub use resource::{ResourceIdentity, ResourceKind};
pub use runtime_state::{CURRENT_RUNTIME_SCHEMA_VERSION, RuntimeState};
pub use workspace::{
    TilingPreference, WorkspaceId, WorkspaceReference, WorkspaceSpec, WorkspaceTarget,
};

pub(crate) fn validate_identifier(value: &str, kind: &str) -> Result<(), error::DomainError> {
    if value.is_empty()
        || !value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        || !value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
    {
        return Err(error::DomainError::InvalidIdentifier {
            kind: kind.to_owned(),
            value: value.to_owned(),
        });
    }

    Ok(())
}
