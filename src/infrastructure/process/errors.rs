use crate::{
    application::ports::ProcessStream,
    error::{ErrorCategory, WorkstateError},
};

pub(crate) fn invalid_working_directory(path: &std::path::Path) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Process,
        "the configured working directory is invalid",
    )
    .with_context("working_directory", path.display().to_string())
}

pub(crate) fn missing_working_directory(
    path: &std::path::Path,
    source: impl std::error::Error + Send + Sync + 'static,
) -> WorkstateError {
    WorkstateError::with_source(
        ErrorCategory::Process,
        "could not inspect the configured working directory",
        source,
    )
    .with_context("working_directory", path.display().to_string())
}

pub(crate) fn output_read_error(
    stream: ProcessStream,
    source: impl std::error::Error + Send + Sync + 'static,
) -> WorkstateError {
    WorkstateError::with_source(
        ErrorCategory::Process,
        "could not read process output",
        source,
    )
    .with_context("stream", stream_name(stream))
}

pub(crate) fn output_sink_error(stream: ProcessStream, error: WorkstateError) -> WorkstateError {
    error
        .with_context("stream", stream_name(stream))
        .with_context("phase", "process output streaming")
}

fn stream_name(stream: ProcessStream) -> &'static str {
    match stream {
        ProcessStream::Stdout => "stdout",
        ProcessStream::Stderr => "stderr",
    }
}
