use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("environment name must not be empty")]
    EmptyEnvironmentName,
    #[error("environment name contains a forbidden path or control character")]
    InvalidEnvironmentName,
    #[error("environment name cannot be . or ..")]
    ReservedEnvironmentName,
    #[error("environment name cannot produce a non-empty filesystem-safe slug")]
    EmptyEnvironmentSlug,
    #[error("environment slug is not lowercase and filesystem-safe: {value}")]
    InvalidEnvironmentSlug { value: String },
    #[error("unsupported environment schema version {actual}; expected {expected}")]
    UnsupportedEnvironmentSchema { actual: u32, expected: u32 },
    #[error("invalid {kind} identifier: {value}")]
    InvalidIdentifier { kind: String, value: String },
    #[error("duplicate workspace identifier: {id}")]
    DuplicateWorkspaceId { id: String },
    #[error("duplicate action identifier: {id}")]
    DuplicateActionId { id: String },
    #[error("action {action_id} depends on missing action {dependency_id}")]
    MissingDependency {
        action_id: String,
        dependency_id: String,
    },
    #[error("action {action_id} cannot depend on itself")]
    SelfDependency { action_id: String },
    #[error("action {action_id} contains duplicate dependency {dependency_id}")]
    DuplicateDependency {
        action_id: String,
        dependency_id: String,
    },
    #[error("action dependency cycle detected: {actions}")]
    DependencyCycle { actions: String },
    #[error("workspace target is invalid: {message}")]
    InvalidWorkspaceTarget { message: String },
    #[error("action {action_id} references missing workspace {workspace_id}")]
    MissingWorkspaceReference {
        action_id: String,
        workspace_id: String,
    },
    #[error("action {action_id} is missing required parameter {parameter}")]
    MissingActionParameter {
        action_id: String,
        parameter: String,
    },
    #[error("action {action_id} has an invalid parameter: {parameter}")]
    InvalidActionParameter {
        action_id: String,
        parameter: String,
    },
    #[error("action {action_id} has an invalid timeout: {message}")]
    InvalidActionTimeout { action_id: String, message: String },
    #[error("action {action_id} has an invalid retry policy")]
    InvalidRetryPolicy { action_id: String },
    #[error("action {action_id} has an invalid execution mode: {message}")]
    InvalidExecutionMode { action_id: String, message: String },
    #[error("action {action_id} has an invalid command: {message}")]
    InvalidCommand { action_id: String, message: String },
    #[error("action {action_id} has an invalid readiness check: {message}")]
    InvalidReadinessCheck { action_id: String, message: String },
    #[error("runtime state has an unsupported schema version {actual}; expected {expected}")]
    UnsupportedRuntimeSchema { actual: u32, expected: u32 },
    #[error("runtime state belongs to {actual_slug}, not {expected_slug}")]
    RuntimeEnvironmentMismatch {
        actual_slug: String,
        expected_slug: String,
    },
    #[error("runtime state already contains resource {identity}")]
    DuplicateRuntimeResource { identity: String },
    #[error("runtime state already contains mutation {target}")]
    DuplicateRuntimeMutation { target: String },
    #[error("graph invariant failed: {message}")]
    GraphInvariant { message: String },
    #[error("invalid lifecycle transition from {from} to {to}")]
    InvalidLifecycleTransition { from: String, to: String },
}
