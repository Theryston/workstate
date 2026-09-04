use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    application::ports::FileSystem,
    error::{ErrorCategory, Result, WorkstateError},
};

pub fn atomic_replace(file_system: &dyn FileSystem, target: &Path, contents: &[u8]) -> Result<()> {
    let temporary = temporary_path(target)?;

    if let Err(error) = file_system.write(&temporary, contents) {
        return Err(with_cleanup_context(file_system, &temporary, error));
    }

    if let Err(error) = file_system.sync(&temporary) {
        return Err(with_cleanup_context(file_system, &temporary, error));
    }

    if let Err(error) = file_system.rename(&temporary, target) {
        return Err(with_cleanup_context(file_system, &temporary, error));
    }

    Ok(())
}

fn temporary_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            WorkstateError::new(
                ErrorCategory::Persistence,
                "cannot create an atomic-write temporary filename",
            )
        })?;

    let timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    };
    let temporary_name = format!(
        ".{file_name}.{process_id}.{timestamp}.tmp",
        process_id = std::process::id()
    );

    Ok(target.with_file_name(temporary_name))
}

fn with_cleanup_context(
    file_system: &dyn FileSystem,
    temporary: &Path,
    error: WorkstateError,
) -> WorkstateError {
    match file_system.exists(temporary) {
        Ok(false) => error,
        Ok(true) => match file_system.remove(temporary) {
            Ok(()) => error,
            Err(cleanup_error) => {
                error.with_context("temporary_cleanup_error", cleanup_error.to_string())
            }
        },
        Err(check_error) => {
            error.with_context("temporary_cleanup_check_error", check_error.to_string())
        }
    }
}
