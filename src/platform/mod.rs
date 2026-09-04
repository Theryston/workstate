use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub operating_system: String,
    pub distribution: Option<String>,
    pub desktop_environment: Option<String>,
    pub terminal: Option<String>,
}

impl PlatformInfo {
    pub fn unknown() -> Self {
        Self::default()
    }
}
