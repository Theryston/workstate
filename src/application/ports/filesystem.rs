use std::path::{Path, PathBuf};

use crate::error::Result;

pub trait FileSystem: Send + Sync {
    fn home_directory(&self) -> Result<PathBuf>;
    fn exists(&self, path: &Path) -> Result<bool>;
    fn create_directory_all(&self, path: &Path) -> Result<()>;
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
    fn write(&self, path: &Path, contents: &[u8]) -> Result<()>;
    fn remove(&self, path: &Path) -> Result<()>;
}
