use crate::error::Result;

pub trait EditorBackend: Send + Sync {
    fn is_available(&self) -> Result<bool>;
}
