use std::path::{Path, PathBuf};

use crate::error::Result;

pub trait FileSystem: Send + Sync {
    fn home_directory(&self) -> Result<PathBuf>;
    fn exists(&self, path: &Path) -> Result<bool>;
    fn is_directory(&self, path: &Path) -> Result<bool>;
    fn create_directory_all(&self, path: &Path) -> Result<()>;
    fn list_directories(&self, path: &Path) -> Result<Vec<PathBuf>>;
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
    fn write(&self, path: &Path, contents: &[u8]) -> Result<()>;
    fn sync(&self, path: &Path) -> Result<()>;
    fn rename(&self, source: &Path, target: &Path) -> Result<()>;
    fn canonicalize(&self, path: &Path) -> Result<PathBuf>;
    fn remove(&self, path: &Path) -> Result<()>;
}
