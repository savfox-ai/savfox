use chrono::{DateTime, Utc};
use savfox_app_server_protocol::AuthMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::token_data::TokenData;

pub const PROVIDER_STORE_FILE_VERSION: u32 = 2;
const PROVIDER_MODELS_DIR_NAME: &str = "models";

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

fn trim_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn normalize_model_slug(raw: &str) -> Option<String> {
    let raw = trim_nonempty(raw)?;
    if let Some((_provider_id, model_code)) = crate::parse_provider_prefixed_model(&raw) {
        return Some(model_code.to_string());
    }
    Some(raw)
}

fn model_slug_from_entry(item: &Value) -> Option<String> {
    item.get("model_code")
        .and_then(Value::as_str)
        .and_then(trim_nonempty)
        .or_else(|| item.get("id").and_then(Value::as_str).and_then(trim_nonempty))
        .or_else(|| item.get("model").and_then(Value::as_str).and_then(trim_nonempty))
        .or_else(|| item.as_str().and_then(trim_nonempty))
        .and_then(|raw| normalize_model_slug(&raw))
}

pub fn provider_models_store_dir(savfox_home: &Path) -> PathBuf {
    savfox_home.join(PROVIDER_MODELS_DIR_NAME)
}

pub fn provider_store_path(savfox_home: &Path, provider_id: &str) -> PathBuf {
    provider_models_store_dir(savfox_home).join(format!("{provider_id}.json"))
}

pub fn load_provider_store_file(savfox_home: &Path, provider_id: &str) -> ProviderStoreFile {
    let path = provider_store_path(savfox_home, provider_id);
    let data = std::fs::read_to_string(&path);
    let Ok(data) = data else {
        return ProviderStoreFile::empty(provider_id);
    };

    if let Ok(mut file) = serde_json::from_str::<ProviderStoreFile>(&data) {
        if file.provider_id.trim().is_empty() {
            file.provider_id = provider_id.to_string();
        }
        if file.enabled_models.is_empty() {
            file.enabled_models = file
                .legacy_models
                .iter()
                .filter_map(model_slug_from_entry)
                .collect();
        } else {
            file.enabled_models = file
                .enabled_models
                .iter()
                .filter_map(|slug| normalize_model_slug(slug))
                .collect();
        }
        return file;
    }

    if let Ok(legacy_models) = serde_json::from_str::<Vec<Value>>(&data) {
        let mut file = ProviderStoreFile::empty(provider_id);
        file.enabled_models = legacy_models
            .iter()
            .filter_map(model_slug_from_entry)
            .collect();
        file.legacy_models = legacy_models;
        return file;
    }

    ProviderStoreFile::empty(provider_id)
}

pub fn save_provider_store_file(
    savfox_home: &Path,
    provider_id: &str,
    file: &ProviderStoreFile,
) -> std::io::Result<()> {
    let dir = provider_models_store_dir(savfox_home);
    std::fs::create_dir_all(&dir)?;
    let path = provider_store_path(savfox_home, provider_id);
    let data = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    std::fs::write(path, data)
}
