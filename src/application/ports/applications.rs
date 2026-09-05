use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApplication {
    pub id: String,
    pub name: String,
}

pub trait ApplicationCatalog: Send + Sync {
    fn list(&self) -> Result<Vec<InstalledApplication>>;
}
