use std::path::{Path, PathBuf};

use tracing::{debug, info};

use crate::discovery::{DiscoveredPlugin, PluginDiscoveryPaths, discover_plugins};
use crate::manifest::load_plugin_manifest;
use crate::types::{PluginConfig, PluginError, PluginResult};

/// Configuration for the plugin loader.
#[derive(Debug, Clone)]
pub struct PluginLoaderConfig {
    /// Plugin discovery search paths.
    pub paths: PluginDiscoveryPaths,
    /// Plugin allowlist. If non-empty, only these plugin IDs will be loaded.
    pub allow: Vec<String>,
    /// Plugin denylist. These plugin IDs will never be loaded.
    pub deny: Vec<String>,
    /// Per-plugin configuration overrides.
    pub plugin_configs: std::collections::HashMap<String, PluginConfig>,
    /// Exclusive slot assignments: `{ "memory": "memory-lancedb" }`.
    /// Only one plugin per slot kind can be active.
    pub slots: std::collections::HashMap<String, String>,
}

impl Default for PluginLoaderConfig {
    fn default() -> Self {
        Self {
            paths: PluginDiscoveryPaths {
                config_paths: Vec::new(),
                workspace_dir: None,
                savfox_home: dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".savfox"),
                builtin_dir: None,
            },
            allow: Vec::new(),
            deny: Vec::new(),
            plugin_configs: std::collections::HashMap::new(),
            slots: std::collections::HashMap::new(),
        }
    }
}

/// Result of loading a single plugin.
#[derive(Debug)]
pub enum PluginLoadResult {
    /// Plugin loaded successfully.
    Loaded { id: String, path: PathBuf },
    /// Plugin was skipped (denied, not allowed, slot conflict).
    Skipped { id: String, reason: String },
    /// Plugin failed to load.
    Failed { path: PathBuf, error: String },
}

/// Load all discovered plugins into a registry.
///
/// This is the main entry point for the plugin system. It:
/// 1. Discovers plugins across all search paths
/// 2. Filters by allow/deny lists
/// 3. Enforces exclusive slot constraints
/// 4. Registers plugins with their configurations
pub fn load_plugins(config: &PluginLoaderConfig) -> Vec<PluginLoadResult> {
    let discovered = discover_plugins(&config.paths);
    let mut results = Vec::new();
    let mut active_slots: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Pre-populate slot assignments from config
    for (slot, plugin_id) in &config.slots {
        active_slots.insert(slot.clone(), plugin_id.clone());
    }

    for plugin in &discovered {
        let result = load_single_plugin(plugin, config, &mut active_slots);
        results.push(result);
    }

    let loaded_count = results
        .iter()
        .filter(|r| matches!(r, PluginLoadResult::Loaded { .. }))
        .count();
    let skipped_count = results
        .iter()
        .filter(|r| matches!(r, PluginLoadResult::Skipped { .. }))
        .count();
    let failed_count = results
        .iter()
        .filter(|r| matches!(r, PluginLoadResult::Failed { .. }))
        .count();

    info!(
        loaded = loaded_count,
        skipped = skipped_count,
        failed = failed_count,
        "Plugin loading complete"
    );

    results
}

fn load_single_plugin(
    discovered: &DiscoveredPlugin,
    config: &PluginLoaderConfig,
    active_slots: &mut std::collections::HashMap<String, String>,
) -> PluginLoadResult {
    // Load manifest
    let manifest = match load_plugin_manifest(&discovered.root_dir) {
        crate::manifest::ManifestLoadResult::Ok { manifest, .. } => manifest,
        crate::manifest::ManifestLoadResult::Err { error, .. } => {
            return PluginLoadResult::Failed {
                path: discovered.root_dir.clone(),
                error,
            };
        }
    };

    let id = &manifest.id;

    // Check denylist
    if config.deny.contains(id) {
        return PluginLoadResult::Skipped {
            id: id.clone(),
            reason: "Plugin is in deny list".to_string(),
        };
    }

    // Check allowlist (if non-empty, only allowed plugins load)
    if !config.allow.is_empty() && !config.allow.contains(id) {
        return PluginLoadResult::Skipped {
            id: id.clone(),
            reason: "Plugin is not in allow list".to_string(),
        };
    }

    // Check exclusive slot constraints
    let kind_str = format!("{:?}", manifest.kind).to_lowercase();
    if let Some(slot_owner) = active_slots.get(&kind_str) {
        if slot_owner != id {
            return PluginLoadResult::Skipped {
                id: id.clone(),
                reason: format!(
                    "Exclusive slot '{}' already assigned to '{}'",
                    kind_str, slot_owner
                ),
            };
        }
    }

    // Check per-plugin enabled flag
    if let Some(plugin_config) = config.plugin_configs.get(id) {
        if !plugin_config.enabled {
            return PluginLoadResult::Skipped {
                id: id.clone(),
                reason: "Plugin is disabled in configuration".to_string(),
            };
        }
    }

    // Register the slot if it's an exclusive-kind plugin
    if matches!(
        manifest.kind,
        crate::types::PluginKind::Memory | crate::types::PluginKind::Provider
    ) {
        active_slots.entry(kind_str).or_insert_with(|| id.clone());
    }

    debug!(
        id = %id,
        path = %discovered.root_dir.display(),
        isolation_mode = ?manifest.isolation_mode,
        "Plugin manifest loaded"
    );

    // Note: actual plugin instantiation from dylib/WASM happens at a higher level.
    // Here we register the manifest and config for later initialization.
    PluginLoadResult::Loaded {
        id: id.clone(),
        path: discovered.root_dir.clone(),
    }
}

/// Load a single plugin from a specific path (for `plugins install` flow).
pub fn load_plugin_from_path(path: &Path) -> PluginResult<crate::manifest::PluginManifest> {
    match load_plugin_manifest(path) {
        crate::manifest::ManifestLoadResult::Ok { manifest, .. } => Ok(manifest),
        crate::manifest::ManifestLoadResult::Err { error, .. } => {
            Err(PluginError::LoadFailed(error))
        }
    }
}
