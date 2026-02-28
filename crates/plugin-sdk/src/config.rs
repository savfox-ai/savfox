use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::PluginConfig;

/// Top-level plugin system configuration.
///
/// Mirrors the `[plugins]` section in savfox config.toml:
/// ```toml
/// [plugins]
/// enabled = true
/// allow = ["discord", "github"]
/// deny = ["untrusted-plugin"]
///
/// [plugins.load]
/// paths = ["/path/to/custom/plugins"]
///
/// [plugins.entries.discord]
/// enabled = true
/// settings = { token = "..." }
///
/// [plugins.slots]
/// memory = "memory-lancedb"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Whether the plugin system is enabled at all.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Allowlist of plugin IDs. If non-empty, only these are loaded.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Denylist of plugin IDs. These are never loaded.
    #[serde(default)]
    pub deny: Vec<String>,

    /// Plugin loading configuration.
    #[serde(default)]
    pub load: PluginLoadConfig,

    /// Per-plugin configuration entries.
    #[serde(default)]
    pub entries: HashMap<String, PluginConfig>,

    /// Exclusive slot assignments.
    /// Key is the slot category (e.g., "memory"), value is the plugin ID.
    #[serde(default)]
    pub slots: HashMap<String, String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow: Vec::new(),
            deny: Vec::new(),
            load: PluginLoadConfig::default(),
            entries: HashMap::new(),
            slots: HashMap::new(),
        }
    }
}

/// Configuration for plugin loading paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginLoadConfig {
    /// Additional directories to search for plugins.
    #[serde(default)]
    pub paths: Vec<std::path::PathBuf>,
}

fn default_true() -> bool {
    true
}

impl PluginsConfig {
    /// Check if a specific plugin should be loaded based on allow/deny lists.
    pub fn should_load(&self, plugin_id: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if self.deny.contains(&plugin_id.to_string()) {
            return false;
        }
        if !self.allow.is_empty() && !self.allow.contains(&plugin_id.to_string()) {
            return false;
        }
        // Check per-plugin enabled flag
        if let Some(entry) = self.entries.get(plugin_id) {
            return entry.enabled;
        }
        true
    }

    /// Get the assigned plugin ID for an exclusive slot.
    pub fn slot_assignment(&self, slot: &str) -> Option<&str> {
        self.slots.get(slot).map(|s| s.as_str())
    }

    /// Get the configuration for a specific plugin.
    pub fn plugin_config(&self, plugin_id: &str) -> PluginConfig {
        self.entries.get(plugin_id).cloned().unwrap_or_default()
    }
}
