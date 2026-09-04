use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use crate::{
    application::ports::FileSystem,
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFileSystem;

impl FileSystem for LocalFileSystem {
    fn home_directory(&self) -> Result<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            WorkstateError::new(ErrorCategory::Persistence, "HOME is not configured")
        })
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error("checking path existence", path, error)),
        }
    }

    fn is_directory(&self, path: &Path) -> Result<bool> {
        fs::metadata(path)
            .map(|metadata| metadata.is_dir())
            .map_err(|error| io_error("checking directory type", path, error))
    }

    fn create_directory_all(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).map_err(|error| io_error("creating directory", path, error))
    }

    fn list_directories(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let entries =
            fs::read_dir(path).map_err(|error| io_error("listing directories", path, error))?;
        let mut directories = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|error| io_error("reading directory entry", path, error))?;
            let entry_type = entry
                .file_type()
                .map_err(|error| io_error("reading directory entry type", path, error))?;
            if entry_type.is_dir() {
                directories.push(entry.path());
            }
        }

        directories.sort();
        Ok(directories)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        fs::read(path).map_err(|error| io_error("reading file", path, error))
    }

    fn write(&self, path: &Path, contents: &[u8]) -> Result<()> {
        fs::write(path, contents).map_err(|error| io_error("writing file", path, error))
    }

    fn sync(&self, path: &Path) -> Result<()> {
        OpenOptions::new()
            .read(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error("syncing file", path, error))
    }

    fn rename(&self, source: &Path, target: &Path) -> Result<()> {
        fs::rename(source, target).map_err(|error| {
            WorkstateError::with_source(
                ErrorCategory::Persistence,
                "atomically replacing persisted file failed",
                error,
            )
            .with_context("source", source.display().to_string())
            .with_context("target", target.display().to_string())
        })
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        fs::canonicalize(path).map_err(|error| io_error("canonicalizing path", path, error))
    }

    fn remove(&self, path: &Path) -> Result<()> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error("inspecting path for removal", path, error)),
        };

        if metadata.file_type().is_dir() {
            fs::remove_dir_all(path).map_err(|error| io_error("removing directory", path, error))
        } else {
            fs::remove_file(path).map_err(|error| io_error("removing file", path, error))
        }
    }
}

fn io_error(operation: &str, path: &Path, source: io::Error) -> WorkstateError {
    WorkstateError::with_source(ErrorCategory::Persistence, operation, source)
        .with_context("path", path.display().to_string())
}
