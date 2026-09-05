use crate::error::Result;

use super::directories::DirectoryCompletion;

pub trait FileCatalog: Send + Sync {
    fn complete_yaml(&self, working_directory: &str, input: &str) -> Result<DirectoryCompletion>;
}
