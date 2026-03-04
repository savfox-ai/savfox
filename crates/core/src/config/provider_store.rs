use chrono::{DateTime, Utc};
use savfox_app_server_protocol::AuthMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::token_data::TokenData;

pub const PROVIDER_STORE_FILE_VERSION: u32 = 2;

fn default_provider_file_version() -> u32 {
    PROVIDER_STORE_FILE_VERSION
}

fn default_auth_type() -> String {
    "api_key".to_string()
}

/// Persisted provider store file under `SAVFOX_HOME/models/<provider>.json`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ProviderStoreFile {
    #[serde(default = "default_provider_file_version")]
    pub version: u32,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub auth: Option<ProviderStoreAuth>,
    #[serde(default)]
    pub enabled_models: Vec<String>,
    /// Legacy model list field still accepted on read for backwards compatibility.
    #[serde(default, rename = "models", skip_serializing)]
    pub legacy_models: Vec<Value>,
}

impl ProviderStoreFile {
    pub fn empty(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            ..Self::default()
        }
    }
}

impl Default for ProviderStoreFile {
    fn default() -> Self {
        Self {
            version: PROVIDER_STORE_FILE_VERSION,
            provider_id: String::new(),
            display_name: String::new(),
            auth: None,
            enabled_models: Vec::new(),
            legacy_models: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ProviderStoreAuth {
    #[serde(rename = "type", default = "default_auth_type")]
    pub auth_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,
}

impl Default for ProviderStoreAuth {
    fn default() -> Self {
        Self {
            auth_type: default_auth_type(),
            env_key: None,
            api_key: None,
            auth_mode: None,
            tokens: None,
            last_refresh: None,
        }
    }
}
