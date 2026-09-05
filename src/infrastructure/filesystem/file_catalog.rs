use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    application::ports::{DirectoryCompletion, DirectorySuggestion, FileCatalog, FileSystem},
    error::Result,
};

use super::PathResolver;

#[derive(Clone)]
pub struct LocalFileCatalog {
    file_system: Arc<dyn FileSystem>,
    home: PathBuf,
}

impl LocalFileCatalog {
    pub fn new(file_system: Arc<dyn FileSystem>) -> Result<Self> {
        let home = file_system.home_directory()?;
        Self::with_home(file_system, home)
    }

    pub fn with_home(file_system: Arc<dyn FileSystem>, home: PathBuf) -> Result<Self> {
        PathResolver::new(home.clone(), file_system.as_ref())?;
        Ok(Self { file_system, home })
    }
}

impl FileCatalog for LocalFileCatalog {
    fn complete_yaml(&self, working_directory: &str, input: &str) -> Result<DirectoryCompletion> {
        let resolver = PathResolver::new(self.home.clone(), self.file_system.as_ref())?;
        let base = match resolver.resolve_directory(working_directory) {
            Ok(path) => path,
            Err(error) => {
                return Ok(DirectoryCompletion {
                    suggestions: Vec::new(),
                    validation_error: Some(error.to_string()),
                });
            }
        };
        let (parent_input, fragment) = file_completion_parts(input);
        let Some(parent) = relative_parent(&base, parent_input) else {
            return Ok(DirectoryCompletion {
                suggestions: Vec::new(),
                validation_error: Some(
                    "Compose file paths must be relative to the working directory".to_owned(),
                ),
            });
        };
        if !self.file_system.exists(&parent)? || !self.file_system.is_directory(&parent)? {
            return Ok(DirectoryCompletion {
                suggestions: Vec::new(),
                validation_error: Some("the Compose file directory does not exist".to_owned()),
            });
        }

        let fragment = fragment.to_lowercase();
        let mut suggestions = self
            .file_system
            .list_files(&parent)?
            .into_iter()
            .filter(|path| is_yaml_file(path))
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_owned();
                if name.is_empty()
                    || name.chars().any(char::is_control)
                    || !name.to_lowercase().starts_with(&fragment)
                {
                    return None;
                }
                let value = path.strip_prefix(&base).ok()?.to_str()?.replace('\\', "/");
                Some(DirectorySuggestion { name, value })
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.value.cmp(&right.value))
        });

        let normalized_input = normalize_relative_input(input);
        let validation_error = if input.is_empty()
            || suggestions
                .iter()
                .any(|suggestion| suggestion.value == normalized_input)
        {
            None
        } else {
            Some("the selected Compose file does not exist".to_owned())
        };

        Ok(DirectoryCompletion {
            suggestions,
            validation_error,
        })
    }
}

fn file_completion_parts(input: &str) -> (&str, &str) {
    input
        .rfind('/')
        .map(|separator| (&input[..=separator], &input[separator + 1..]))
        .unwrap_or(("", input))
}

fn relative_parent(base: &Path, parent_input: &str) -> Option<PathBuf> {
    if parent_input.is_empty() {
        return Some(base.to_owned());
    }
    if parent_input.starts_with('/')
        || parent_input.starts_with('~')
        || parent_input.starts_with("$HOME")
    {
        return None;
    }
    let relative = parent_input.trim_end_matches('/');
    let path = Path::new(if relative.is_empty() { "." } else { relative });
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(base.join(path))
}

fn normalize_relative_input(input: &str) -> String {
    input.strip_prefix("./").unwrap_or(input).to_owned()
}

fn is_yaml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml"))
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use crate::{
        application::ports::{FileCatalog, FileSystem},
        infrastructure::filesystem::local::LocalFileSystem,
    };

    use super::LocalFileCatalog;

    #[test]
    fn completes_yaml_files_relative_to_the_working_directory() {
        let Ok(home_directory) = tempfile::tempdir() else {
            return;
        };
        let home = home_directory.path().to_owned();
        let project = home.join("project");
        if fs::create_dir(&project).is_err()
            || fs::write(project.join("compose.yaml"), "services: {}").is_err()
            || fs::write(project.join("compose.yml"), "services: {}").is_err()
            || fs::write(project.join("notes.txt"), "notes").is_err()
        {
            return;
        }
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let Ok(catalog) = LocalFileCatalog::with_home(file_system, home) else {
            return;
        };

        let Ok(completion) = catalog.complete_yaml("~/project", "") else {
            return;
        };
        assert_eq!(completion.validation_error, None);
        assert_eq!(
            completion
                .suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["compose.yaml", "compose.yml"]
        );
    }

    #[test]
    fn refreshes_nested_yaml_files_and_rejects_non_yaml_files() {
        let Ok(home_directory) = tempfile::tempdir() else {
            return;
        };
        let home = home_directory.path().to_owned();
        let project = home.join("project");
        let nested = project.join("deploy");
        if fs::create_dir(&project).is_err()
            || fs::create_dir(&nested).is_err()
            || fs::write(nested.join("production.yaml"), "services: {}").is_err()
            || fs::write(nested.join("production.json"), "{}").is_err()
        {
            return;
        }
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let Ok(catalog) = LocalFileCatalog::with_home(file_system, home) else {
            return;
        };

        let Ok(completion) = catalog.complete_yaml("~/project", "deploy/pro") else {
            return;
        };
        assert_eq!(
            completion
                .suggestions
                .first()
                .map(|suggestion| suggestion.value.as_str()),
            Some("deploy/production.yaml")
        );
        assert_eq!(
            completion.validation_error,
            Some("the selected Compose file does not exist".to_owned())
        );
    }
}
