use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
};

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeserializeError};

use super::{
    DomainError, EnvironmentSlug, TilingPreference, WorkspaceId, WorkspaceTarget,
    validate_identifier,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ActionId(String);

impl ActionId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_identifier(&value, "action")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ActionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl Display for ActionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    RunOnce,
    Background,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    #[default]
    OwnedOnly,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeout {
    pub milliseconds: u64,
}

impl Timeout {
    pub fn new(milliseconds: u64) -> Result<Self, DomainError> {
        if milliseconds == 0 {
            return Err(DomainError::InvalidActionTimeout {
                action_id: "unspecified".to_owned(),
                message: "milliseconds must be greater than zero".to_owned(),
            });
        }

        Ok(Self { milliseconds })
    }

    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self.milliseconds == 0 {
            return Err(DomainError::InvalidActionTimeout {
                action_id: action_id.to_string(),
                message: "milliseconds must be greater than zero".to_owned(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub delay_milliseconds: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            delay_milliseconds: 0,
        }
    }
}

impl RetryPolicy {
    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self.max_attempts == 0 {
            return Err(DomainError::InvalidRetryPolicy {
                action_id: action_id.to_string(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            shell: false,
            environment: BTreeMap::new(),
        }
    }

    pub fn from_argv_line(action_id: &ActionId, line: &str) -> Result<Self, DomainError> {
        let tokens = tokenize_argv_line(line).map_err(|message| DomainError::InvalidCommand {
            action_id: action_id.to_string(),
            message,
        })?;
        let Some((program, arguments)) = tokens.split_first() else {
            return Err(DomainError::InvalidCommand {
                action_id: action_id.to_string(),
                message: "the command must contain an executable".to_owned(),
            });
        };
        let mut command = Self::new(program.clone());
        command.arguments = arguments.to_vec();
        Ok(command)
    }

    pub fn display_line(&self) -> String {
        if self.shell {
            return self.program.clone();
        }
        let mut tokens = Vec::with_capacity(self.arguments.len() + 1);
        tokens.push(quote_argv_token(&self.program));
        tokens.extend(
            self.arguments
                .iter()
                .map(|argument| quote_argv_token(argument)),
        );
        tokens.join(" ")
    }

    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self.program.is_empty() || self.program.chars().any(char::is_control) {
            return Err(DomainError::InvalidCommand {
                action_id: action_id.to_string(),
                message: "the program or shell command must be non-empty and contain no control characters".to_owned(),
            });
        }

        if self.shell && !self.arguments.is_empty() {
            return Err(DomainError::InvalidCommand {
                action_id: action_id.to_string(),
                message: "shell commands must not define argv arguments".to_owned(),
            });
        }

        if self
            .arguments
            .iter()
            .any(|argument| argument.contains('\0') || argument.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidCommand {
                action_id: action_id.to_string(),
                message: "arguments must not contain control characters".to_owned(),
            });
        }

        if self.environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains('=')
                || key.chars().any(char::is_control)
                || value.chars().any(char::is_control)
                || value.contains('\0')
        }) {
            return Err(DomainError::InvalidCommand {
                action_id: action_id.to_string(),
                message:
                    "environment entries must have valid names and must not contain NUL characters"
                        .to_owned(),
            });
        }

        Ok(())
    }
}

fn tokenize_argv_line(line: &str) -> std::result::Result<Vec<String>, String> {
    if line.chars().any(char::is_control) {
        return Err("the command must not contain control characters".to_owned());
    }

    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut token_started = false;

    for character in line.chars() {
        if escaped {
            token.push(character);
            token_started = true;
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    token.push(character);
                }
            }
            Some('"') => match character {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => token.push(character),
            },
            Some(_) => token.push(character),
            None => match character {
                '\\' => {
                    escaped = true;
                    token_started = true;
                }
                '\'' | '"' => {
                    quote = Some(character);
                    token_started = true;
                }
                character if character.is_whitespace() => {
                    if token_started {
                        tokens.push(std::mem::take(&mut token));
                        token_started = false;
                    }
                }
                _ => {
                    token.push(character);
                    token_started = true;
                }
            },
        }
    }

    if escaped {
        return Err("the command cannot end with an escape character".to_owned());
    }
    if quote.is_some() {
        return Err("the command contains an unterminated quote".to_owned());
    }
    if token_started {
        tokens.push(token);
    }
    if tokens.is_empty() {
        return Err("the command must contain an executable".to_owned());
    }
    Ok(tokens)
}

fn quote_argv_token(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| !character.is_whitespace() && !matches!(character, '\'' | '"' | '\\'))
    {
        return value.to_owned();
    }

    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub command: Option<CommandSpec>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub mounts: Vec<ContainerMount>,
    #[serde(default)]
    pub ports: Vec<ContainerPort>,
}

impl ContainerSpec {
    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().any(char::is_control) {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "container.name".to_owned(),
            });
        }

        if let Some(image) = &self.image
            && (image.is_empty() || image.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "container.image".to_owned(),
            });
        }

        if let Some(command) = &self.command {
            command.validate_for(action_id)?;
        }

        if self.environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains('=')
                || key.chars().any(char::is_control)
                || value.chars().any(char::is_control)
                || value.contains('\0')
        }) {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "container.environment".to_owned(),
            });
        }

        if self.mounts.iter().any(|mount| {
            mount.source.is_empty()
                || mount.target.is_empty()
                || mount.source.chars().any(char::is_control)
                || mount.target.chars().any(char::is_control)
        }) {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "container.mounts".to_owned(),
            });
        }

        if self.ports.iter().any(|port| {
            port.host == 0
                || port.container == 0
                || port.protocol.is_empty()
                || port.protocol.chars().any(char::is_control)
                || !matches!(port.protocol.as_str(), "tcp" | "udp" | "sctp")
        }) {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "container.ports".to_owned(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerMount {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPort {
    pub host: u16,
    pub container: u16,
    #[serde(default = "default_container_port_protocol")]
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeSpec {
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub up_command: Option<CommandSpec>,
    #[serde(default)]
    pub down_command: Option<CommandSpec>,
}

impl ComposeSpec {
    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self
            .project_name
            .as_ref()
            .is_some_and(|name| name.is_empty() || name.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "compose.project_name".to_owned(),
            });
        }

        if self
            .files
            .iter()
            .any(|file| file.is_empty() || file.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "compose.files".to_owned(),
            });
        }

        if self
            .services
            .iter()
            .any(|service| service.is_empty() || service.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "compose.services".to_owned(),
            });
        }

        if let Some(command) = &self.up_command {
            command.validate_for(action_id)?;
        }
        if let Some(command) = &self.down_command {
            command.validate_for(action_id)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatorSpec {
    pub avd: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

impl EmulatorSpec {
    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        if self.avd.is_empty() || self.avd.chars().any(char::is_control) {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "emulator.avd".to_owned(),
            });
        }

        if self
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "emulator.arguments".to_owned(),
            });
        }

        Ok(())
    }
}

pub type CustomParameters = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ActionParameters {
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default)]
    pub command: Option<CommandSpec>,
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub container: Option<ContainerSpec>,
    #[serde(default)]
    pub compose: Option<ComposeSpec>,
    #[serde(default)]
    pub emulator: Option<EmulatorSpec>,
    #[serde(default)]
    pub custom: CustomParameters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    OpenApplication,
    OpenProject,
    #[serde(alias = "start_service")]
    RunCommand,
    ConfigureTiling,
    StartContainer,
    StartCompose,
    StartAndroidEmulator,
    WaitForCondition,
    VerifyResource,
    Custom {
        name: String,
    },
}

impl ActionKind {
    pub fn key(&self) -> String {
        match self {
            Self::OpenApplication => "open_application".to_owned(),
            Self::OpenProject => "open_project".to_owned(),
            Self::RunCommand => "run_command".to_owned(),
            Self::ConfigureTiling => "configure_tiling".to_owned(),
            Self::StartContainer => "start_container".to_owned(),
            Self::StartCompose => "start_compose".to_owned(),
            Self::StartAndroidEmulator => "start_android_emulator".to_owned(),
            Self::WaitForCondition => "wait_for_condition".to_owned(),
            Self::VerifyResource => "verify_resource".to_owned(),
            Self::Custom { name } => format!("custom:{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadinessCheck {
    None,
    Tcp {
        host: String,
        port: u16,
        timeout: Timeout,
    },
    Http {
        url: String,
        #[serde(default)]
        expected_status: Option<u16>,
        timeout: Timeout,
    },
    Command {
        command: CommandSpec,
        timeout: Timeout,
    },
    Delay {
        milliseconds: u64,
    },
    Container {
        name: String,
        timeout: Timeout,
    },
    Compose {
        #[serde(default)]
        services: Vec<String>,
        timeout: Timeout,
    },
}

impl ReadinessCheck {
    fn validate_for(&self, action_id: &ActionId) -> Result<(), DomainError> {
        match self {
            Self::None => Ok(()),
            Self::Tcp {
                host,
                port,
                timeout,
            } => {
                if host.is_empty() || host.chars().any(char::is_control) || *port == 0 {
                    return Err(DomainError::InvalidReadinessCheck {
                        action_id: action_id.to_string(),
                        message: "TCP host must be non-empty and port must be greater than zero"
                            .to_owned(),
                    });
                }
                timeout.validate_for(action_id)
            }
            Self::Http {
                url,
                expected_status,
                timeout,
            } => {
                if !(url.starts_with("http://") || url.starts_with("https://"))
                    || url.chars().any(char::is_control)
                    || expected_status.is_some_and(|status| status == 0)
                {
                    return Err(DomainError::InvalidReadinessCheck {
                        action_id: action_id.to_string(),
                        message:
                            "HTTP URL must use http or https and expected status must be valid"
                                .to_owned(),
                    });
                }
                timeout.validate_for(action_id)
            }
            Self::Command { command, timeout } => {
                command.validate_for(action_id)?;
                timeout.validate_for(action_id)
            }
            Self::Delay { milliseconds } if *milliseconds == 0 => {
                Err(DomainError::InvalidReadinessCheck {
                    action_id: action_id.to_string(),
                    message: "delay must be greater than zero milliseconds".to_owned(),
                })
            }
            Self::Delay { .. } => Ok(()),
            Self::Container { name, timeout } => {
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err(DomainError::InvalidReadinessCheck {
                        action_id: action_id.to_string(),
                        message:
                            "container name must be non-empty and contain no control characters"
                                .to_owned(),
                    });
                }
                timeout.validate_for(action_id)
            }
            Self::Compose { services, timeout } => {
                if services
                    .iter()
                    .any(|service| service.is_empty() || service.chars().any(char::is_control))
                {
                    return Err(DomainError::InvalidReadinessCheck {
                        action_id: action_id.to_string(),
                        message: "Compose service names must be non-empty and contain no control characters".to_owned(),
                    });
                }
                timeout.validate_for(action_id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSpec {
    pub id: ActionId,
    pub kind: ActionKind,
    #[serde(default)]
    pub display_label: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<ActionId>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub desktop_workspace: Option<WorkspaceId>,
    #[serde(default)]
    pub execution_mode: Option<ExecutionMode>,
    #[serde(default)]
    pub parameters: ActionParameters,
    #[serde(default)]
    pub readiness_checks: Vec<ReadinessCheck>,
    #[serde(default)]
    pub timeout: Option<Timeout>,
    #[serde(default)]
    pub retry_policy: RetryPolicy,
    #[serde(default)]
    pub cleanup_policy: CleanupPolicy,
    #[serde(skip)]
    pub resolved_workspace_target: Option<WorkspaceTarget>,
    #[serde(skip)]
    pub resolved_tiling: Option<TilingPreference>,
    #[serde(skip)]
    pub resolved_environment: Option<EnvironmentSlug>,
}

impl ActionSpec {
    pub fn new(id: impl Into<String>, kind: ActionKind) -> Result<Self, DomainError> {
        Ok(Self {
            id: ActionId::new(id)?,
            kind,
            display_label: None,
            depends_on: Vec::new(),
            working_directory: None,
            desktop_workspace: None,
            execution_mode: None,
            parameters: ActionParameters::default(),
            readiness_checks: Vec::new(),
            timeout: None,
            retry_policy: RetryPolicy::default(),
            cleanup_policy: CleanupPolicy::default(),
            resolved_workspace_target: None,
            resolved_tiling: None,
            resolved_environment: None,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let action_id = &self.id;

        if self
            .working_directory
            .as_ref()
            .is_some_and(|path| path.is_empty() || path.contains('\0'))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "working_directory".to_owned(),
            });
        }

        if self
            .display_label
            .as_ref()
            .is_some_and(|label| label.is_empty() || label.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidActionParameter {
                action_id: action_id.to_string(),
                parameter: "display_label".to_owned(),
            });
        }

        if let Some(timeout) = &self.timeout {
            timeout.validate_for(action_id)?;
        }
        self.retry_policy.validate_for(action_id)?;

        for check in &self.readiness_checks {
            check.validate_for(action_id)?;
        }

        match &self.kind {
            ActionKind::OpenApplication => {
                require_text(&self.parameters.application, action_id, "application")?;
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::OpenProject => {
                require_text(&self.parameters.application, action_id, "application")?;
                require_text(&self.parameters.project_path, action_id, "project_path")?;
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::RunCommand => {
                let command = self.parameters.command.as_ref().ok_or_else(|| {
                    DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "command".to_owned(),
                    }
                })?;
                command.validate_for(action_id)?;
                if self.execution_mode.is_none() {
                    return Err(DomainError::InvalidExecutionMode {
                        action_id: action_id.to_string(),
                        message: "command actions must declare run_once or background".to_owned(),
                    });
                }
            }
            ActionKind::ConfigureTiling => {
                if self.desktop_workspace.is_none() {
                    return Err(DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "desktop_workspace".to_owned(),
                    });
                }
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::StartContainer => {
                let container = self.parameters.container.as_ref().ok_or_else(|| {
                    DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "container".to_owned(),
                    }
                })?;
                container.validate_for(action_id)?;
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::StartCompose => {
                let compose = self.parameters.compose.as_ref().ok_or_else(|| {
                    DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "compose".to_owned(),
                    }
                })?;
                compose.validate_for(action_id)?;
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::StartAndroidEmulator => {
                let emulator = self.parameters.emulator.as_ref().ok_or_else(|| {
                    DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "emulator".to_owned(),
                    }
                })?;
                emulator.validate_for(action_id)?;
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::WaitForCondition | ActionKind::VerifyResource => {
                if self.readiness_checks.is_empty() {
                    return Err(DomainError::MissingActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "readiness_checks".to_owned(),
                    });
                }
                reject_execution_mode(self, action_id)?;
            }
            ActionKind::Custom { name } => {
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err(DomainError::InvalidActionParameter {
                        action_id: action_id.to_string(),
                        parameter: "custom.name".to_owned(),
                    });
                }
                if let Some(mode) = self.execution_mode
                    && mode == ExecutionMode::Background
                    && self.parameters.command.is_none()
                {
                    return Err(DomainError::InvalidExecutionMode {
                        action_id: action_id.to_string(),
                        message: "a background custom action must provide a command".to_owned(),
                    });
                }
                if let Some(command) = &self.parameters.command {
                    command.validate_for(action_id)?;
                }
            }
        }

        Ok(())
    }
}

fn require_text(
    value: &Option<String>,
    action_id: &ActionId,
    parameter: &str,
) -> Result<(), DomainError> {
    if value
        .as_ref()
        .is_none_or(|text| text.is_empty() || text.chars().any(char::is_control))
    {
        return Err(DomainError::MissingActionParameter {
            action_id: action_id.to_string(),
            parameter: parameter.to_owned(),
        });
    }

    Ok(())
}

fn reject_execution_mode(action: &ActionSpec, action_id: &ActionId) -> Result<(), DomainError> {
    if action.execution_mode.is_some() {
        return Err(DomainError::InvalidExecutionMode {
            action_id: action_id.to_string(),
            message: "only command-like actions may declare an execution mode".to_owned(),
        });
    }

    Ok(())
}

fn default_max_attempts() -> u32 {
    1
}

fn default_container_port_protocol() -> String {
    "tcp".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        ActionId, ActionKind, ActionParameters, ActionSpec, CommandSpec, ExecutionMode,
        ReadinessCheck, Timeout,
    };

    #[test]
    fn command_actions_require_an_execution_mode_and_command() {
        let action = ActionSpec::new("api", ActionKind::RunCommand).ok();
        assert!(action.is_some());
        let Some(mut action) = action else {
            return;
        };
        assert!(action.validate().is_err());

        action.parameters = ActionParameters {
            command: Some(CommandSpec::new("bun")),
            ..ActionParameters::default()
        };
        action.execution_mode = Some(ExecutionMode::Background);
        assert!(action.validate().is_ok());
    }

    #[test]
    fn readiness_checks_validate_their_required_values() {
        let action = ActionSpec::new("health", ActionKind::VerifyResource).ok();
        assert!(action.is_some());
        let Some(mut action) = action else {
            return;
        };
        action.readiness_checks.push(ReadinessCheck::Tcp {
            host: "127.0.0.1".to_owned(),
            port: 8080,
            timeout: Timeout {
                milliseconds: 1_000,
            },
        });
        assert!(action.validate().is_ok());

        action
            .readiness_checks
            .push(ReadinessCheck::Delay { milliseconds: 0 });
        assert!(action.validate().is_err());
    }

    #[test]
    fn background_mode_is_rejected_for_non_command_actions() {
        let action = ActionSpec::new("zed", ActionKind::OpenApplication).ok();
        assert!(action.is_some());
        let Some(mut action) = action else {
            return;
        };
        action.execution_mode = Some(ExecutionMode::Background);
        assert!(action.validate().is_err());
    }

    #[test]
    fn removed_start_service_kind_loads_as_run_command() {
        let action = toml::from_str::<ActionSpec>("id = \"api\"\nkind = \"start_service\"\n");
        assert!(action.is_ok());
        let Some(action) = action.ok() else {
            return;
        };
        assert_eq!(action.kind, ActionKind::RunCommand);
        let serialized = toml::to_string(&action);
        assert!(serialized.is_ok());
        let Some(serialized) = serialized.ok() else {
            return;
        };
        assert!(serialized.contains("kind = \"run_command\""));
        assert!(!serialized.contains("start_service"));
    }

    #[test]
    fn command_lines_are_split_without_enabling_shell_execution() {
        let Some(action_id) = ActionId::new("run-command").ok() else {
            return;
        };
        let command = CommandSpec::from_argv_line(&action_id, "bun run \"dev server\"");
        assert!(command.is_ok());
        let Some(command) = command.ok() else {
            return;
        };
        assert_eq!(command.program, "bun");
        assert_eq!(command.arguments, vec!["run", "dev server"]);
        assert!(!command.shell);
        assert_eq!(command.display_line(), "bun run 'dev server'");
    }

    #[test]
    fn command_lines_reject_unterminated_quotes() {
        let Some(action_id) = ActionId::new("run-command").ok() else {
            return;
        };
        assert!(CommandSpec::from_argv_line(&action_id, "bun \"dev").is_err());
    }
}
