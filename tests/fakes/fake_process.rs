use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use workstate::{
    application::ports::{
        BackgroundProcess, BoxFuture, ProcessOutput, ProcessRequest, ProcessRunner,
    },
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Clone, Default)]
pub struct FakeProcessRunner {
    state: Arc<Mutex<FakeProcessState>>,
}

#[derive(Default)]
struct FakeProcessState {
    requests: Vec<ProcessRequest>,
    responses: VecDeque<ProcessOutput>,
    background_requests: Vec<ProcessRequest>,
    next_identity: usize,
}

impl FakeProcessRunner {
    pub fn with_responses(responses: impl IntoIterator<Item = ProcessOutput>) -> Self {
        let runner = Self::default();
        if let Ok(mut state) = runner.state.lock() {
            state.responses.extend(responses);
        }
        runner
    }

    pub fn requests(&self) -> Result<Vec<ProcessRequest>> {
        self.state
            .lock()
            .map(|state| state.requests.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake process lock failed"))
    }

    #[allow(dead_code)]
    pub fn background_requests(&self) -> Result<Vec<ProcessRequest>> {
        self.state
            .lock()
            .map(|state| state.background_requests.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake process lock failed"))
    }

    fn record_request(&self, request: ProcessRequest, background: bool) -> Result<ProcessOutput> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake process lock failed"))?;
        if background {
            state.background_requests.push(request.clone());
        } else {
            state.requests.push(request);
        }
        Ok(state
            .responses
            .pop_front()
            .unwrap_or_else(|| ProcessOutput {
                status: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }))
    }
}

impl ProcessRunner for FakeProcessRunner {
    fn run<'a>(&'a self, request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>> {
        let result = self.record_request(request, false);
        Box::pin(async move { result })
    }

    fn start_background<'a>(
        &'a self,
        request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        let result = self
            .state
            .lock()
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "fake process lock failed"))
            .and_then(|mut state| {
                state.background_requests.push(request);
                let identity = format!("fake-background-{}", state.next_identity);
                state.next_identity = state.next_identity.saturating_add(1);
                BackgroundProcess::new(identity)
            });
        Box::pin(async move { result })
    }
}
