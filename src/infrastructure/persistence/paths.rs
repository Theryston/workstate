use std::path::{Path, PathBuf};

use crate::{
    application::ports::FileSystem,
    domain::EnvironmentSlug,
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkstatePaths {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPaths {
    directory: PathBuf,
    configuration: PathBuf,
    state: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
}

impl WorkstatePaths {
    pub fn new(home_directory: PathBuf) -> Result<Self> {
        if !home_directory.is_absolute() {
            return Err(path_error("the home directory must be absolute"));
        }

        Ok(Self {
            root: home_directory.join(".workstate"),
        })
    }

    pub fn from_file_system(file_system: &dyn FileSystem) -> Result<Self> {
        Self::new(file_system.home_directory()?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn environment(&self, slug: &EnvironmentSlug) -> Result<EnvironmentPaths> {
        let directory = self.root.join(slug.as_str());
        ensure_environment_child(&self.root, &directory)?;

        Ok(EnvironmentPaths {
            configuration: directory.join("environment.toml"),
            state: directory.join("state.toml"),
            logs: directory.join("logs"),
            runtime: directory.join("runtime"),
            directory,
        })
    }
}

impl EnvironmentPaths {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn configuration(&self) -> &Path {
        &self.configuration
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    pub fn logs(&self) -> &Path {
        &self.logs
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub fn ensure_directories(&self, file_system: &dyn FileSystem) -> Result<()> {
        file_system.create_directory_all(&self.directory)?;
        file_system.create_directory_all(&self.logs)?;
        file_system.create_directory_all(&self.runtime)?;
        Ok(())
    }

    pub fn deletion_target(&self) -> &Path {
        &self.directory
    }
}

fn ensure_environment_child(root: &Path, candidate: &Path) -> Result<()> {
    if candidate == root || candidate.strip_prefix(root).is_err() {
        return Err(
            path_error("environment path would escape the Workstate root")
                .with_context("root", root.display().to_string())
                .with_context("candidate", candidate.display().to_string()),
        );
    }

    Ok(())
}

fn path_error(message: impl Into<String>) -> WorkstateError {
    WorkstateError::new(ErrorCategory::Persistence, message)
}
