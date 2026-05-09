//! Plugin system  - trait-based plugin API for extending the gateway.

use std::collections::HashMap;

use savfox_utils::home_dir::PLUGINS_SUBDIR;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const PRIMARY_MANIFEST_FILE: &str = "savfox.plugin.toml";

/// Plugin metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
}

/// Plugin state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Registered,
    Enabled,
    Disabled,
    Error,
}

/// Plugin config schema field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfigField {
    pub name: String,
    pub field_type: ConfigFieldType,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub help_text: Option<String>,
    #[serde(default)]
    pub is_sensitive: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldType {
    String,
    Number,
    Bool,
    Select,
    Password,
    Textarea,
}

/// Plugin config schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfigSchema {
    pub fields: Vec<PluginConfigField>,
}

/// Plugin lifecycle trait
pub trait PluginLifecycle: Send + Sync {
    /// Called when the plugin is enabled
    fn on_enable(&self) {}
    /// Called when the plugin is disabled
    fn on_disable(&self) {}
    /// Called when config changes
    fn on_config_change(&self, _config: &Value) {}
    /// Health check
    fn health_check(&self) -> bool {
        true
    }
}

/// A registered plugin with its metadata and state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredPlugin {
    pub info: PluginInfo,
    pub state: PluginState,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub config_schema: Option<PluginConfigSchema>,
}

/// HTTP route metadata exposed to the debug UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpRoute {
    pub plugin_id: String,
    pub method: String,
    pub path: String,
    pub requires_auth: bool,
    pub rate_limit_per_minute: u32,
    pub enabled: bool,
}

/// Plugin manifest loaded from `plugin.toml` in a plugin directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Entry point type: "subprocess" (default), "wasm", or "script"
    #[serde(default = "default_entry_type")]
    pub entry_type: String,
    /// Entry point path relative to plugin directory
    #[serde(default)]
    pub entry_point: Option<String>,
    /// Config schema fields
    #[serde(default)]
    pub config_fields: Vec<PluginConfigField>,
}

fn default_entry_type() -> String {
    "subprocess".to_owned()
}

/// Plugin registry
#[derive(Debug, Default)]
pub struct PluginRegistry {
    plugins: HashMap<String, RegisteredPlugin>,
}

impl PluginRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Register a plugin
    pub fn register(&mut self, info: PluginInfo, config_schema: Option<PluginConfigSchema>) {
        let id = info.id.clone();
        self.plugins.insert(
            id,
            RegisteredPlugin {
                info,
                state: PluginState::Registered,
                config: Value::Null,
                config_schema,
            },
        );
    }

    /// Enable a plugin
    pub fn enable(&mut self, id: &str) -> Result<(), String> {
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| format!("Plugin not found: {id}"))?;
        plugin.state = PluginState::Enabled;
        Ok(())
    }

    /// Disable a plugin
    pub fn disable(&mut self, id: &str) -> Result<(), String> {
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| format!("Plugin not found: {id}"))?;
        plugin.state = PluginState::Disabled;
        Ok(())
    }

    /// Get a plugin
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&RegisteredPlugin> {
        self.plugins.get(id)
    }

    /// List all plugins
    #[must_use]
    pub fn list(&self) -> Vec<&RegisteredPlugin> {
        self.plugins.values().collect()
    }

    /// Update plugin config
    pub fn set_config(&mut self, id: &str, config: Value) -> Result<(), String> {
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| format!("Plugin not found: {id}"))?;

        if let Some(schema) = &plugin.config_schema {
            validate_plugin_config(schema, &config)?;
        }

        plugin.config = config;
        Ok(())
    }

    /// Remove a plugin from the registry
    pub fn remove(&mut self, id: &str) -> bool {
        self.plugins.remove(id).is_some()
    }
}

// ── Plugin Loader ───────────────────────────────────────────────────────────

/// Discovers and loads plugins from a directory.
///
/// Plugins are stored in `{savfox_home}/plugins/`. Each plugin is a
/// subdirectory containing an `savfox.plugin.toml` manifest.
pub struct PluginLoader {
    plugins_dir: std::path::PathBuf,
    state_path: std::path::PathBuf,
}

impl std::fmt::Debug for PluginLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginLoader")
            .field("plugins_dir", &self.plugins_dir)
            .finish()
    }
}

impl PluginLoader {
    /// Create a new loader rooted at `{savfox_home}/plugins/`.
    #[must_use]
    pub fn new(savfox_home: &std::path::Path) -> Self {
        Self {
            plugins_dir: savfox_home.join(PLUGINS_SUBDIR),
            state_path: savfox_home.join("plugins-state.json"),
        }
    }

    /// Discover plugins from the filesystem and populate the registry.
    pub async fn discover(&self, registry: &mut PluginRegistry) -> Result<usize, String> {
        let dir = &self.plugins_dir;
        if !dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        let mut entries = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| format!("failed to read plugins dir: {e}"))?;

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = resolve_manifest_path(&path);
            let Some(manifest_path) = manifest_path else {
                continue;
            };

            match self.load_manifest(&manifest_path).await {
                Ok(manifest) => {
                    let id = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_owned();

                    let info = PluginInfo {
                        id: id.clone(),
                        name: manifest.name,
                        version: manifest.version,
                        description: manifest.description.unwrap_or_default(),
                        author: manifest.author,
                    };

                    let config_schema = if manifest.config_fields.is_empty() {
                        None
                    } else {
                        Some(PluginConfigSchema {
                            fields: manifest.config_fields,
                        })
                    };

                    registry.register(info, config_schema);
                    count += 1;
                }
                Err(err) => {
                    tracing::warn!(path = %manifest_path.display(), "failed to load plugin manifest: {err}");
                }
            }
        }

        // Load saved state (enabled/disabled, config)
        self.load_state(registry).await;

        Ok(count)
    }

    async fn load_manifest(&self, path: &std::path::Path) -> Result<PluginManifest, String> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        toml::from_str::<PluginManifest>(&content).map_err(|e| format!("parse error: {e}"))
    }

    /// Load persisted plugin state (enabled/disabled status, config).
    async fn load_state(&self, registry: &mut PluginRegistry) {
        let content = match tokio::fs::read_to_string(&self.state_path).await {
            Ok(c) => c,
            Err(_) => return,
        };

        let state: HashMap<String, Value> = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(_) => return,
        };

        for (id, entry) in state {
            if let Some(enabled) = entry.get("enabled").and_then(|v| v.as_bool()) {
                if enabled {
                    let _ = registry.enable(&id);
                } else {
                    let _ = registry.disable(&id);
                }
            }
            if let Some(config) = entry.get("config") {
                let _ = registry.set_config(&id, config.clone());
            }
        }
    }

    /// Save plugin state to disk.
    pub async fn save_state(&self, registry: &PluginRegistry) -> Result<(), String> {
        let mut state: HashMap<String, Value> = HashMap::new();

        for plugin in registry.list() {
            state.insert(
                plugin.info.id.clone(),
                serde_json::json!({
                    "enabled": plugin.state == PluginState::Enabled,
                    "config": plugin.config,
                }),
            );
        }

        crate::json_store::save_json(&self.state_path, &state, "plugin state").await
    }

    /// Return the plugins directory path.
    #[must_use]
    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.plugins_dir
    }
}

fn resolve_manifest_path(plugin_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let primary = plugin_dir.join(PRIMARY_MANIFEST_FILE);
    if primary.exists() {
        return Some(primary);
    }

    None
}

fn validate_plugin_config(schema: &PluginConfigSchema, config: &Value) -> Result<(), String> {
    let Some(config_obj) = config.as_object() else {
        return Err("plugin config must be a JSON object".to_owned());
    };

    for key in config_obj.keys() {
        if schema.fields.iter().all(|field| field.name != *key) {
            return Err(format!("unknown plugin config field '{key}'"));
        }
    }

    for field in &schema.fields {
        let value = config_obj.get(&field.name);
        if field.required && is_missing_required_value(value) {
            return Err(format!(
                "missing required plugin config field '{}'",
                field.name
            ));
        }

        if let Some(value) = value {
            validate_plugin_config_field(field, value)?;
        }
    }

    Ok(())
}

fn is_missing_required_value(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        _ => false,
    }
}

fn validate_plugin_config_field(field: &PluginConfigField, value: &Value) -> Result<(), String> {
    let is_valid = match field.field_type {
        ConfigFieldType::String
        | ConfigFieldType::Password
        | ConfigFieldType::Textarea
        | ConfigFieldType::Select => value.is_string(),
        ConfigFieldType::Number => value.is_number(),
        ConfigFieldType::Bool => value.is_boolean(),
    };

    if !is_valid {
        return Err(format!(
            "plugin config field '{}' has invalid type for {:?}",
            field.name, field.field_type
        ));
    }

    if matches!(field.field_type, ConfigFieldType::Select)
        && let (Some(options), Some(selected)) = (&field.options, value.as_str())
        && !options.iter().any(|opt| opt == selected)
    {
        return Err(format!(
            "plugin config field '{}' must be one of: {}",
            field.name,
            options.join(", ")
        ));
    }

    Ok(())
}

/// Discover all plugins from `{savfox_home}/plugins/` and return a snapshot.
pub async fn discover_snapshot(
    savfox_home: &std::path::Path,
) -> Result<Vec<RegisteredPlugin>, String> {
    let loader = PluginLoader::new(savfox_home);
    let mut registry = PluginRegistry::new();
    loader.discover(&mut registry).await?;
    let mut plugins: Vec<RegisteredPlugin> = registry.list().into_iter().cloned().collect();
    plugins.sort_by(|a, b| a.info.id.cmp(&b.info.id));
    Ok(plugins)
}

/// Build REST route descriptors for plugin HTTP endpoints.
#[must_use]
pub fn describe_http_routes(
    plugins: &[RegisteredPlugin],
    rate_limit_per_minute: u32,
) -> Vec<PluginHttpRoute> {
    plugins
        .iter()
        .map(|plugin| PluginHttpRoute {
            plugin_id: plugin.info.id.clone(),
            method: "POST".to_owned(),
            path: format!("/plugins/{}/...", plugin.info.id),
            requires_auth: true,
            rate_limit_per_minute,
            enabled: plugin.state == PluginState::Enabled,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn discovers_primary_savfox_manifest_file() {
        let tmp = tempdir().expect("tempdir");
        let plugin_dir = tmp.path().join("plugins").join("demo");
        std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        std::fs::write(
            plugin_dir.join(PRIMARY_MANIFEST_FILE),
            r#"
name = "Demo Plugin"
version = "0.1.0"
description = "Demo"
"#,
        )
        .expect("write manifest");

        let loader = PluginLoader::new(tmp.path());
        let mut registry = PluginRegistry::new();
        let count = loader.discover(&mut registry).await.expect("discover");

        assert_eq!(count, 1);
        assert!(registry.get("demo").is_some());
    }

    #[test]
    fn validates_schema_on_plugin_config_updates() {
        let mut registry = PluginRegistry::new();
        registry.register(
            PluginInfo {
                id: "demo".to_owned(),
                name: "Demo".to_owned(),
                version: "0.1.0".to_owned(),
                description: "Demo plugin".to_owned(),
                author: None,
            },
            Some(PluginConfigSchema {
                fields: vec![
                    PluginConfigField {
                        name: "api_key".to_owned(),
                        field_type: ConfigFieldType::String,
                        label: "API Key".to_owned(),
                        placeholder: None,
                        help_text: None,
                        is_sensitive: true,
                        required: true,
                        default_value: None,
                        options: None,
                    },
                    PluginConfigField {
                        name: "mode".to_owned(),
                        field_type: ConfigFieldType::Select,
                        label: "Mode".to_owned(),
                        placeholder: None,
                        help_text: None,
                        is_sensitive: false,
                        required: false,
                        default_value: None,
                        options: Some(vec!["safe".to_owned(), "full".to_owned()]),
                    },
                ],
            }),
        );

        let missing_required = registry.set_config("demo", json!({}));
        assert!(missing_required.is_err());

        let invalid_select = registry.set_config(
            "demo",
            json!({
                "api_key": "secret",
                "mode": "invalid"
            }),
        );
        assert!(invalid_select.is_err());

        let valid = registry.set_config(
            "demo",
            json!({
                "api_key": "secret",
                "mode": "safe"
            }),
        );
        assert!(valid.is_ok());
    }
}
