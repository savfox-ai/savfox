use serde::{Deserialize, Serialize};

pub const DEFAULT_REGISTRY_GIT: &str = "https://github.com/savfox-ai/registry.git";

/// Configuration for a git-based skill registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub git: String,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            git: DEFAULT_REGISTRY_GIT.to_string(),
        }
    }
}
