use std::{
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    time::Duration,
};

use crate::{
    application::{
        planner::{CancellationToken, ReadinessCheckResult},
        ports::{BoxFuture, ProcessRequest, ProcessRunner},
    },
    domain::{ActionId, CommandSpec},
    error::{ErrorCategory, Result, WorkstateError},
};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn tcp_check(
    host: String,
    port: u16,
    timeout: Duration,
    cancellation: CancellationToken,
) -> Result<ReadinessCheckResult> {
    cancellation.check()?;
    let operation = tokio::task::spawn_blocking(move || {
        let address = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|source| {
                WorkstateError::with_source(
                    ErrorCategory::Integration,
                    "could not resolve the TCP readiness host",
                    source,
                )
            })?
            .next()
            .ok_or_else(|| {
                WorkstateError::new(
                    ErrorCategory::Integration,
                    "the TCP readiness host did not resolve to an address",
                )
            })?;
        TcpStream::connect_timeout(&address, timeout).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Integration,
                "TCP readiness connection failed",
                source,
            )
        })?;
        Ok::<(), WorkstateError>(())
    });
    let result = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "operation was cancelled during the TCP readiness check",
            ).with_context("cancelled", "true"));
        }
        result = tokio::time::timeout(timeout, operation) => result,
    };
    match result {
        Ok(Ok(Ok(()))) => Ok(ReadinessCheckResult::passed()),
        Ok(Ok(Err(error))) => Ok(ReadinessCheckResult::failed(error.message)),
        Ok(Err(error)) => Err(WorkstateError::new(
            ErrorCategory::Runtime,
            "TCP readiness worker failed",
        )
        .with_context("detail", error.to_string())),
        Err(_) => Ok(ReadinessCheckResult::failed("TCP connection timed out")),
    }
}

pub async fn http_check(
    runner: &dyn ProcessRunner,
    url: String,
    expected_status: Option<u16>,
    timeout: Duration,
    working_directory: Option<PathBuf>,
    cancellation: CancellationToken,
) -> Result<ReadinessCheckResult> {
    cancellation.check()?;
    let timeout_value = curl_timeout(timeout);
    let request = ProcessRequest {
        program: "curl".to_owned(),
        arguments: vec![
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--location".to_owned(),
            "--connect-timeout".to_owned(),
            timeout_value.clone(),
            "--max-time".to_owned(),
            timeout_value,
            "--output".to_owned(),
            "/dev/null".to_owned(),
            "--write-out".to_owned(),
            "%{http_code}".to_owned(),
            url.clone(),
        ],
        working_directory,
        environment: Vec::new(),
    };
    let output = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "operation was cancelled during the HTTP readiness check",
            ).with_context("cancelled", "true"));
        }
        result = tokio::time::timeout(timeout, runner.run(request)) => {
            match result {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return Err(error
                        .with_context("operation", "HTTP readiness check")
                        .with_context("url", redact_url(&url)));
                }
                Err(_) => return Ok(ReadinessCheckResult::failed("HTTP request timed out")),
            }
        }
    };
    if !output.succeeded() {
        return Ok(ReadinessCheckResult::failed(format!(
            "HTTP request failed for {}",
            redact_url(&url)
        )));
    }
    let status = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u16>()
        .map_err(|_| {
            WorkstateError::new(
                ErrorCategory::Integration,
                "HTTP readiness command returned an invalid status",
            )
        })?;
    let accepted =
        expected_status.map_or((200..400).contains(&status), |expected| status == expected);
    if accepted {
        Ok(ReadinessCheckResult::passed())
    } else {
        Ok(ReadinessCheckResult::failed(format!(
            "HTTP readiness returned status {status}"
        )))
    }
}

pub async fn command_check(
    runner: &dyn ProcessRunner,
    action_id: &ActionId,
    command: &CommandSpec,
    timeout: Duration,
    working_directory: Option<PathBuf>,
    cancellation: CancellationToken,
) -> Result<ReadinessCheckResult> {
    cancellation.check()?;
    let request = crate::infrastructure::process::command_spec::to_process_request(
        command,
        working_directory,
    )?;
    let output = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(WorkstateError::new(
                ErrorCategory::Runtime,
                "operation was cancelled during the command readiness check",
            ).with_context("cancelled", "true"));
        }
        result = tokio::time::timeout(timeout, runner.run(request)) => {
            match result {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(ReadinessCheckResult::failed("readiness command timed out")),
            }
        }
    };
    if output.succeeded() {
        return Ok(ReadinessCheckResult::passed());
    }
    let status = output
        .status
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    Ok(ReadinessCheckResult::failed(format!(
        "readiness command for action '{action_id}' exited with status {status}"
    )))
}

pub async fn delay(
    milliseconds: u64,
    cancellation: CancellationToken,
) -> Result<ReadinessCheckResult> {
    cancellation.check()?;
    tokio::select! {
        _ = cancellation.cancelled() => Err(WorkstateError::new(
            ErrorCategory::Runtime,
            "operation was cancelled during a Docker readiness delay",
        ).with_context("cancelled", "true")),
        _ = tokio::time::sleep(Duration::from_millis(milliseconds)) => Ok(ReadinessCheckResult::passed()),
    }
}

pub fn poll_interval(value: Duration) -> Duration {
    if value.is_zero() {
        DEFAULT_POLL_INTERVAL
    } else {
        value
    }
}

fn curl_timeout(timeout: Duration) -> String {
    let milliseconds = timeout.as_millis().max(1);
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

pub fn redact_url(url: &str) -> String {
    let Some((prefix, query)) = url.split_once('?') else {
        return url.to_owned();
    };
    let redacted = query
        .split('&')
        .map(|part| {
            let Some((key, _)) = part.split_once('=') else {
                return part.to_owned();
            };
            format!("{key}=[redacted]")
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{prefix}?{redacted}")
}

pub fn boxed<'a, T>(
    future: impl std::future::Future<Output = Result<T>> + Send + 'a,
) -> BoxFuture<'a, Result<T>> {
    Box::pin(future)
}
