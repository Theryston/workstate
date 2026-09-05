use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::Value;

use crate::{
    application::{
        planner::CancellationToken,
        ports::{
            DockerComposeObservation, DockerComposeRequest, DockerComposeServiceSnapshot,
            DockerComposeSnapshot, DockerContainerState, DockerHealthState,
        },
    },
    error::{ErrorCategory, Result, WorkstateError},
};

use super::{
    engine::DockerEngineController,
    errors::{compose_configuration_error, compose_failure},
};

#[derive(Clone)]
pub struct DockerComposeController {
    engine: Arc<DockerEngineController>,
    standalone_executable: Option<PathBuf>,
}

impl DockerComposeController {
    pub fn new(
        engine: Arc<DockerEngineController>,
        standalone_executable: Option<PathBuf>,
    ) -> Self {
        Self {
            engine,
            standalone_executable,
        }
    }

    pub async fn observe(
        &self,
        request: DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<DockerComposeObservation> {
        cancellation.check()?;
        let engine = self.engine.inspect(cancellation.clone()).await?;
        if !engine.ready {
            return Ok(DockerComposeObservation::Unavailable(engine));
        }
        let arguments = self.arguments(&request, ["ps", "--all", "--format", "json"])?;
        let output = self
            .run(request.working_directory.clone(), arguments)
            .await?;
        if !output.succeeded() {
            if is_missing_project(&output) {
                return Ok(DockerComposeObservation::Missing);
            }
            return Err(compose_failure("inspect Compose project", &output));
        }
        let parsed = parse_services(&output.stdout)?;
        let services = parsed.services;
        if services.is_empty() {
            return Ok(DockerComposeObservation::Missing);
        }
        let project_name =
            project_name(&request.working_directory, parsed.project_name.as_deref())?;
        Ok(DockerComposeObservation::Present(DockerComposeSnapshot {
            project_name,
            working_directory: request.working_directory,
            services,
        }))
    }

    pub async fn up(
        &self,
        request: &DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<crate::application::ports::ProcessOutput> {
        cancellation.check()?;
        let arguments = if let Some(command) = &request.specification.up_command {
            let process_request = crate::infrastructure::process::command_spec::to_process_request(
                command,
                Some(request.working_directory.clone()),
            )?;
            return self.engine.run_process(process_request).await;
        } else {
            let mut arguments = self.arguments(request, ["up", "--detach"])?;
            arguments.extend(request.specification.services.iter().cloned());
            arguments
        };
        self.run(request.working_directory.clone(), arguments).await
    }

    pub async fn down(
        &self,
        request: &DockerComposeRequest,
        cancellation: CancellationToken,
    ) -> Result<crate::application::ports::ProcessOutput> {
        cancellation.check()?;
        if let Some(command) = &request.specification.down_command {
            reject_destructive_down_command(command)?;
            let process_request = crate::infrastructure::process::command_spec::to_process_request(
                command,
                Some(request.working_directory.clone()),
            )?;
            return self.engine.run_process(process_request).await;
        }
        let arguments = self.arguments(request, ["down"])?;
        self.run(request.working_directory.clone(), arguments).await
    }

    fn arguments<const N: usize>(
        &self,
        request: &DockerComposeRequest,
        operation: [&str; N],
    ) -> Result<Vec<String>> {
        let mut arguments = self.command_prefix();
        if let Some(file) = &request.specification.compose_file {
            let path = resolve_compose_file(&request.working_directory, file)?;
            arguments.extend(["--file".to_owned(), path.display().to_string()]);
        }
        arguments.extend(operation.into_iter().map(str::to_owned));
        Ok(arguments)
    }

    fn command_prefix(&self) -> Vec<String> {
        if self.standalone_executable.is_some() {
            Vec::new()
        } else {
            vec!["compose".to_owned()]
        }
    }

    async fn run(
        &self,
        working_directory: PathBuf,
        arguments: Vec<String>,
    ) -> Result<crate::application::ports::ProcessOutput> {
        if let Some(executable) = &self.standalone_executable {
            return self
                .engine
                .run_process(crate::application::ports::ProcessRequest {
                    program: executable.display().to_string(),
                    arguments,
                    working_directory: Some(working_directory),
                    environment: Vec::new(),
                })
                .await;
        }
        self.engine.run(arguments, Some(working_directory)).await
    }
}

fn project_name(working_directory: &Path, observed: Option<&str>) -> Result<String> {
    if let Some(name) = observed.filter(|value| !value.is_empty()) {
        return Ok(name.to_owned());
    }
    working_directory
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            compose_configuration_error(
                "the Compose working directory does not provide a default project name",
            )
        })
}

fn resolve_compose_file(working_directory: &Path, file: &str) -> Result<PathBuf> {
    if file.is_empty() || file.chars().any(char::is_control) {
        return Err(compose_configuration_error(
            "Compose file paths must be non-empty and contain no control characters",
        ));
    }
    let candidate = if file.starts_with("~/") || file.starts_with("$HOME/") || file == "~" {
        return Err(compose_configuration_error(
            "home-relative Compose file paths must be resolved before building the Docker request",
        ));
    } else {
        let path = Path::new(file);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            working_directory.join(path)
        }
    };
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(compose_configuration_error(
            "Compose file paths must not contain parent-directory traversal",
        ));
    }
    Ok(candidate)
}

fn is_missing_project(output: &crate::application::ports::ProcessOutput) -> bool {
    let diagnostic = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    diagnostic.contains("no such service")
        || diagnostic.contains("no container")
        || diagnostic.contains("not found")
        || diagnostic.contains("does not exist")
}

struct ParsedComposeServices {
    services: Vec<DockerComposeServiceSnapshot>,
    project_name: Option<String>,
}

fn parse_services(bytes: &[u8]) -> Result<ParsedComposeServices> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(ParsedComposeServices {
            services: Vec::new(),
            project_name: None,
        });
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return parse_service_value(&value);
    }
    let mut services = Vec::new();
    let mut project_name = None;
    for line in trimmed.lines().filter(|line| !line.trim().is_empty()) {
        let value = serde_json::from_str::<Value>(line).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Integration,
                "Docker Compose returned malformed service data",
                source,
            )
        })?;
        let parsed = parse_service_value(&value)?;
        if project_name.is_none() {
            project_name = parsed.project_name;
        }
        services.extend(parsed.services);
    }
    Ok(ParsedComposeServices {
        services,
        project_name,
    })
}

fn parse_service_value(value: &Value) -> Result<ParsedComposeServices> {
    let values = match value {
        Value::Array(items) => items.iter().collect::<Vec<_>>(),
        Value::Object(_) => vec![value],
        _ => {
            return Err(compose_configuration_error(
                "Docker Compose returned an unexpected JSON value",
            ));
        }
    };
    let project_name = values
        .iter()
        .find_map(|value| json_string_optional(value, &["Project", "project"]));
    let services = values
        .into_iter()
        .map(parse_service)
        .collect::<Result<Vec<_>>>()?;
    Ok(ParsedComposeServices {
        services,
        project_name,
    })
}

fn parse_service(value: &Value) -> Result<DockerComposeServiceSnapshot> {
    let name = json_string(value, &["Service", "service", "Name", "name"])?;
    let container_id = json_string_optional(value, &["ID", "id", "ContainerID", "container_id"]);
    let state = parse_container_state(json_string_optional(value, &["State", "state"]));
    let health = parse_health(json_string_optional(value, &["Health", "health"]));
    Ok(DockerComposeServiceSnapshot {
        name,
        container_id,
        state,
        health,
    })
}

fn json_string(value: &Value, keys: &[&str]) -> Result<String> {
    json_string_optional(value, keys)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            compose_configuration_error("Docker Compose service data omitted its service name")
        })
}

fn json_string_optional(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.chars().any(char::is_control))
            .map(str::to_owned)
    })
}

fn parse_container_state(value: Option<String>) -> DockerContainerState {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "created" => DockerContainerState::Created,
        "running" => DockerContainerState::Running,
        "paused" => DockerContainerState::Paused,
        "restarting" => DockerContainerState::Restarting,
        "exited" => DockerContainerState::Exited,
        "dead" => DockerContainerState::Dead,
        value => DockerContainerState::Unknown(value.to_owned()),
    }
}

fn parse_health(value: Option<String>) -> DockerHealthState {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "healthy" => DockerHealthState::Healthy,
        "unhealthy" => DockerHealthState::Unhealthy,
        "starting" => DockerHealthState::Starting,
        "" => DockerHealthState::None,
        value => DockerHealthState::Unknown(value.to_owned()),
    }
}

fn reject_destructive_down_command(command: &crate::domain::CommandSpec) -> Result<()> {
    let forbidden = ["--volumes", "-v", "--rmi", "--remove-orphans"];
    if command
        .arguments
        .iter()
        .any(|argument| forbidden.contains(&argument.as_str()))
        || forbidden.iter().any(|flag| command.program.contains(flag))
    {
        return Err(WorkstateError::new(
            ErrorCategory::Integration,
            "destructive Docker Compose cleanup flags are not allowed",
        ));
    }
    Ok(())
}
