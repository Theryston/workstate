use crate::error::Result;

pub trait ConfigStore: Send + Sync {
    fn load(&self, environment: &str) -> Result<Option<Vec<u8>>>;
    fn save(&self, environment: &str, contents: &[u8]) -> Result<()>;
    fn delete(&self, environment: &str) -> Result<()>;
}

pub trait StateStore: Send + Sync {
    fn load(&self, environment: &str) -> Result<Option<Vec<u8>>>;
    fn save(&self, environment: &str, contents: &[u8]) -> Result<()>;
    fn delete(&self, environment: &str) -> Result<()>;
}
