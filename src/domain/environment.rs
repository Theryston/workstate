use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeserializeError};

use super::{ActionGraph, ActionSpec, DomainError, WorkspaceSpec};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EnvironmentName(String);

impl EnvironmentName {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();

        if value.is_empty() || value.chars().all(char::is_whitespace) {
            return Err(DomainError::EmptyEnvironmentName);
        }

        if value == "." || value == ".." {
            return Err(DomainError::ReservedEnvironmentName);
        }

        if value.chars().any(|character| {
            character == '/' || character == '\\' || character == '\0' || character.is_control()
        }) {
            return Err(DomainError::InvalidEnvironmentName);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn derive_slug(&self) -> Result<EnvironmentSlug, DomainError> {
        let mut slug = String::new();
        let mut separator_pending = false;

        for character in self.0.chars() {
            if character.is_ascii_alphanumeric() {
                if separator_pending && !slug.is_empty() {
                    slug.push('-');
                }
                separator_pending = false;
                slug.push(character.to_ascii_lowercase());
            } else if character == '-' || character == '_' || character.is_whitespace() {
                separator_pending = true;
            }
        }

        if slug.is_empty() {
            return Err(DomainError::EmptyEnvironmentSlug);
        }

        EnvironmentSlug::new(slug)
    }
}

impl<'de> Deserialize<'de> for EnvironmentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl Display for EnvironmentName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EnvironmentSlug(String);

impl EnvironmentSlug {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();

        if value.is_empty()
            || !value.chars().next().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            })
            || !value.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
            || value.ends_with('-')
            || value.contains("--")
        {
            return Err(DomainError::InvalidEnvironmentSlug { value });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EnvironmentSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl Display for EnvironmentSlug {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub name: EnvironmentName,
    pub slug: EnvironmentSlug,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSpec>,
    #[serde(default)]
    pub actions: Vec<ActionSpec>,
}

impl EnvironmentConfig {
    pub fn new(name: impl Into<String>) -> Result<Self, DomainError> {
        let name = EnvironmentName::new(name)?;
        let slug = name.derive_slug()?;

        Ok(Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            name,
            slug,
            workspaces: Vec::new(),
            actions: Vec::new(),
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(DomainError::UnsupportedEnvironmentSchema {
                actual: self.schema_version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }

        let mut workspace_ids = std::collections::BTreeSet::new();
        for workspace in &self.workspaces {
            workspace.validate()?;
            if !workspace_ids.insert(workspace.id.clone()) {
                return Err(DomainError::DuplicateWorkspaceId {
                    id: workspace.id.to_string(),
                });
            }
        }

        ActionGraph::validate(&self.actions, &workspace_ids)?;
        Ok(())
    }

    pub fn rename(&mut self, name: impl Into<String>) -> Result<(), DomainError> {
        self.name = EnvironmentName::new(name)?;
        Ok(())
    }

    pub fn add_workspace(&mut self, workspace: WorkspaceSpec) -> Result<(), DomainError> {
        if self.workspaces.iter().any(|item| item.id == workspace.id) {
            return Err(DomainError::DuplicateWorkspaceId {
                id: workspace.id.to_string(),
            });
        }
        workspace.validate()?;
        self.workspaces.push(workspace);
        Ok(())
    }

    pub fn add_action(&mut self, action: ActionSpec) -> Result<(), DomainError> {
        if self.actions.iter().any(|item| item.id == action.id) {
            return Err(DomainError::DuplicateActionId {
                id: action.id.to_string(),
            });
        }
        self.actions.push(action);
        Ok(())
    }
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::{CURRENT_SCHEMA_VERSION, EnvironmentConfig, EnvironmentName, EnvironmentSlug};

    #[test]
    fn derives_a_stable_lowercase_slug_without_rewriting_the_display_name() {
        let name = EnvironmentName::new("Personal Blog").ok();
        assert!(name.is_some());
        let Some(name) = name else {
            return;
        };

        assert_eq!(name.as_str(), "Personal Blog");
        assert_eq!(
            name.derive_slug().ok().map(|slug| slug.to_string()),
            Some("personal-blog".to_owned())
        );
    }

    #[test]
    fn rejects_empty_and_path_like_environment_names() {
        assert!(EnvironmentName::new("").is_err());
        assert!(EnvironmentName::new("   ").is_err());
        assert!(EnvironmentName::new("../blog").is_err());
        assert!(EnvironmentName::new("..").is_err());
    }

    #[test]
    fn rejects_invalid_slugs() {
        assert!(EnvironmentSlug::new("").is_err());
        assert!(EnvironmentSlug::new("Personal-Blog").is_err());
        assert!(EnvironmentSlug::new("../blog").is_err());
        assert!(EnvironmentSlug::new("personal--blog").is_err());
    }

    #[test]
    fn new_configuration_has_the_current_schema_and_empty_collections() {
        let configuration = EnvironmentConfig::new("Blog").ok();
        assert!(configuration.is_some());
        let Some(configuration) = configuration else {
            return;
        };

        assert_eq!(configuration.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(configuration.workspaces.is_empty());
        assert!(configuration.actions.is_empty());
        assert!(configuration.validate().is_ok());
    }

    #[test]
    fn renaming_keeps_the_persisted_slug_stable() {
        let configuration = EnvironmentConfig::new("Personal Blog").ok();
        assert!(configuration.is_some());
        let Some(mut configuration) = configuration else {
            return;
        };

        assert!(configuration.rename("My Blog").is_ok());
        assert_eq!(configuration.name.as_str(), "My Blog");
        assert_eq!(configuration.slug.as_str(), "personal-blog");
    }
}
