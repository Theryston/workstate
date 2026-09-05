use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use crate::error::{ErrorCategory, Result, WorkstateError};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRequest {
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ProcessOutput {
    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutputChunk {
    pub stream: ProcessStream,
    pub bytes: Vec<u8>,
}

pub trait ProcessOutputSink: Send + Sync {
    fn emit<'a>(&'a self, chunk: ProcessOutputChunk) -> BoxFuture<'a, Result<()>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundProcess {
    pub identity: String,
}

impl BackgroundProcess {
    pub fn new(identity: impl Into<String>) -> Result<Self> {
        let identity = identity.into();
        if identity.is_empty() || identity.contains('\0') {
            return Err(WorkstateError::new(
                ErrorCategory::Process,
                "background process identity must be non-empty and contain no NUL characters",
            ));
        }
        Ok(Self { identity })
    }
}

pub trait ProcessRunner: Send + Sync {
    fn run<'a>(&'a self, request: ProcessRequest) -> BoxFuture<'a, Result<ProcessOutput>>;

    fn run_with_output<'a>(
        &'a self,
        request: ProcessRequest,
        output: Arc<dyn ProcessOutputSink>,
    ) -> BoxFuture<'a, Result<ProcessOutput>> {
        Box::pin(async move {
            let result = self.run(request).await?;
            if !result.stdout.is_empty() {
                output
                    .emit(ProcessOutputChunk {
                        stream: ProcessStream::Stdout,
                        bytes: result.stdout.clone(),
                    })
                    .await?;
            }
            if !result.stderr.is_empty() {
                output
                    .emit(ProcessOutputChunk {
                        stream: ProcessStream::Stderr,
                        bytes: result.stderr.clone(),
                    })
                    .await?;
            }
            Ok(result)
        })
    }

    fn start_background<'a>(
        &'a self,
        _request: ProcessRequest,
    ) -> BoxFuture<'a, Result<BackgroundProcess>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Process,
                "background process handoff is not configured",
            ))
        })
    }

    fn stop_background<'a>(&'a self, _process: BackgroundProcess) -> BoxFuture<'a, Result<()>> {
        Box::pin(async {
            Err(WorkstateError::new(
                ErrorCategory::Process,
                "background process cleanup is not configured",
            ))
        })
    }
}
