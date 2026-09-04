use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeserializeError};

use super::{DomainError, validate_identifier};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_identifier(&value, "workspace")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl Display for WorkspaceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTarget {
    Current,
    Existing { reference: WorkspaceReference },
    NextEmpty,
    Create { name: String },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceReference {
    Name(String),
    Identifier(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TilingPreference {
    #[default]
    Unchanged,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    pub id: WorkspaceId,
    pub target: WorkspaceTarget,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tiling: TilingPreference,
}

impl WorkspaceSpec {
    pub fn new(id: impl Into<String>, target: WorkspaceTarget) -> Result<Self, DomainError> {
        Ok(Self {
            id: WorkspaceId::new(id)?,
            target,
            name: None,
            tiling: TilingPreference::default(),
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(name) = &self.name
            && (name.is_empty() || name.chars().any(char::is_control))
        {
            return Err(DomainError::InvalidWorkspaceTarget {
                message: format!("workspace {} has an invalid display name", self.id),
            });
        }

        match &self.target {
            WorkspaceTarget::Existing { reference } => match reference {
                WorkspaceReference::Name(name) | WorkspaceReference::Identifier(name)
                    if name.is_empty() || name.chars().any(char::is_control) =>
                {
                    Err(DomainError::InvalidWorkspaceTarget {
                        message: format!("workspace {} has an invalid existing reference", self.id),
                    })
                }
                WorkspaceReference::Name(_) | WorkspaceReference::Identifier(_) => Ok(()),
            },
            WorkspaceTarget::Create { name }
                if name.is_empty() || name.chars().any(char::is_control) =>
            {
                Err(DomainError::InvalidWorkspaceTarget {
                    message: format!("workspace {} has an invalid create name", self.id),
                })
            }
            WorkspaceTarget::Current
            | WorkspaceTarget::NextEmpty
            | WorkspaceTarget::Create { .. }
            | WorkspaceTarget::None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TilingPreference, WorkspaceReference, WorkspaceSpec, WorkspaceTarget};

    #[test]
    fn validates_workspace_target_variants() {
        let existing = WorkspaceSpec::new(
            "editor",
            WorkspaceTarget::Existing {
                reference: WorkspaceReference::Name("Development".to_owned()),
            },
        );
        assert!(existing.is_ok());

        let create = WorkspaceSpec::new(
            "services",
            WorkspaceTarget::Create {
                name: "Services".to_owned(),
            },
        );
        assert!(create.is_ok());
        let Some(mut create) = create.ok() else {
            return;
        };
        create.tiling = TilingPreference::Enabled;
        assert!(create.validate().is_ok());
    }

    #[test]
    fn rejects_empty_existing_and_created_workspace_names() {
        let existing = WorkspaceSpec::new(
            "editor",
            WorkspaceTarget::Existing {
                reference: WorkspaceReference::Name(String::new()),
            },
        );
        assert!(existing.is_ok());
        let Some(existing) = existing.ok() else {
            return;
        };
        assert!(existing.validate().is_err());

        let create = WorkspaceSpec::new(
            "services",
            WorkspaceTarget::Create {
                name: String::new(),
            },
        );
        assert!(create.is_ok());
        let Some(create) = create.ok() else {
            return;
        };
        assert!(create.validate().is_err());
    }
}
