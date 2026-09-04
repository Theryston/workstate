use crate::error::Result;

pub trait TerminalBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;
}
