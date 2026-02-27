use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::api::{BoxedPlugin, Plugin};
use crate::manifest::{ManifestLoadResult, PluginManifest, load_plugin_manifest};
use crate::types::{
    PluginCommandContext, PluginCommandResult, PluginConfig, PluginContext, PluginHookContext,
    PluginHookEvent, PluginHookResult, PluginIsolationMode, PluginMetadata, PluginResult,
};

pub struct RegisteredPlugin {
    pub plugin: BoxedPlugin,
    pub manifest: Option<PluginManifest>,
    pub config: PluginConfig,
    pub source_path: Option<PathBuf>,
    pub enabled: bool,
}

impl RegisteredPlugin {
    pub fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: self.plugin.id().to_string(),
            name: self.plugin.name().to_string(),
            version: self.plugin.version().map(|s| s.to_string()),
            description: self.plugin.description().map(|s| s.to_string()),
            author: self.manifest.as_ref().and_then(|m| m.author.clone()),
            homepage: self.manifest.as_ref().and_then(|m| m.homepage.clone()),
            license: self.manifest.as_ref().and_then(|m| m.license.clone()),
            kind: self.manifest.as_ref().map(|m| m.kind).unwrap_or_default(),
            tags: self
                .manifest
                .as_ref()
                .map(|m| m.tags.clone())
                .unwrap_or_default(),
            isolation_mode: self
                .manifest
                .as_ref()
                .map(|m| m.isolation_mode)
                .unwrap_or(PluginIsolationMode::InProcess),
        }
    }
}

pub struct PluginRegistry {
    plugins: Arc<RwLock<HashMap<String, RegisteredPlugin>>>,
    plugins_dir: PathBuf,
}

impl PluginRegistry {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins: Arc::new(RwLock::new(HashMap::new())),
            plugins_dir,
        }
    }

    pub async fn register(&self, plugin: BoxedPlugin) -> PluginResult<()> {
        self.register_with_config(plugin, PluginConfig::default(), None)
            .await
    }

    pub async fn register_with_config(
        &self,
        plugin: BoxedPlugin,
        config: PluginConfig,
        source_path: Option<PathBuf>,
    ) -> PluginResult<()> {
        let id = plugin.id().to_string();

        let manifest = source_path
            .as_ref()
            .and_then(|path| match load_plugin_manifest(path) {
                ManifestLoadResult::Ok { manifest, .. } => Some(manifest),
                ManifestLoadResult::Err { error, .. } => {
                    warn!("Failed to load manifest for plugin {}: {}", id, error);
                    None
                }
            });

        plugin.validate_config(&config)?;

        let registered = RegisteredPlugin {
            plugin,
            manifest,
            config,
            source_path,
            enabled: true,
        };

        let mut plugins = self.plugins.write().await;
        if plugins.contains_key(&id) {
            return Err(crate::types::PluginError::AlreadyRegistered(id));
        }

        info!("Registered plugin: {}", id);
        plugins.insert(id, registered);
        Ok(())
    }

    pub async fn unregister(&self, id: &str) -> PluginResult<()> {
        let mut plugins = self.plugins.write().await;
        if plugins.remove(id).is_some() {
            info!("Unregistered plugin: {}", id);
            Ok(())
        } else {
            Err(crate::types::PluginError::NotFound(id.to_string()))
        }
    }

    pub async fn get(&self, id: &str) -> Option<Arc<RegisteredPlugin>> {
        let plugins = self.plugins.read().await;
        plugins.get(id).map(|p| {
            Arc::new(RegisteredPlugin {
                plugin: Box::new(DummyPlugin {
                    id: p.plugin.id().to_string(),
                }),
                manifest: p.manifest.clone(),
                config: p.config.clone(),
                source_path: p.source_path.clone(),
                enabled: p.enabled,
            })
        })
    }

    pub async fn list(&self) -> Vec<PluginMetadata> {
        let plugins = self.plugins.read().await;
        plugins.values().map(|p| p.metadata()).collect()
    }

    pub async fn list_enabled(&self) -> Vec<String> {
        let plugins = self.plugins.read().await;
        plugins
            .values()
            .filter(|p| p.enabled)
            .map(|p| p.plugin.id().to_string())
            .collect()
    }

    pub async fn enable(&self, id: &str) -> PluginResult<()> {
        let mut plugins = self.plugins.write().await;
        if let Some(plugin) = plugins.get_mut(id) {
            plugin.enabled = true;
            info!("Enabled plugin: {}", id);
            Ok(())
        } else {
            Err(crate::types::PluginError::NotFound(id.to_string()))
        }
    }

    pub async fn disable(&self, id: &str) -> PluginResult<()> {
        let mut plugins = self.plugins.write().await;
        if let Some(plugin) = plugins.get_mut(id) {
            plugin.enabled = false;
            info!("Disabled plugin: {}", id);
            Ok(())
        } else {
            Err(crate::types::PluginError::NotFound(id.to_string()))
        }
    }

    pub async fn update_config(&self, id: &str, config: PluginConfig) -> PluginResult<()> {
        let mut plugins = self.plugins.write().await;
        if let Some(plugin) = plugins.get_mut(id) {
            plugin.plugin.validate_config(&config)?;
            plugin.config = config;
            info!("Updated config for plugin: {}", id);
            Ok(())
        } else {
            Err(crate::types::PluginError::NotFound(id.to_string()))
        }
    }

    pub async fn initialize_all(&self, workspace_dir: &std::path::Path) {
        let plugins = self.plugins.read().await;
        for (id, registered) in plugins.iter() {
            if !registered.enabled {
                continue;
            }
            let ctx = PluginContext {
                plugin_id: id.clone(),
                workspace_dir: workspace_dir.to_path_buf(),
                config: registered.config.clone(),
            };
            if let Err(e) = registered.plugin.start(ctx).await {
                error!("Failed to start plugin {}: {}", id, e);
            }
        }
    }

    pub async fn shutdown_all(&self, workspace_dir: &std::path::Path) {
        let plugins = self.plugins.read().await;
        for (id, registered) in plugins.iter() {
            let ctx = PluginContext {
                plugin_id: id.clone(),
                workspace_dir: workspace_dir.to_path_buf(),
                config: registered.config.clone(),
            };
            if let Err(e) = registered.plugin.stop(ctx).await {
                error!("Failed to stop plugin {}: {}", id, e);
            }
        }
    }

    pub async fn start_plugin(
        &self,
        plugin_id: &str,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<()> {
        let plugins = self.plugins.read().await;
        let Some(registered) = plugins.get(plugin_id) else {
            return Err(crate::types::PluginError::NotFound(plugin_id.to_string()));
        };
        let ctx = PluginContext {
            plugin_id: plugin_id.to_string(),
            workspace_dir: workspace_dir.to_path_buf(),
            config: registered.config.clone(),
        };
        registered.plugin.start(ctx).await
    }

    pub async fn stop_plugin(
        &self,
        plugin_id: &str,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<()> {
        let plugins = self.plugins.read().await;
        let Some(registered) = plugins.get(plugin_id) else {
            return Err(crate::types::PluginError::NotFound(plugin_id.to_string()));
        };
        let ctx = PluginContext {
            plugin_id: plugin_id.to_string(),
            workspace_dir: workspace_dir.to_path_buf(),
            config: registered.config.clone(),
        };
        registered.plugin.stop(ctx).await
    }

    pub async fn uninstall_plugin(
        &self,
        plugin_id: &str,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<()> {
        let registered = {
            let mut plugins = self.plugins.write().await;
            let Some(registered) = plugins.remove(plugin_id) else {
                return Err(crate::types::PluginError::NotFound(plugin_id.to_string()));
            };
            registered
        };
        let ctx = PluginContext {
            plugin_id: plugin_id.to_string(),
            workspace_dir: workspace_dir.to_path_buf(),
            config: registered.config.clone(),
        };
        registered.plugin.stop(ctx.clone()).await?;
        registered.plugin.uninstall(ctx).await
    }

    pub async fn execute_tool(
        &self,
        plugin_id: &str,
        tool_name: &str,
        params: serde_json::Value,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<serde_json::Value> {
        let plugins = self.plugins.read().await;
        if let Some(registered) = plugins.get(plugin_id) {
            if !registered.enabled {
                return Err(crate::types::PluginError::ExecutionError(format!(
                    "Plugin {} is disabled",
                    plugin_id
                )));
            }
            let ctx = PluginContext {
                plugin_id: plugin_id.to_string(),
                workspace_dir: workspace_dir.to_path_buf(),
                config: registered.config.clone(),
            };
            registered
                .plugin
                .execute_tool(tool_name.to_string(), params, ctx)
                .await
        } else {
            Err(crate::types::PluginError::NotFound(plugin_id.to_string()))
        }
    }

    pub async fn execute_command(
        &self,
        plugin_id: &str,
        command_name: &str,
        cmd_ctx: PluginCommandContext,
        workspace_dir: &std::path::Path,
    ) -> PluginCommandResult {
        let plugins = self.plugins.read().await;
        if let Some(registered) = plugins.get(plugin_id) {
            if !registered.enabled {
                return PluginCommandResult {
                    text: None,
                    error: Some(format!("Plugin {} is disabled", plugin_id)),
                };
            }
            let ctx = PluginContext {
                plugin_id: plugin_id.to_string(),
                workspace_dir: workspace_dir.to_path_buf(),
                config: registered.config.clone(),
            };
            registered
                .plugin
                .execute_command(command_name.to_string(), cmd_ctx, ctx)
                .await
        } else {
            PluginCommandResult {
                text: None,
                error: Some(format!("Plugin not found: {}", plugin_id)),
            }
        }
    }

    pub async fn handle_hook(
        &self,
        event: PluginHookEvent,
        hook_ctx: PluginHookContext,
        workspace_dir: &std::path::Path,
    ) -> PluginHookResult {
        let plugins = self.plugins.read().await;
        let mut results = Vec::new();

        for (id, registered) in plugins.iter() {
            if !registered.enabled {
                continue;
            }

            let hooks = registered.plugin.hooks();
            let should_handle = hooks.iter().any(|h| h.event == event.name);

            if !should_handle {
                continue;
            }

            let ctx = PluginContext {
                plugin_id: id.clone(),
                workspace_dir: workspace_dir.to_path_buf(),
                config: registered.config.clone(),
            };

            match registered
                .plugin
                .handle_hook(event.clone(), hook_ctx.clone(), ctx)
                .await
            {
                result if result.cancel => {
                    return result;
                }
                result => {
                    results.push(result);
                }
            }
        }

        results
            .into_iter()
            .fold(PluginHookResult::default(), |acc, r| {
                if r.cancel {
                    return r;
                }
                PluginHookResult {
                    payload: r.payload.or(acc.payload),
                    cancel: acc.cancel,
                    block_reason: r.block_reason.or(acc.block_reason),
                }
            })
    }

    pub async fn discover_plugins(&self) -> Vec<PathBuf> {
        let mut discovered = Vec::new();

        if !self.plugins_dir.exists() {
            return discovered;
        }

        if let Ok(entries) = std::fs::read_dir(&self.plugins_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join(crate::manifest::PLUGIN_MANIFEST_FILENAME);
                    if manifest_path.exists() {
                        discovered.push(path);
                    }
                }
            }
        }

        discovered
    }
}

struct DummyPlugin {
    id: String,
}

impl Plugin for DummyPlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        "dummy"
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new(PathBuf::from("plugins"))
    }
}
