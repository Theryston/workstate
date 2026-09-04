use crate::error::Result;

pub trait ContainerBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;
}
