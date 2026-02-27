use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Tool,
    Channel,
    Provider,
    Memory,
    #[default]
    Utility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfigUiHint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default)]
    pub advanced: bool,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginChannelDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommandDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub accepts_args: bool,
    #[serde(default = "default_require_auth")]
    pub require_auth: bool,
}

fn default_require_auth() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookDefinition {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub isolation_mode: PluginIsolationMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginIsolationMode {
    #[default]
    InProcess,
    Subprocess,
    WasmSandbox,
}

impl PluginIsolationMode {
    pub fn is_isolated(self) -> bool {
        matches!(self, Self::Subprocess | Self::WasmSandbox)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub enabled: bool,
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            settings: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginContext {
    pub plugin_id: String,
    pub workspace_dir: std::path::PathBuf,
    pub config: PluginConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommandContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    pub channel: String,
    #[serde(default)]
    pub is_authorized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    pub command_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommandResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookEvent {
    pub name: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub cancel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
}

impl Default for PluginHookResult {
    fn default() -> Self {
        Self {
            payload: None,
            cancel: false,
            block_reason: None,
        }
    }
}

pub type PluginResult<T> = Result<T, PluginError>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Plugin already registered: {0}")]
    AlreadyRegistered(String),

    #[error("Failed to load plugin: {0}")]
    LoadFailed(String),

    #[error("Failed to parse manifest: {0}")]
    ManifestParseError(String),

    #[error("Plugin execution error: {0}")]
    ExecutionError(String),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}
