#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use workstate::{
    application::{
        planner::CancellationToken,
        ports::{
            BoxFuture, DockerBackend, DockerCheckReport, DockerCleanupRequest,
            DockerComposeObservation, DockerComposeRequest, DockerComposeServiceSnapshot,
            DockerComposeSnapshot, DockerContainerObservation, DockerContainerRequest,
            DockerContainerSnapshot, DockerContainerState, DockerEngineRequest,
            DockerEngineSnapshot, DockerEnsureOutcome, DockerHealthState, DockerMountSnapshot,
            DockerOperationStatus, DockerPortSnapshot,
        },
    },
    domain::{OwnershipStatus, ResourceIdentity, ResourceKind, ResourceRecord},
    error::{ErrorCategory, Result, WorkstateError},
    integrations::docker::models,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerCall {
    InspectEngine,
    EnsureEngine,
    ObserveContainer(String),
    EnsureContainer(String),
    ObserveCompose(String),
    EnsureCompose(String),
    StopOwned,
}

#[derive(Clone)]
pub struct FakeDocker {
    state: Arc<Mutex<FakeDockerState>>,
}

struct FakeDockerState {
    engine: DockerEngineSnapshot,
    containers: BTreeMap<String, DockerContainerSnapshot>,
    compose: BTreeMap<String, DockerComposeSnapshot>,
    calls: Vec<DockerCall>,
    next_container: usize,
}

impl Default for FakeDocker {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeDockerState {
                engine: DockerEngineSnapshot::ready("fake"),
                containers: BTreeMap::new(),
                compose: BTreeMap::new(),
                calls: Vec::new(),
                next_container: 1,
            })),
        }
    }
}

impl FakeDocker {
    pub fn with_engine(self, engine: DockerEngineSnapshot) -> Self {
        if let Ok(mut state) = self.state.lock() {
            state.engine = engine;
        }
        self
    }

    pub fn insert_container(&self, snapshot: DockerContainerSnapshot) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| lock_error())
            .map(|mut state| {
                state.containers.insert(snapshot.name.clone(), snapshot);
            })
    }

    pub fn insert_compose(&self, snapshot: DockerComposeSnapshot) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| lock_error())
            .map(|mut state| {
                let key = models::compose_project_identity(
                    &snapshot.project_name,
                    &snapshot.working_directory,
                );
                state.compose.insert(key, snapshot);
            })
    }

    pub fn calls(&self) -> Result<Vec<DockerCall>> {
        self.state
            .lock()
            .map(|state| state.calls.clone())
            .map_err(|_| lock_error())
    }

    pub fn container(&self, name: &str) -> Result<Option<DockerContainerSnapshot>> {
        self.state
            .lock()
            .map(|state| state.containers.get(name).cloned())
            .map_err(|_| lock_error())
    }

    pub fn compose_project(
        &self,
        project_name: &str,
        working_directory: &std::path::Path,
    ) -> Result<Option<DockerComposeSnapshot>> {
        let key = models::compose_project_identity(project_name, working_directory);
        self.state
            .lock()
            .map(|state| state.compose.get(&key).cloned())
            .map_err(|_| lock_error())
    }

    fn project_key(request: &DockerComposeRequest) -> String {
        let name = request
            .working_directory
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "fake-project".to_owned());
        models::compose_project_identity(&name, &request.working_directory)
    }

    fn container_record(
        request: &DockerContainerRequest,
        snapshot: &DockerContainerSnapshot,
        ownership: OwnershipStatus,
    ) -> Result<ResourceRecord> {
        let identity = ResourceIdentity::new(ResourceKind::DockerContainer, snapshot.id.clone())
            .map_err(WorkstateError::from)?;
        Ok(ResourceRecord::new(identity, ownership).with_action(request.context.action_id.clone()))
    }

    fn compose_resources(
        request: &DockerComposeRequest,
        snapshot: &DockerComposeSnapshot,
        ownership: OwnershipStatus,
    ) -> Result<Vec<ResourceRecord>> {
        let mut resources = vec![models::compose_record(
            &request.context,
            snapshot,
            ownership,
        )?];
        resources.extend(models::compose_service_records(
            &request.context,
            snapshot,
            ownership,
        )?);
        Ok(resources)
    }
}

impl DockerBackend for FakeDocker {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn inspect_engine<'a>(
        &'a self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEngineSnapshot>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            state.calls.push(DockerCall::InspectEngine);
            Ok(state.engine.clone())
        })
    }

    fn ensure_engine_ready<'a>(
        &'a self,
        _request: DockerEngineRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            state.calls.push(DockerCall::EnsureEngine);
            if state.engine.ready {
                Ok(DockerEnsureOutcome::new(DockerOperationStatus::Reused))
            } else {
                Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "fake Docker Engine is unavailable",
                ))
            }
        })
    }

    fn observe_container<'a>(
        &'a self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerContainerObservation>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            state.calls.push(DockerCall::ObserveContainer(
                request.specification.name.clone(),
            ));
            if !state.engine.ready {
                return Ok(DockerContainerObservation::Unavailable(
                    state.engine.clone(),
                ));
            }
            Ok(state
                .containers
                .get(&request.specification.name)
                .cloned()
                .map_or(DockerContainerObservation::Missing, |snapshot| {
                    DockerContainerObservation::Present(Box::new(snapshot))
                }))
        })
    }

    fn ensure_container<'a>(
        &'a self,
        request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            let name = request.specification.name.clone();
            state.calls.push(DockerCall::EnsureContainer(name.clone()));
            if !state.engine.ready {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "fake Docker Engine is unavailable",
                ));
            }
            if let Some(snapshot) = state.containers.get(&name).cloned() {
                if !models::container_matches(&request, &snapshot) {
                    return Err(WorkstateError::new(
                        ErrorCategory::Integration,
                        "fake Docker container configuration conflict",
                    ));
                }
                let status = if models::healthy_container(&snapshot) {
                    DockerOperationStatus::Reused
                } else {
                    DockerOperationStatus::Repaired
                };
                let record =
                    Self::container_record(&request, &snapshot, OwnershipStatus::ReusedExisting)?;
                return Ok(DockerEnsureOutcome::new(status).with_resources(vec![record]));
            }
            let Some(image) = request.specification.image.clone() else {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "fake Docker container image is required",
                ));
            };
            let id = format!("fake-container-{}", state.next_container);
            state.next_container = state.next_container.saturating_add(1);
            let snapshot = DockerContainerSnapshot {
                id,
                name: name.clone(),
                image: Some(image),
                command: request.specification.command.clone(),
                working_directory: None,
                environment: request.specification.environment.clone(),
                mounts: request
                    .specification
                    .mounts
                    .iter()
                    .map(|mount| DockerMountSnapshot {
                        source: mount.source.clone(),
                        target: mount.target.clone(),
                        read_only: mount.read_only,
                    })
                    .collect(),
                ports: request
                    .specification
                    .ports
                    .iter()
                    .map(|port| DockerPortSnapshot {
                        host: port.host,
                        container: port.container,
                        protocol: port.protocol.clone(),
                    })
                    .collect(),
                state: DockerContainerState::Running,
                health: DockerHealthState::None,
                exit_code: None,
                status: Some("running".to_owned()),
            };
            state.containers.insert(name, snapshot.clone());
            let record =
                Self::container_record(&request, &snapshot, OwnershipStatus::CreatedByCurrentRun)?;
            Ok(DockerEnsureOutcome::new(DockerOperationStatus::Created)
                .with_resources(vec![record]))
        })
    }

    fn observe_compose<'a>(
        &'a self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerComposeObservation>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            let key = Self::project_key(&request);
            state.calls.push(DockerCall::ObserveCompose(key.clone()));
            if !state.engine.ready {
                return Ok(DockerComposeObservation::Unavailable(state.engine.clone()));
            }
            Ok(state.compose.get(&key).cloned().map_or(
                DockerComposeObservation::Missing,
                DockerComposeObservation::Present,
            ))
        })
    }

    fn ensure_compose<'a>(
        &'a self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            let key = Self::project_key(&request);
            state.calls.push(DockerCall::EnsureCompose(key.clone()));
            if !state.engine.ready {
                return Err(WorkstateError::new(
                    ErrorCategory::Integration,
                    "fake Docker Engine is unavailable",
                ));
            }
            if let Some(snapshot) = state.compose.get(&key).cloned()
                && snapshot.is_healthy(&request.specification.services)
            {
                return Ok(
                    DockerEnsureOutcome::new(DockerOperationStatus::Reused).with_resources(
                        Self::compose_resources(
                            &request,
                            &snapshot,
                            OwnershipStatus::ReusedExisting,
                        )?,
                    ),
                );
            }
            let project_name = request
                .working_directory
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| "fake-project".to_owned());
            let service_names = if request.specification.services.is_empty() {
                vec!["default".to_owned()]
            } else {
                request.specification.services.clone()
            };
            let services = service_names
                .into_iter()
                .enumerate()
                .map(|(index, name)| DockerComposeServiceSnapshot {
                    name,
                    container_id: Some(format!("fake-compose-container-{index}")),
                    state: DockerContainerState::Running,
                    health: DockerHealthState::None,
                })
                .collect();
            let snapshot = DockerComposeSnapshot {
                project_name,
                working_directory: request.working_directory.clone(),
                services,
            };
            state.compose.insert(key, snapshot.clone());
            Ok(
                DockerEnsureOutcome::new(DockerOperationStatus::Created).with_resources(
                    Self::compose_resources(
                        &request,
                        &snapshot,
                        OwnershipStatus::CreatedByCurrentRun,
                    )?,
                ),
            )
        })
    }

    fn check_readiness<'a>(
        &'a self,
        _request: DockerContainerRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async move {
            cancellation.check()?;
            Ok(DockerCheckReport {
                checks_run: 0,
                last_detail: None,
            })
        })
    }

    fn check_compose_readiness<'a>(
        &'a self,
        _request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerCheckReport>> {
        Box::pin(async move {
            cancellation.check()?;
            Ok(DockerCheckReport {
                checks_run: 0,
                last_detail: None,
            })
        })
    }

    fn stop_owned<'a>(
        &'a self,
        request: DockerCleanupRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<DockerEnsureOutcome>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cancellation.check()?;
            let mut state = state.lock().map_err(|_| lock_error())?;
            state.calls.push(DockerCall::StopOwned);
            for resource in request
                .resources
                .iter()
                .filter(|resource| resource.is_cleanup_candidate())
            {
                match resource.resource.kind {
                    ResourceKind::DockerContainer => {
                        state
                            .containers
                            .retain(|_, snapshot| snapshot.id != resource.resource.stable_identity);
                    }
                    ResourceKind::DockerCompose => {
                        state
                            .compose
                            .retain(|key, _| key != &resource.resource.stable_identity);
                    }
                    _ => {}
                }
            }
            Ok(DockerEnsureOutcome::new(DockerOperationStatus::Repaired))
        })
    }
}

fn lock_error() -> WorkstateError {
    WorkstateError::new(ErrorCategory::Runtime, "fake Docker state lock failed")
}

pub fn container_snapshot(name: &str, image: &str, running: bool) -> DockerContainerSnapshot {
    DockerContainerSnapshot {
        id: format!("fake-{name}"),
        name: name.to_owned(),
        image: Some(image.to_owned()),
        command: None,
        working_directory: None,
        environment: BTreeMap::new(),
        mounts: Vec::new(),
        ports: Vec::new(),
        state: if running {
            DockerContainerState::Running
        } else {
            DockerContainerState::Exited
        },
        health: DockerHealthState::None,
        exit_code: None,
        status: Some(if running { "running" } else { "exited" }.to_owned()),
    }
}

pub fn compose_snapshot(
    project_name: &str,
    working_directory: PathBuf,
    services: &[(&str, bool)],
) -> DockerComposeSnapshot {
    DockerComposeSnapshot {
        project_name: project_name.to_owned(),
        working_directory,
        services: services
            .iter()
            .enumerate()
            .map(|(index, (name, running))| DockerComposeServiceSnapshot {
                name: (*name).to_owned(),
                container_id: Some(format!("fake-compose-{index}")),
                state: if *running {
                    DockerContainerState::Running
                } else {
                    DockerContainerState::Exited
                },
                health: DockerHealthState::None,
            })
            .collect(),
    }
}
