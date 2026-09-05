use std::path::{Component, PathBuf};

use crate::{
    application::ports::FileSystem,
    error::{ErrorCategory, Result, WorkstateError},
};

pub mod directory_catalog;
pub mod file_catalog;
pub mod local;

pub use directory_catalog::LocalDirectoryCatalog;
pub use file_catalog::LocalFileCatalog;

pub struct PathResolver<'a> {
    home: PathBuf,
    file_system: &'a dyn FileSystem,
}

impl<'a> PathResolver<'a> {
    pub fn new(home: PathBuf, file_system: &'a dyn FileSystem) -> Result<Self> {
        if !home.is_absolute() {
            return Err(path_error("the configured home directory must be absolute"));
        }

        Ok(Self { home, file_system })
    }

    pub fn expand(&self, raw: &str) -> Result<PathBuf> {
        if raw.is_empty() || raw.contains('\0') {
            return Err(path_error(
                "configured path must be non-empty and contain no NUL characters",
            ));
        }

        let expanded = if raw == "~" {
            self.home.clone()
        } else if let Some(suffix) = raw.strip_prefix("~/") {
            self.home.join(suffix)
        } else if raw == "$HOME" {
            self.home.clone()
        } else if let Some(suffix) = raw.strip_prefix("$HOME/") {
            self.home.join(suffix)
        } else {
            PathBuf::from(raw)
        };

        if raw.starts_with('~') && raw != "~" && !raw.starts_with("~/") {
            return Err(path_error("only ~ and ~/path forms are supported"));
        }

        if expanded
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(path_error(
                "configured paths must not contain parent-directory traversal",
            ));
        }

        if !expanded.is_absolute() {
            return Err(path_error(
                "configured paths must be absolute or start with ~ or $HOME",
            ));
        }

        if expanded.to_string_lossy().contains('$') {
            return Err(path_error(
                "configured path contains an unresolved environment variable",
            ));
        }

        Ok(expanded)
    }

    pub fn resolve_directory(&self, raw: &str) -> Result<PathBuf> {
        let path = self.expand(raw)?;
        let exists = self.file_system.exists(&path)?;
        if !exists {
            return Err(path_error("required configured directory does not exist")
                .with_context("path", path.display().to_string()));
        }

        if !self.file_system.is_directory(&path)? {
            return Err(path_error("required configured path is not a directory")
                .with_context("path", path.display().to_string()));
        }

        Ok(path)
    }

    pub fn canonicalize_for_execution(&self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve_directory(raw)?;
        self.file_system
            .canonicalize(&path)
            .map_err(|error| error.with_context("configured_path", raw.to_owned()))
    }
}

fn path_error(message: impl Into<String>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Persistence, message)
}
