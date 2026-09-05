use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::Path,
};

use crate::{
    application::ports::{
        DockerActionContext, DockerComposeServiceSnapshot, DockerComposeSnapshot,
        DockerContainerRequest, DockerContainerSnapshot, DockerHealthState, DockerMountSnapshot,
        DockerOperationStatus,
    },
    domain::{
        CleanupPolicy, CommandSpec, OwnershipStatus, ResourceIdentity, ResourceKind, ResourceRecord,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

pub const CONTAINER_CLEANUP_OPERATION: &str = "cleanup_operation";
pub const CONTAINER_CLEANUP_REMOVE: &str = "remove";
pub const CONTAINER_CLEANUP_STOP: &str = "stop";

pub fn container_matches(
    request: &DockerContainerRequest,
    snapshot: &DockerContainerSnapshot,
) -> bool {
    if snapshot.name != request.specification.name {
        return false;
    }
    if request.specification.image.as_ref() != snapshot.image.as_ref()
        && request.specification.image.is_some()
    {
        return false;
    }
    if let Some(command) = &request.specification.command
        && snapshot.command.as_ref() != Some(command)
    {
        return false;
    }
    if !request.specification.environment.is_empty()
        && request.specification.environment != snapshot.environment
    {
        return false;
    }
    if !request.specification.mounts.is_empty()
        && request.specification.mounts.len() != snapshot.mounts.len()
    {
        return false;
    }
    if request.specification.mounts.iter().any(|mount| {
        !snapshot.mounts.iter().any(|candidate| {
            candidate.source == mount.source
                && candidate.target == mount.target
                && candidate.read_only == mount.read_only
        })
    }) {
        return false;
    }
    if !request.specification.ports.is_empty()
        && request.specification.ports.len() != snapshot.ports.len()
    {
        return false;
    }
    request.specification.ports.iter().all(|port| {
        snapshot.ports.iter().any(|candidate| {
            candidate.host == port.host
                && candidate.container == port.container
                && candidate.protocol == port.protocol
        })
    })
}

pub fn container_configuration_key(request: &DockerContainerRequest) -> String {
    let environment = request
        .specification
        .environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let mounts = request
        .specification
        .mounts
        .iter()
        .map(|mount| format!("{}:{}:{}", mount.source, mount.target, mount.read_only))
        .collect();
    let ports = request
        .specification
        .ports
        .iter()
        .map(|port| format!("{}:{}:{}", port.host, port.container, port.protocol))
        .collect();
    hash_configuration(
        &request.specification.name,
        request.specification.image.as_deref(),
        &command_key(request.specification.command.as_ref()),
        environment,
        mounts,
        ports,
    )
}

pub fn snapshot_configuration_key(snapshot: &DockerContainerSnapshot) -> String {
    let environment = snapshot
        .environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let mounts = snapshot
        .mounts
        .iter()
        .map(|mount| format!("{}:{}:{}", mount.source, mount.target, mount.read_only))
        .collect();
    let ports = snapshot
        .ports
        .iter()
        .map(|port| format!("{}:{}:{}", port.host, port.container, port.protocol))
        .collect();
    hash_configuration(
        &snapshot.name,
        snapshot.image.as_deref(),
        &command_key(snapshot.command.as_ref()),
        environment,
        mounts,
        ports,
    )
}

fn hash_configuration(
    name: &str,
    image: Option<&str>,
    command: &str,
    mut environment: Vec<String>,
    mut mounts: Vec<String>,
    mut ports: Vec<String>,
) -> String {
    environment.sort();
    mounts.sort();
    ports.sort();
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    image.hash(&mut hasher);
    command.hash(&mut hasher);
    environment.hash(&mut hasher);
    mounts.hash(&mut hasher);
    ports.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn command_key(command: Option<&CommandSpec>) -> String {
    command.map(CommandSpec::display_line).unwrap_or_default()
}

pub fn container_record(
    context: &DockerActionContext,
    request: &DockerContainerRequest,
    snapshot: &DockerContainerSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
) -> Result<ResourceRecord> {
    let cleanup_operation = ownership
        .is_environment_owned()
        .then_some(CONTAINER_CLEANUP_REMOVE);
    container_record_with_cleanup(
        context,
        request,
        snapshot,
        ownership,
        observed_before,
        cleanup_operation,
    )
}

pub fn container_record_with_cleanup(
    context: &DockerActionContext,
    request: &DockerContainerRequest,
    snapshot: &DockerContainerSnapshot,
    ownership: OwnershipStatus,
    observed_before: bool,
    cleanup_operation: Option<&str>,
) -> Result<ResourceRecord> {
    let identity = ResourceIdentity::new(ResourceKind::DockerContainer, snapshot.id.clone())
        .map_err(WorkstateError::from)?;
    let mut record =
        ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
    record.observed_before = observed_before;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("container_name".to_owned(), snapshot.name.clone());
    record.integration_metadata.insert(
        "configuration_key".to_owned(),
        container_configuration_key(request),
    );
    record
        .integration_metadata
        .insert("environment".to_owned(), context.environment.to_string());
    if let Some(operation) = cleanup_operation {
        record
            .integration_metadata
            .insert(CONTAINER_CLEANUP_OPERATION.to_owned(), operation.to_owned());
    }
    Ok(record)
}

pub fn compose_project_identity(project_name: &str, working_directory: &Path) -> String {
    format!("{project_name}@{}", working_directory.display())
}

pub fn compose_record(
    context: &DockerActionContext,
    snapshot: &DockerComposeSnapshot,
    ownership: OwnershipStatus,
) -> Result<ResourceRecord> {
    let stable_identity =
        compose_project_identity(&snapshot.project_name, &snapshot.working_directory);
    let identity = ResourceIdentity::new(ResourceKind::DockerCompose, stable_identity)
        .map_err(WorkstateError::from)?;
    let mut record =
        ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
    record.observed_before = ownership != OwnershipStatus::CreatedByCurrentRun;
    record.cleanup_policy = context.cleanup_policy;
    record
        .integration_metadata
        .insert("project_name".to_owned(), snapshot.project_name.clone());
    record.integration_metadata.insert(
        "working_directory".to_owned(),
        snapshot.working_directory.display().to_string(),
    );
    record
        .integration_metadata
        .insert("environment".to_owned(), context.environment.to_string());
    Ok(record)
}

pub fn compose_service_records(
    context: &DockerActionContext,
    snapshot: &DockerComposeSnapshot,
    ownership: OwnershipStatus,
) -> Result<Vec<ResourceRecord>> {
    let cleanup_operation = ownership
        .is_environment_owned()
        .then_some(CONTAINER_CLEANUP_REMOVE);
    compose_service_records_with_cleanup(context, snapshot, ownership, cleanup_operation)
}

pub fn compose_service_records_with_cleanup(
    context: &DockerActionContext,
    snapshot: &DockerComposeSnapshot,
    ownership: OwnershipStatus,
    cleanup_operation: Option<&str>,
) -> Result<Vec<ResourceRecord>> {
    snapshot
        .services
        .iter()
        .filter_map(|service| service.container_id.as_ref().map(|id| (service, id)))
        .map(|(service, container_id)| {
            let identity =
                ResourceIdentity::new(ResourceKind::DockerContainer, container_id.clone())
                    .map_err(WorkstateError::from)?;
            let mut record =
                ResourceRecord::new(identity, ownership).with_action(context.action_id.clone());
            record.observed_before = ownership != OwnershipStatus::CreatedByCurrentRun
                || cleanup_operation == Some(CONTAINER_CLEANUP_STOP);
            record.cleanup_policy = context.cleanup_policy;
            record
                .integration_metadata
                .insert("compose_project".to_owned(), snapshot.project_name.clone());
            record.integration_metadata.insert(
                "compose_working_directory".to_owned(),
                snapshot.working_directory.display().to_string(),
            );
            record
                .integration_metadata
                .insert("service_name".to_owned(), service.name.clone());
            if let Some(operation) = cleanup_operation {
                record
                    .integration_metadata
                    .insert(CONTAINER_CLEANUP_OPERATION.to_owned(), operation.to_owned());
            }
            Ok(record)
        })
        .collect()
}

pub fn compose_service_is_ready(service: &DockerComposeServiceSnapshot) -> bool {
    service.state.is_running() && service.health.satisfies_readiness()
}

pub fn operation_changed(status: DockerOperationStatus) -> bool {
    matches!(
        status,
        DockerOperationStatus::Created | DockerOperationStatus::Repaired
    )
}

pub fn record_matches_snapshot(
    record: &ResourceRecord,
    request: &DockerContainerRequest,
    snapshot: &DockerContainerSnapshot,
) -> bool {
    record
        .integration_metadata
        .get("container_name")
        .is_some_and(|name| name == &snapshot.name)
        && container_matches(request, snapshot)
        && record
            .integration_metadata
            .get("configuration_key")
            .is_some_and(|key| key == &container_configuration_key(request))
}

pub fn healthy_container(snapshot: &DockerContainerSnapshot) -> bool {
    snapshot.state.is_running() && snapshot.health.satisfies_readiness()
}

pub fn cleanup_policy(value: Option<&str>) -> CleanupPolicy {
    match value {
        Some("preserve") => CleanupPolicy::Preserve,
        _ => CleanupPolicy::OwnedOnly,
    }
}

pub fn health_from_label(value: Option<&str>) -> DockerHealthState {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "healthy" => DockerHealthState::Healthy,
        "unhealthy" => DockerHealthState::Unhealthy,
        "starting" => DockerHealthState::Starting,
        "" => DockerHealthState::None,
        value => DockerHealthState::Unknown(value.to_owned()),
    }
}

pub fn invalid_snapshot(message: impl Into<String>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Integration, message)
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn mount_snapshot(
    source: impl Into<String>,
    target: impl Into<String>,
    read_only: bool,
) -> DockerMountSnapshot {
    DockerMountSnapshot {
        source: source.into(),
        target: target.into(),
        read_only,
    }
}
