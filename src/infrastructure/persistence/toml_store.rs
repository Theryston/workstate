use std::sync::Arc;

use crate::{
    application::ports::{ConfigStore, FileSystem, StateStore},
    domain::{EnvironmentConfig, EnvironmentSlug, RuntimeState},
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::persistence::{atomic_write::atomic_replace, paths::WorkstatePaths},
};

pub struct TomlConfigStore {
    file_system: Arc<dyn FileSystem>,
    paths: WorkstatePaths,
}

pub struct TomlStateStore {
    file_system: Arc<dyn FileSystem>,
    paths: WorkstatePaths,
}

impl TomlConfigStore {
    pub fn new(file_system: Arc<dyn FileSystem>, paths: WorkstatePaths) -> Self {
        Self { file_system, paths }
    }
}

impl TomlStateStore {
    pub fn new(file_system: Arc<dyn FileSystem>, paths: WorkstatePaths) -> Self {
        Self { file_system, paths }
    }
}

impl ConfigStore for TomlConfigStore {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<EnvironmentConfig>> {
        let paths = self.paths.environment(environment)?;
        if !self.file_system.exists(paths.configuration())? {
            return Ok(None);
        }

        let bytes = self.file_system.read(paths.configuration())?;
        let contents = String::from_utf8(bytes).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "environment.toml is not valid UTF-8",
                source,
            )
            .with_context("path", paths.configuration().display().to_string())
        })?;
        let configuration = toml::from_str::<EnvironmentConfig>(&contents).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "environment.toml is not valid TOML",
                source,
            )
            .with_context("path", paths.configuration().display().to_string())
        })?;

        configuration.validate().map_err(WorkstateError::from)?;
        if configuration.slug != *environment {
            return Err(WorkstateError::new(
                ErrorCategory::Persistence,
                "environment.toml slug does not match its requested environment",
            )
            .with_context("requested_slug", environment.to_string())
            .with_context("stored_slug", configuration.slug.to_string())
            .with_context("path", paths.configuration().display().to_string()));
        }

        Ok(Some(configuration))
    }

    fn create(&self, configuration: &EnvironmentConfig) -> Result<()> {
        configuration.validate().map_err(WorkstateError::from)?;
        let paths = self.paths.environment(&configuration.slug)?;
        if self.file_system.exists(paths.configuration())? {
            return Err(WorkstateError::new(
                ErrorCategory::Persistence,
                "an environment with this slug already exists",
            )
            .with_context("slug", configuration.slug.to_string())
            .with_context("path", paths.configuration().display().to_string()));
        }

        self.save(configuration)
    }

    fn save(&self, configuration: &EnvironmentConfig) -> Result<()> {
        configuration.validate().map_err(WorkstateError::from)?;
        let paths = self.paths.environment(&configuration.slug)?;
        paths.ensure_directories(self.file_system.as_ref())?;
        let contents = toml::to_string_pretty(configuration).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "could not serialize environment configuration",
                source,
            )
        })?;

        atomic_replace(
            self.file_system.as_ref(),
            paths.configuration(),
            contents.as_bytes(),
        )
    }

    fn delete(&self, environment: &EnvironmentSlug) -> Result<()> {
        let paths = self.paths.environment(environment)?;
        if self.file_system.exists(paths.deletion_target())? {
            self.file_system.remove(paths.deletion_target())?;
        }
        Ok(())
    }

    fn list(&self) -> Result<Vec<EnvironmentSlug>> {
        if !self.file_system.exists(self.paths.root())? {
            return Ok(Vec::new());
        }

        let directories = self.file_system.list_directories(self.paths.root())?;
        let mut environments = Vec::with_capacity(directories.len());

        for directory in directories {
            let name = directory
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    WorkstateError::new(
                        ErrorCategory::Persistence,
                        "Workstate environment directory has an invalid name",
                    )
                    .with_context("path", directory.display().to_string())
                })?;
            let slug = EnvironmentSlug::new(name).map_err(WorkstateError::from)?;
            environments.push(slug);
        }

        environments.sort();
        Ok(environments)
    }
}

impl StateStore for TomlStateStore {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<RuntimeState>> {
        let paths = self.paths.environment(environment)?;
        if !self.file_system.exists(paths.state())? {
            return Ok(None);
        }

        let bytes = self.file_system.read(paths.state())?;
        let contents = String::from_utf8(bytes).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "state.toml is not valid UTF-8",
                source,
            )
            .with_context("path", paths.state().display().to_string())
        })?;
        let state = toml::from_str::<RuntimeState>(&contents).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "state.toml is not valid TOML",
                source,
            )
            .with_context("path", paths.state().display().to_string())
        })?;

        state.validate().map_err(WorkstateError::from)?;
        if state.environment_slug != *environment {
            return Err(WorkstateError::new(
                ErrorCategory::Persistence,
                "state.toml slug does not match its requested environment",
            )
            .with_context("requested_slug", environment.to_string())
            .with_context("stored_slug", state.environment_slug.to_string())
            .with_context("path", paths.state().display().to_string()));
        }

        Ok(Some(state))
    }

    fn save(&self, state: &RuntimeState) -> Result<()> {
        state.validate().map_err(WorkstateError::from)?;
        let paths = self.paths.environment(&state.environment_slug)?;
        paths.ensure_directories(self.file_system.as_ref())?;
        let contents = toml::to_string_pretty(state).map_err(|source| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "could not serialize runtime state",
                source,
            )
        })?;

        atomic_replace(
            self.file_system.as_ref(),
            paths.state(),
            contents.as_bytes(),
        )
    }

    fn delete(&self, environment: &EnvironmentSlug) -> Result<()> {
        let paths = self.paths.environment(environment)?;
        if self.file_system.exists(paths.state())? {
            self.file_system.remove(paths.state())?;
        }
        Ok(())
    }
}
