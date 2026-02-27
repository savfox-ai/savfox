use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::manifest::{PLUGIN_MANIFEST_FILENAME, load_plugin_manifest};
use crate::types::PluginResult;

/// Discovered plugin directory with its manifest path.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Root directory of the plugin.
    pub root_dir: PathBuf,
    /// Path to the manifest file.
    pub manifest_path: PathBuf,
}

/// Discovery order for plugin search paths.
///
/// 1. Explicit paths from config (`plugins.load.paths`)
/// 2. Workspace-local: `<workspace>/.savfox/extensions/`
/// 3. User-global: `~/.savfox/extensions/`
/// 4. Built-in plugins directory (optional)
#[derive(Debug, Clone)]
pub struct PluginDiscoveryPaths {
    /// Explicit paths from configuration.
    pub config_paths: Vec<PathBuf>,
    /// Workspace root (for `<workspace>/.savfox/extensions/`).
    pub workspace_dir: Option<PathBuf>,
    /// User home-based savfox dir (for `~/.savfox/extensions/`).
    pub savfox_home: PathBuf,
    /// Built-in plugins directory (shipped with the binary).
    pub builtin_dir: Option<PathBuf>,
}

impl PluginDiscoveryPaths {
    /// Collect all search directories in priority order.
    pub fn search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // 1. Config-specified paths (highest priority)
        dirs.extend(self.config_paths.iter().cloned());

        // 2. Workspace-local extensions
        if let Some(ws) = &self.workspace_dir {
            let ws_ext = ws.join(".savfox").join("extensions");
            if ws_ext.is_dir() {
                dirs.push(ws_ext);
            }
        }

        // 3. User-global extensions
        let global_ext = self.savfox_home.join("extensions");
        if global_ext.is_dir() {
            dirs.push(global_ext);
        }

        // 4. Built-in plugins (lowest priority)
        if let Some(builtin) = &self.builtin_dir {
            if builtin.is_dir() {
                dirs.push(builtin.clone());
            }
        }

        dirs
    }
}

/// Discover all plugins across the configured search paths.
///
/// Each subdirectory containing `savfox.plugin.json` is treated as a plugin.
/// Returns discovered plugins in priority order (config paths first, builtins last).
pub fn discover_plugins(paths: &PluginDiscoveryPaths) -> Vec<DiscoveredPlugin> {
    let search_dirs = paths.search_dirs();
    let mut discovered = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for dir in &search_dirs {
        debug!(dir = %dir.display(), "Scanning for plugins");

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "Failed to read plugin directory");
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join(PLUGIN_MANIFEST_FILENAME);
            if !manifest_path.exists() {
                continue;
            }

            // Load manifest to check ID for deduplication
            match load_plugin_manifest(&path) {
                crate::manifest::ManifestLoadResult::Ok { manifest, .. } => {
                    if seen_ids.contains(&manifest.id) {
                        debug!(
                            id = %manifest.id,
                            path = %path.display(),
                            "Skipping duplicate plugin (higher priority already loaded)"
                        );
                        continue;
                    }
                    seen_ids.insert(manifest.id.clone());
                    discovered.push(DiscoveredPlugin {
                        root_dir: path,
                        manifest_path,
                    });
                }
                crate::manifest::ManifestLoadResult::Err { error, .. } => {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Skipping plugin with invalid manifest"
                    );
                }
            }
        }
    }

    debug!(count = discovered.len(), "Plugin discovery complete");
    discovered
}

/// Check if a specific directory is a valid plugin directory.
pub fn is_plugin_dir(dir: &Path) -> bool {
    dir.is_dir() && dir.join(PLUGIN_MANIFEST_FILENAME).exists()
}

/// Resolve a plugin directory by ID, searching all paths.
pub fn find_plugin_by_id(
    paths: &PluginDiscoveryPaths,
    plugin_id: &str,
) -> PluginResult<Option<DiscoveredPlugin>> {
    let all = discover_plugins(paths);
    for discovered in all {
        match load_plugin_manifest(&discovered.root_dir) {
            crate::manifest::ManifestLoadResult::Ok { manifest, .. } => {
                if manifest.id == plugin_id {
                    return Ok(Some(discovered));
                }
            }
            _ => continue,
        }
    }
    Ok(None)
}
