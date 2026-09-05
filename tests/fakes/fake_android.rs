#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use workstate::{
    application::{
        planner::CancellationToken,
        ports::{
            AndroidVirtualDevice, BoxFuture, EmulatorBackend, EmulatorCleanupOutcome,
            EmulatorEnsureOutcome, EmulatorObservation, EmulatorRequest,
        },
    },
    domain::ResourceRecord,
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone, Default)]
pub struct FakeAndroid {
    state: Arc<Mutex<FakeAndroidState>>,
}

#[derive(Default)]
struct FakeAndroidState {
    avds: Vec<AndroidVirtualDevice>,
    observation: Option<EmulatorObservation>,
    ensure_outcome: Option<EmulatorEnsureOutcome>,
    cleanup_outcome: Option<EmulatorCleanupOutcome>,
    observed_requests: Vec<EmulatorRequest>,
    ensured_requests: Vec<EmulatorRequest>,
    cleanup_requests: Vec<(EmulatorRequest, Vec<ResourceRecord>)>,
}

impl FakeAndroid {
    pub fn with_avds(avds: impl IntoIterator<Item = AndroidVirtualDevice>) -> Self {
        let fake = Self::default();
        if let Ok(mut state) = fake.state.lock() {
            state.avds.extend(avds);
        }
        fake
    }

    pub fn set_observation(&self, observation: EmulatorObservation) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| state.observation = Some(observation))
            .map_err(|_| lock_error())
    }

    pub fn set_ensure_outcome(&self, outcome: EmulatorEnsureOutcome) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| state.ensure_outcome = Some(outcome))
            .map_err(|_| lock_error())
    }

    pub fn set_cleanup_outcome(&self, outcome: EmulatorCleanupOutcome) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| state.cleanup_outcome = Some(outcome))
            .map_err(|_| lock_error())
    }

    pub fn observed_requests(&self) -> Result<Vec<EmulatorRequest>> {
        self.state
            .lock()
            .map(|state| state.observed_requests.clone())
            .map_err(|_| lock_error())
    }

    pub fn ensured_requests(&self) -> Result<Vec<EmulatorRequest>> {
        self.state
            .lock()
            .map(|state| state.ensured_requests.clone())
            .map_err(|_| lock_error())
    }

    pub fn cleanup_requests(&self) -> Result<Vec<(EmulatorRequest, Vec<ResourceRecord>)>> {
        self.state
            .lock()
            .map(|state| state.cleanup_requests.clone())
            .map_err(|_| lock_error())
    }
}

impl EmulatorBackend for FakeAndroid {
    fn is_available(&self) -> Result<bool> {
        Ok(true)
    }

    fn list_avds(&self) -> BoxFuture<'_, Result<Vec<AndroidVirtualDevice>>> {
        let result = self
            .state
            .lock()
            .map(|state| state.avds.clone())
            .map_err(|_| lock_error());
        Box::pin(async move { result })
    }

    fn observe(
        &self,
        request: EmulatorRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorObservation>> {
        let result = self
            .state
            .lock()
            .map(|mut state| {
                state.observed_requests.push(request);
                state
                    .observation
                    .clone()
                    .unwrap_or(EmulatorObservation::Missing)
            })
            .map_err(|_| lock_error());
        Box::pin(async move { result })
    }

    fn ensure(
        &self,
        request: EmulatorRequest,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorEnsureOutcome>> {
        let result = self
            .state
            .lock()
            .map(|mut state| {
                state.ensured_requests.push(request);
                state.ensure_outcome.clone().unwrap_or_else(|| {
                    EmulatorEnsureOutcome::new(
                        workstate::application::ports::EmulatorOperationStatus::Ready,
                    )
                })
            })
            .map_err(|_| lock_error());
        Box::pin(async move { result })
    }

    fn stop_owned(
        &self,
        request: EmulatorRequest,
        resources: Vec<ResourceRecord>,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<EmulatorCleanupOutcome>> {
        let result = self
            .state
            .lock()
            .map(|mut state| {
                state.cleanup_requests.push((request, resources));
                state.cleanup_outcome.clone().unwrap_or_else(|| {
                    EmulatorCleanupOutcome::new(
                        workstate::application::ports::EmulatorOperationStatus::Ready,
                    )
                })
            })
            .map_err(|_| lock_error());
        Box::pin(async move { result })
    }
}

fn lock_error() -> WorkstateError {
    WorkstateError::new(ErrorCategory::Runtime, "fake Android state lock failed")
}
