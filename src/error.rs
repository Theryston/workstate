use std::{
    collections::BTreeMap,
    error::Error as StdError,
    fmt::{self, Display, Formatter},
};

use thiserror::Error;

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

    pub const fn exit_code(&self) -> u8 {
        match self.category {
            ErrorCategory::Cli => 2,
            _ => 1,
        }
    }
}
