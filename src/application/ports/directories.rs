use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySuggestion {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryCompletion {
    pub suggestions: Vec<DirectorySuggestion>,
    pub validation_error: Option<String>,
}

pub trait DirectoryCatalog: Send + Sync {
    fn complete(&self, input: &str) -> Result<DirectoryCompletion>;
}
