use std::{
    collections::BTreeMap,
    error::Error as StdError,
    fmt::{self, Display, Formatter},
};

use thiserror::Error;

use crate::domain::DomainError;

pub type Result<T> = std::result::Result<T, WorkstateError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Domain,
    Persistence,
    Platform,
    Process,
    Integration,
    Ui,
    Cli,
    Runtime,
}

impl Display for ErrorCategory {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Domain => "domain error",
            Self::Persistence => "persistence error",
            Self::Platform => "platform error",
            Self::Process => "process error",
            Self::Integration => "integration error",
            Self::Ui => "UI error",
            Self::Cli => "CLI error",
            Self::Runtime => "runtime error",
        };

        formatter.write_str(label)
    }
}

#[derive(Debug, Error)]
#[error("{category}: {message}")]
pub struct WorkstateError {
    pub category: ErrorCategory,
    pub message: String,
    pub context: BTreeMap<String, String>,
    #[source]
    pub source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl WorkstateError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            context: BTreeMap::new(),
            source: None,
        }
    }

    pub fn with_source<E>(category: ErrorCategory, message: impl Into<String>, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self {
            category,
            message: message.into(),
            context: BTreeMap::new(),
            source: Some(Box::new(source)),
        }
    }

    pub fn with_boxed_source(
        category: ErrorCategory,
        message: impl Into<String>,
        source: Box<dyn StdError + Send + Sync + 'static>,
    ) -> Self {
        Self {
            category,
            message: message.into(),
            context: BTreeMap::new(),
            source: Some(source),
        }
    }

    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    pub fn render(&self) -> String {
        if let Some(rendered) = self.render_platform_diagnostics() {
            return rendered;
        }
        if self.context.is_empty() {
            return self.to_string();
        }

        let mut rendered = self.to_string();
        if let Some(command) = self.context.get("suggested_command") {
            rendered.push_str("\n\nCreate it with:\n  ");
            rendered.push_str(command);
        }
        let preferred_keys = [
            "operating_system",
            "distribution",
            "desktop_environment",
            "terminal_capability",
            "supported_profiles",
            "missing_capabilities",
        ];
        for key in preferred_keys
            .into_iter()
            .filter(|key| self.context.contains_key(*key))
            .chain(
                self.context
                    .keys()
                    .map(String::as_str)
                    .filter(|key| *key != "suggested_command" && !preferred_keys.contains(key)),
            )
        {
            let Some(value) = self.context.get(key) else {
                continue;
            };
            rendered.push('\n');
            rendered.push_str("  ");
            rendered.push_str(&humanize_context_key(key));
            rendered.push_str(": ");
            rendered.push_str(value);
        }
        rendered
    }

    fn render_platform_diagnostics(&self) -> Option<String> {
        if self.category != ErrorCategory::Platform {
            return None;
        }

        let operating_system = self.context.get("operating_system")?;
        let distribution = self.context.get("distribution")?;
        let desktop_environment = self.context.get("desktop_environment")?;
        let terminal_capability = self.context.get("terminal_capability")?;
        let supported_profiles = self.context.get("supported_profiles")?;

        let mut rendered = self.message.clone();
        rendered.push_str("\n\nDetected environment:\n");
        rendered.push_str("  Operating system: ");
        rendered.push_str(operating_system);
        rendered.push('\n');
        rendered.push_str("  Distribution: ");
        rendered.push_str(distribution);
        rendered.push('\n');
        rendered.push_str("  Desktop environment: ");
        rendered.push_str(desktop_environment);
        rendered.push('\n');
        rendered.push_str("  Terminal capability: ");
        rendered.push_str(terminal_capability);
        rendered.push_str("\n\nCurrently supported:\n");
        for profile in supported_profiles.split("; ") {
            rendered.push_str("  ");
            rendered.push_str(profile);
            rendered.push('\n');
        }
        if let Some(missing) = self.context.get("missing_capabilities") {
            rendered.push_str("\nMissing capabilities:\n  ");
            rendered.push_str(missing);
            rendered.push('\n');
        }

        Some(rendered)
    }

    pub const fn exit_code(&self) -> u8 {
        match self.category {
            ErrorCategory::Cli => 2,
            _ => 1,
        }
    }
}

fn humanize_context_key(key: &str) -> String {
    let mut result = String::with_capacity(key.len());
    let mut uppercase_first = true;
    for character in key.chars() {
        if character == '_' {
            result.push(' ');
        } else if uppercase_first {
            result.extend(character.to_uppercase());
            uppercase_first = false;
        } else {
            result.push(character);
        }
    }
    result
}

impl From<DomainError> for WorkstateError {
    fn from(source: DomainError) -> Self {
        Self::with_source(ErrorCategory::Domain, source.to_string(), source)
    }
}
