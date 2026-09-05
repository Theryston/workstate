use std::{path::PathBuf, sync::Arc};

use crate::{
    application::ports::{DirectoryCatalog, DirectoryCompletion, DirectorySuggestion, FileSystem},
    error::Result,
};

use super::PathResolver;

#[derive(Clone)]
pub struct LocalDirectoryCatalog {
    file_system: Arc<dyn FileSystem>,
    home: PathBuf,
}

impl LocalDirectoryCatalog {
    pub fn new(file_system: Arc<dyn FileSystem>) -> Result<Self> {
        let home = file_system.home_directory()?;
        Self::with_home(file_system, home)
    }

    pub fn with_home(file_system: Arc<dyn FileSystem>, home: PathBuf) -> Result<Self> {
        PathResolver::new(home.clone(), file_system.as_ref())?;
        Ok(Self { file_system, home })
    }
}

impl DirectoryCatalog for LocalDirectoryCatalog {
    fn complete(&self, input: &str) -> Result<DirectoryCompletion> {
        let resolver = PathResolver::new(self.home.clone(), self.file_system.as_ref())?;
        let validation_error = resolver
            .resolve_directory(input)
            .err()
            .map(|error| error.to_string());
        let Some((parent_input, fragment)) = completion_parts(input) else {
            return Ok(DirectoryCompletion {
                suggestions: Vec::new(),
                validation_error,
            });
        };

        let parent = match resolver.expand(parent_input) {
            Ok(parent) => parent,
            Err(error) => {
                return Ok(DirectoryCompletion {
                    suggestions: Vec::new(),
                    validation_error: Some(error.to_string()),
                });
            }
        };
        if !self.file_system.exists(&parent)? {
            return Ok(DirectoryCompletion {
                suggestions: Vec::new(),
                validation_error,
            });
        }
        if !self.file_system.is_directory(&parent)? {
            return Ok(DirectoryCompletion {
                suggestions: Vec::new(),
                validation_error,
            });
        }

        let fragment = fragment.to_lowercase();
        let mut suggestions = self
            .file_system
            .list_directories(&parent)?
            .into_iter()
            .filter_map(|directory| directory_name(&directory))
            .filter(|name| name.to_lowercase().starts_with(&fragment))
            .map(|name| DirectorySuggestion {
                value: format!("{parent_input}{name}"),
                name,
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.value.cmp(&right.value))
        });

        Ok(DirectoryCompletion {
            suggestions,
            validation_error,
        })
    }
}

fn completion_parts(input: &str) -> Option<(&str, &str)> {
    if input == "~" {
        return Some(("~/", ""));
    }
    if input == "$HOME" {
        return Some(("$HOME/", ""));
    }
    let separator = input.rfind('/')?;
    Some((&input[..=separator], &input[separator + 1..]))
}

fn directory_name(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_owned();
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use crate::{
        application::ports::{DirectoryCatalog, FileSystem},
        infrastructure::filesystem::local::LocalFileSystem,
    };

    use super::LocalDirectoryCatalog;

    #[test]
    fn completes_only_directories_and_preserves_user_path_syntax() {
        let Ok(home_directory) = tempfile::tempdir() else {
            return;
        };
        let home = home_directory.path().to_owned();
        if fs::create_dir(home.join("Code")).is_err()
            || fs::create_dir(home.join("Documents")).is_err()
            || fs::write(home.join("notes.txt"), "notes").is_err()
        {
            return;
        }
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let Ok(catalog) = LocalDirectoryCatalog::with_home(file_system, home) else {
            return;
        };

        let Ok(completion) = catalog.complete("~/") else {
            return;
        };
        assert_eq!(completion.validation_error, None);
        assert_eq!(
            completion
                .suggestions
                .iter()
                .map(|suggestion| suggestion.value.as_str())
                .collect::<Vec<_>>(),
            vec!["~/Code", "~/Documents"]
        );
    }

    #[test]
    fn refreshes_suggestions_for_nested_directories_and_reports_invalid_paths() {
        let Ok(home_directory) = tempfile::tempdir() else {
            return;
        };
        let home = home_directory.path().to_owned();
        let code = home.join("Code");
        if fs::create_dir(&code).is_err() || fs::create_dir(code.join("api")).is_err() {
            return;
        }
        let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        let Ok(catalog) = LocalDirectoryCatalog::with_home(file_system, home) else {
            return;
        };

        let Ok(completion) = catalog.complete("~/Code/") else {
            return;
        };
        assert_eq!(
            completion
                .suggestions
                .first()
                .map(|suggestion| suggestion.value.as_str()),
            Some("~/Code/api")
        );
        let Ok(invalid) = catalog.complete("~/Missing") else {
            return;
        };
        assert!(invalid.validation_error.is_some());
        assert!(invalid.suggestions.is_empty());
    }
}
