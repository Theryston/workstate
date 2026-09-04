use crate::error::Result;

pub trait EmulatorBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;
}
