use tracing::{debug, info};

use crate::registry::PluginRegistry;
use crate::types::PluginResult;

/// Plugin lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    /// Plugin is registered but not yet initialized.
    Registered,
    /// Plugin is initializing.
    Initializing,
    /// Plugin is running and ready.
    Running,
    /// Plugin is shutting down.
    ShuttingDown,
    /// Plugin has been stopped.
    Stopped,
    /// Plugin encountered an error.
    Error,
}

impl std::fmt::Display for PluginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered => write!(f, "registered"),
            Self::Initializing => write!(f, "initializing"),
            Self::Running => write!(f, "running"),
            Self::ShuttingDown => write!(f, "shutting_down"),
            Self::Stopped => write!(f, "stopped"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Plugin status information.
#[derive(Debug, Clone)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub state: PluginState,
    pub error: Option<String>,
}

/// Manages the lifecycle of all registered plugins.
///
/// Handles initialization, shutdown, and error recovery.
#[derive(Debug)]
pub struct PluginLifecycleManager {
    states: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, PluginState>>>,
    errors: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
}

impl PluginLifecycleManager {
    pub fn new() -> Self {
        Self {
            states: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            errors: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Initialize all enabled plugins in the registry.
    pub async fn initialize_all(
        &self,
        registry: &PluginRegistry,
        workspace_dir: &std::path::Path,
    ) -> Vec<PluginResult<()>> {
        let enabled = registry.list_enabled().await;
        let mut results = Vec::new();

        for id in &enabled {
            let result = self.initialize_plugin(registry, id, workspace_dir).await;
            results.push(result);
        }

        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        let err_count = results.iter().filter(|r| r.is_err()).count();
        info!(
            ok = ok_count,
            errors = err_count,
            "Plugin initialization complete"
        );

        results
    }

    /// Initialize a single plugin.
    async fn initialize_plugin(
        &self,
        registry: &PluginRegistry,
        plugin_id: &str,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<()> {
        {
            let mut states = self.states.write().await;
            states.insert(plugin_id.to_string(), PluginState::Initializing);
        }

        debug!(id = %plugin_id, "Initializing plugin");

        match registry.start_plugin(plugin_id, workspace_dir).await {
            Ok(()) => {
                let mut states = self.states.write().await;
                states.insert(plugin_id.to_string(), PluginState::Running);
                debug!(id = %plugin_id, "Plugin initialized");
                Ok(())
            }
            Err(err) => {
                self.record_error(plugin_id, err.to_string()).await;
                Err(err)
            }
        }
    }

    /// Shutdown all plugins gracefully.
    pub async fn shutdown_all(&self, registry: &PluginRegistry, workspace_dir: &std::path::Path) {
        let running: Vec<String> = {
            let states = self.states.read().await;
            states
                .iter()
                .filter(|(_, s)| **s == PluginState::Running)
                .map(|(id, _)| id.clone())
                .collect()
        };

        for id in &running {
            let mut states = self.states.write().await;
            states.insert(id.clone(), PluginState::ShuttingDown);
        }

        for id in &running {
            if let Err(err) = registry.stop_plugin(id, workspace_dir).await {
                self.record_error(id, err.to_string()).await;
            }
        }

        {
            let mut states = self.states.write().await;
            for id in &running {
                states.insert(id.clone(), PluginState::Stopped);
            }
        }

        info!(count = running.len(), "All plugins shut down");
    }

    pub async fn uninstall_plugin(
        &self,
        registry: &PluginRegistry,
        plugin_id: &str,
        workspace_dir: &std::path::Path,
    ) -> PluginResult<()> {
        {
            let mut states = self.states.write().await;
            states.insert(plugin_id.to_string(), PluginState::ShuttingDown);
        }

        match registry.uninstall_plugin(plugin_id, workspace_dir).await {
            Ok(()) => {
                let mut states = self.states.write().await;
                let mut errors = self.errors.write().await;
                states.remove(plugin_id);
                errors.remove(plugin_id);
                Ok(())
            }
            Err(err) => {
                self.record_error(plugin_id, err.to_string()).await;
                Err(err)
            }
        }
    }

    /// Get the current state of a plugin.
    pub async fn state(&self, plugin_id: &str) -> PluginState {
        let states = self.states.read().await;
        states
            .get(plugin_id)
            .copied()
            .unwrap_or(PluginState::Registered)
    }

    /// Get status of all plugins.
    pub async fn all_statuses(&self, registry: &PluginRegistry) -> Vec<PluginStatus> {
        let states = self.states.read().await;
        let errors = self.errors.read().await;
        let plugin_list = registry.list().await;

        plugin_list
            .into_iter()
            .map(|meta| {
                let state = states
                    .get(&meta.id)
                    .copied()
                    .unwrap_or(PluginState::Registered);
                let error = errors.get(&meta.id).cloned();
                PluginStatus {
                    id: meta.id,
                    name: meta.name,
                    state,
                    error,
                }
            })
            .collect()
    }

    /// Record an error for a plugin.
    pub async fn record_error(&self, plugin_id: &str, error: String) {
        let mut states = self.states.write().await;
        let mut errors = self.errors.write().await;
        states.insert(plugin_id.to_string(), PluginState::Error);
        errors.insert(plugin_id.to_string(), error);
    }
}

impl Default for PluginLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}
