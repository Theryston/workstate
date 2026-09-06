use thiserror::Error;

use crate::error::{ErrorCategory, WorkstateError};

#[derive(Debug, Error)]
pub enum CosmicError {
    #[error("COSMIC operation '{operation}' could not connect to Wayland: {detail}")]
    ConnectionFailed { operation: String, detail: String },
    #[error(
        "COSMIC operation '{operation}' requires protocol global '{global}', but it was not advertised"
    )]
    RequiredGlobalMissing { operation: String, global: String },
    #[error(
        "COSMIC operation '{operation}' requires protocol '{protocol}' version {required}, but the compositor advertised {advertised:?}"
    )]
    ProtocolVersionUnsupported {
        operation: String,
        protocol: String,
        required: u32,
        advertised: Option<u32>,
    },
    #[error("COSMIC operation '{operation}' could not synchronize its initial state: {detail}")]
    InitialSynchronizationFailed { operation: String, detail: String },
    #[error(
        "COSMIC operation '{operation}' did not receive its initial state before the {timeout_ms} ms deadline"
    )]
    InitialSynchronizationTimedOut { operation: String, timeout_ms: u64 },
    #[error("COSMIC operation '{operation}' failed while dispatching Wayland events: {detail}")]
    WaylandDispatchFailed { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' failed while flushing Wayland requests: {detail}")]
    WaylandFlushFailed { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' could not find workspace '{identity}'")]
    WorkspaceNotFound { operation: String, identity: String },
    #[error(
        "COSMIC operation '{operation}' found more than one workspace matching '{identity}' ({matches} matches)"
    )]
    WorkspaceAmbiguous {
        operation: String,
        identity: String,
        matches: usize,
    },
    #[error("COSMIC operation '{operation}' could not find window '{identity}'")]
    WindowNotFound { operation: String, identity: String },
    #[error(
        "COSMIC operation '{operation}' found more than one window matching '{identity}' ({matches} matches)"
    )]
    WindowAmbiguous {
        operation: String,
        identity: String,
        matches: usize,
    },
    #[error(
        "COSMIC operation '{operation}' cannot continue because capability '{capability}' is unavailable: {detail}"
    )]
    CapabilityUnavailable {
        operation: String,
        capability: String,
        detail: String,
    },
    #[error("COSMIC operation '{operation}' could not confirm mutation for '{identity}': {detail}")]
    MutationNotConfirmed {
        operation: String,
        identity: String,
        detail: String,
    },
    #[error("COSMIC operation '{operation}' received invalid protocol data: {detail}")]
    InvalidProtocolData { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' exceeded its {timeout_ms} ms deadline")]
    OperationTimedOut { operation: String, timeout_ms: u64 },
    #[error("COSMIC operation '{operation}' failed in its blocking session: {detail}")]
    BlockingTaskFailed { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' failed: {detail}")]
    CommandFailed { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' returned malformed data: {detail}")]
    MalformedOutput { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' returned incomplete data: {detail}")]
    IncompleteOutput { operation: String, detail: String },
    #[error("COSMIC operation '{operation}' is unavailable: {detail}")]
    Unavailable { operation: String, detail: String },
}

impl CosmicError {
    pub fn into_workstate(self) -> WorkstateError {
        let message = self.to_string();
        WorkstateError::with_source(self.category(), message, self)
    }

    fn category(&self) -> ErrorCategory {
        match self {
            Self::RequiredGlobalMissing { .. }
            | Self::ProtocolVersionUnsupported { .. }
            | Self::CapabilityUnavailable { .. } => ErrorCategory::Platform,
            Self::ConnectionFailed { .. }
            | Self::InitialSynchronizationFailed { .. }
            | Self::InitialSynchronizationTimedOut { .. }
            | Self::WaylandDispatchFailed { .. }
            | Self::WaylandFlushFailed { .. }
            | Self::WorkspaceNotFound { .. }
            | Self::WorkspaceAmbiguous { .. }
            | Self::WindowNotFound { .. }
            | Self::WindowAmbiguous { .. }
            | Self::MutationNotConfirmed { .. }
            | Self::InvalidProtocolData { .. }
            | Self::OperationTimedOut { .. }
            | Self::BlockingTaskFailed { .. }
            | Self::CommandFailed { .. }
            | Self::MalformedOutput { .. }
            | Self::IncompleteOutput { .. }
            | Self::Unavailable { .. } => ErrorCategory::Integration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CosmicError;
    use crate::error::ErrorCategory;

    #[test]
    fn native_capability_errors_map_to_the_platform_category() {
        let error = CosmicError::CapabilityUnavailable {
            operation: "observe".to_owned(),
            capability: "workspace enumeration".to_owned(),
            detail: "the required global was not advertised".to_owned(),
        };

        let workstate_error = error.into_workstate();

        assert_eq!(workstate_error.category, ErrorCategory::Platform);
        assert!(workstate_error.message.contains("workspace enumeration"));
    }

    #[test]
    fn native_transport_errors_map_to_the_integration_category() {
        let error = CosmicError::ConnectionFailed {
            operation: "observe".to_owned(),
            detail: "WAYLAND_DISPLAY is unavailable".to_owned(),
        };

        let workstate_error = error.into_workstate();

        assert_eq!(workstate_error.category, ErrorCategory::Integration);
        assert!(workstate_error.message.contains("WAYLAND_DISPLAY"));
    }

    #[test]
    fn legacy_unavailable_errors_keep_the_existing_category() {
        let error = CosmicError::Unavailable {
            operation: "create-workspace".to_owned(),
            detail: "workspace creation is not supported".to_owned(),
        };

        let workstate_error = error.into_workstate();

        assert_eq!(workstate_error.category, ErrorCategory::Integration);
    }
}
