use std::sync::Arc;

use crossterm::event::KeyCode;
use tempfile::tempdir;
use workstate::{
    application::ports::{ConfigStore, FileSystem, StateStore},
    domain::{
        ActionKind, ActionSpec, CommandSpec, EnvironmentConfig, EnvironmentSlug, ExecutionMode,
        MutationRecord, OwnershipStatus, ReadinessCheck, ResourceIdentity, ResourceKind,
        ResourceRecord, RunStatus, RuntimeState, TilingPreference, WorkspaceId, WorkspaceSpec,
        WorkspaceTarget,
    },
    infrastructure::{
        filesystem::{PathResolver, local::LocalFileSystem},
        persistence::{
            TomlConfigStore, TomlStateStore, WorkstatePaths, atomic_write::atomic_replace,
        },
    },
    ui::{EditorAction, EditorMode, EditorState},
};

fn sample_configuration() -> Option<EnvironmentConfig> {
    let mut configuration = EnvironmentConfig::new("Personal Blog").ok()?;
    let mut workspace = WorkspaceSpec::new(
        "api",
        WorkspaceTarget::Create {
            name: "API".to_owned(),
        },
    )
    .ok()?;
    workspace.tiling = TilingPreference::Enabled;
    configuration.add_workspace(workspace).ok()?;

    let mut action = ActionSpec::new("api-command", ActionKind::RunCommand).ok()?;
    action.working_directory = Some("$HOME/Projects/blog/api".to_owned());
    action.desktop_workspace = Some(WorkspaceId::new("api").ok()?);
    action.execution_mode = Some(ExecutionMode::RunOnce);
    action.parameters.command = Some(CommandSpec::new("bun"));
    action.readiness_checks.push(ReadinessCheck::None);
    configuration.add_action(action).ok()?;

    Some(configuration)
}

fn sample_runtime_state() -> Option<RuntimeState> {
    let slug = EnvironmentSlug::new("personal-blog").ok()?;
    let mut state = RuntimeState::new(slug, "run-1");
    let identity =
        ResourceIdentity::new(ResourceKind::TmuxSession, "workstate-personal-blog").ok()?;
    state
        .record_resource(ResourceRecord::new(
            identity,
            OwnershipStatus::CreatedByCurrentRun,
        ))
        .ok()?;
    let mut mutation = MutationRecord::new("workspace:api").ok()?;
    mutation.previous_value = Some("disabled".to_owned());
    mutation.applied_value = Some("enabled".to_owned());
    state.record_mutation(mutation).ok()?;
    state.set_status(RunStatus::Ready);
    Some(state)
}

fn test_file_system() -> Arc<dyn FileSystem> {
    Arc::new(LocalFileSystem)
}

fn required<T>(value: Option<T>, message: &str) -> Option<T> {
    assert!(value.is_some(), "{message}");
    value
}

#[test]
fn configuration_and_runtime_state_round_trip_through_separate_toml_files() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(configuration) = required(
        sample_configuration(),
        "sample configuration creation failed",
    ) else {
        return;
    };
    let Some(state) = required(
        sample_runtime_state(),
        "sample runtime state creation failed",
    ) else {
        return;
    };
    let file_system = test_file_system();
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let config_store = TomlConfigStore::new(Arc::clone(&file_system), paths.clone());
    let state_store = TomlStateStore::new(Arc::clone(&file_system), paths.clone());

    assert!(config_store.save(&configuration).is_ok());
    assert!(state_store.save(&state).is_ok());

    let loaded_configuration = config_store.load(&configuration.slug);
    let Some(loaded_configuration) = required(
        loaded_configuration.ok().flatten(),
        "configuration was not loaded",
    ) else {
        return;
    };
    assert_eq!(loaded_configuration, configuration);

    let loaded_state = state_store.load(&state.environment_slug);
    let Some(loaded_state) = required(loaded_state.ok().flatten(), "runtime state was not loaded")
    else {
        return;
    };
    assert_eq!(loaded_state, state);

    let Some(environment_paths) = required(
        paths.environment(&configuration.slug).ok(),
        "environment paths could not be resolved",
    ) else {
        return;
    };
    assert!(matches!(
        file_system.exists(environment_paths.configuration()),
        Ok(true)
    ));
    assert!(matches!(
        file_system.exists(environment_paths.state()),
        Ok(true)
    ));
    assert_ne!(environment_paths.configuration(), environment_paths.state());
}

#[test]
fn missing_toml_files_are_reported_as_absent() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(slug) = required(EnvironmentSlug::new("missing").ok(), "slug creation failed") else {
        return;
    };
    let file_system = test_file_system();
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let config_store = TomlConfigStore::new(Arc::clone(&file_system), paths.clone());
    let state_store = TomlStateStore::new(Arc::clone(&file_system), paths);

    assert!(matches!(config_store.load(&slug), Ok(None)));
    assert!(matches!(state_store.load(&slug), Ok(None)));
}

#[test]
fn malformed_and_unsupported_schema_files_fail_without_being_accepted() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(configuration) = required(
        sample_configuration(),
        "sample configuration creation failed",
    ) else {
        return;
    };
    let file_system = test_file_system();
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let Some(environment_paths) = required(
        paths.environment(&configuration.slug).ok(),
        "environment paths could not be resolved",
    ) else {
        return;
    };
    assert!(
        environment_paths
            .ensure_directories(file_system.as_ref())
            .is_ok()
    );
    let store = TomlConfigStore::new(Arc::clone(&file_system), paths.clone());

    assert!(
        file_system
            .write(environment_paths.configuration(), b"invalid = [")
            .is_ok()
    );
    assert!(store.load(&configuration.slug).is_err());

    let Some(state) = required(
        sample_runtime_state(),
        "sample runtime state creation failed",
    ) else {
        return;
    };
    let state_store = TomlStateStore::new(Arc::clone(&file_system), paths);

    let unsupported = b"schema_version = 999\nname = \"Personal Blog\"\nslug = \"personal-blog\"\n";
    assert!(
        file_system
            .write(environment_paths.configuration(), unsupported)
            .is_ok()
    );
    assert!(store.load(&configuration.slug).is_err());

    assert!(state_store.save(&state).is_ok());
    assert!(
        file_system
            .write(environment_paths.state(), b"invalid = [")
            .is_ok()
    );
    assert!(state_store.load(&state.environment_slug).is_err());
}

#[test]
fn atomic_replacement_preserves_a_directory_when_replacement_fails() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let target = home.path().join("existing-directory");
    let file_system = LocalFileSystem;
    assert!(file_system.create_directory_all(&target).is_ok());

    let result = atomic_replace(&file_system, &target, b"new contents");

    assert!(result.is_err());
    assert!(matches!(file_system.is_directory(&target), Ok(true)));
}

#[test]
fn configured_paths_expand_without_using_the_current_directory() {
    let Some(root) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let home = root.path().join("home");
    let project = home.join("Projects/blog/api");
    let file_system = LocalFileSystem;
    assert!(file_system.create_directory_all(&project).is_ok());
    let Some(resolver) = required(
        PathResolver::new(home.clone(), &file_system).ok(),
        "path resolver creation failed",
    ) else {
        return;
    };

    assert_eq!(
        resolver.expand("~/Projects/blog/api").ok(),
        Some(project.clone())
    );
    assert_eq!(
        resolver.expand("$HOME/Projects/blog/api").ok(),
        Some(project.clone())
    );
    assert_eq!(
        resolver.resolve_directory("~/Projects/blog/api").ok(),
        Some(project)
    );
    assert!(resolver.expand("relative/project").is_err());
    assert!(resolver.expand("~/../outside").is_err());
    assert!(resolver.expand("$OTHER/project").is_err());
}

#[test]
fn canceled_edits_do_not_change_the_original_configuration_bytes() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(configuration) = required(
        sample_configuration(),
        "sample configuration creation failed",
    ) else {
        return;
    };
    let file_system = test_file_system();
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let store = TomlConfigStore::new(Arc::clone(&file_system), paths.clone());
    assert!(store.save(&configuration).is_ok());
    let Some(environment_paths) = required(
        paths.environment(&configuration.slug).ok(),
        "environment paths could not be resolved",
    ) else {
        return;
    };
    let before = file_system.read(environment_paths.configuration());
    let Some(before) = required(before.ok(), "configuration bytes could not be read") else {
        return;
    };

    let loaded = store.load(&configuration.slug);
    assert!(loaded.is_ok());
    let Some(loaded) = loaded.ok().flatten() else {
        return;
    };
    let mut editor = EditorState::new(loaded, EditorMode::Edit);
    assert!(editor.configuration.rename("Changed Blog").is_ok());
    assert_eq!(
        editor.handle_key(KeyCode::Esc),
        EditorAction::CancelRequested
    );

    let after = file_system.read(environment_paths.configuration());
    assert_eq!(after.ok(), Some(before));
}

#[test]
fn creating_a_second_environment_with_the_same_slug_is_rejected() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(first) = required(
        sample_configuration(),
        "sample configuration creation failed",
    ) else {
        return;
    };
    let Some(second) = required(
        EnvironmentConfig::new("personal_blog").ok(),
        "colliding configuration creation failed",
    ) else {
        return;
    };
    assert_eq!(first.slug, second.slug);
    let file_system = test_file_system();
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let store = TomlConfigStore::new(Arc::clone(&file_system), paths);

    assert!(store.create(&first).is_ok());
    assert!(store.create(&second).is_err());
    let loaded = store.load(&first.slug);
    assert!(loaded.is_ok());
}

#[test]
fn environment_deletion_targets_cannot_escape_the_workstate_root() {
    let Some(home) = required(tempdir().ok(), "temporary directory creation failed") else {
        return;
    };
    let Some(slug) = required(
        EnvironmentSlug::new("safe-environment").ok(),
        "slug creation failed",
    ) else {
        return;
    };
    let Some(paths) = required(
        WorkstatePaths::new(home.path().to_path_buf()).ok(),
        "Workstate path creation failed",
    ) else {
        return;
    };
    let Some(environment_paths) = required(
        paths.environment(&slug).ok(),
        "environment paths could not be resolved",
    ) else {
        return;
    };

    assert!(
        environment_paths
            .deletion_target()
            .strip_prefix(paths.root())
            .is_ok()
    );
    assert_ne!(environment_paths.deletion_target(), paths.root());
    assert!(EnvironmentSlug::new("../outside").is_err());
}
