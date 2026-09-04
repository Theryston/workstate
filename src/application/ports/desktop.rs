use crate::error::Result;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopSnapshot;

pub trait DesktopBackend: Send + Sync {
    fn snapshot(&self) -> Result<DesktopSnapshot>;
}
