use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use savfox_app_server_protocol::AuthMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model_provider_info::ModelProviderInfo;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Value>,
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
            models: Vec::new(),
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
    if let Some((_provider_id, model_slug)) = crate::parse_provider_prefixed_model(&raw) {
        return Some(model_slug.to_string());
    }
    Some(raw)
}

fn model_slug_from_entry(item: &Value) -> Option<String> {
    item.get("model_slug")
        .and_then(Value::as_str)
        .and_then(trim_nonempty)
        .or_else(|| {
            item.get("id")
                .and_then(Value::as_str)
                .and_then(trim_nonempty)
        })
        .or_else(|| {
            item.get("model")
                .and_then(Value::as_str)
                .and_then(trim_nonempty)
        })
        .or_else(|| item.as_str().and_then(trim_nonempty))
        .and_then(|raw| normalize_model_slug(&raw))
}

fn env_var_looks_like_secret(env_var: &str) -> bool {
    let env_var = env_var.to_ascii_uppercase();
    env_var.contains("API_KEY") || env_var.contains("TOKEN") || env_var.ends_with("_KEY")
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
        file.enabled_models = file
            .enabled_models
            .iter()
            .filter_map(|slug| normalize_model_slug(slug))
            .collect();
        if file.enabled_models.is_empty() {
            file.enabled_models = file
                .models
                .iter()
                .filter_map(model_slug_from_entry)
                .collect();
        }
        return file;
    }

    if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
        let mut file = ProviderStoreFile::empty(provider_id);
        file.enabled_models = models.iter().filter_map(model_slug_from_entry).collect();
        file.models = models;
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

pub fn read_provider_store_api_key(savfox_home: &Path, provider_id: &str) -> Option<String> {
    load_provider_store_file(savfox_home, provider_id)
        .auth
        .and_then(|auth| auth.api_key)
        .and_then(|api_key| trim_nonempty(&api_key))
}

pub fn has_provider_store_configuration(savfox_home: &Path) -> bool {
    let dir = provider_models_store_dir(savfox_home);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };

    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            return false;
        }

        let provider_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .trim();
        if provider_id.is_empty() {
            return false;
        }

        let file = load_provider_store_file(savfox_home, provider_id);
        let has_models = !file.enabled_models.is_empty() || !file.models.is_empty();
        let has_api_key = file
            .auth
            .as_ref()
            .and_then(|auth| auth.api_key.as_deref())
            .is_some_and(|value| !value.trim().is_empty());
        let has_auth_object = std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<Value>(&data).ok())
            .and_then(|json| json.get("auth").cloned())
            .is_some_and(|auth| {
                !auth.is_null() && !auth.as_object().is_some_and(|obj| obj.is_empty())
            });

        has_models || has_api_key || has_auth_object
    })
}

pub fn provider_env_key_for_store(
    provider_id: &str,
    provider: &ModelProviderInfo,
) -> Option<String> {
    if let Some(env_key) = provider.env_key.as_deref().and_then(trim_nonempty) {
        return Some(env_key);
    }

    if let Some(env_headers) = &provider.env_http_headers {
        let mut env_keys: Vec<String> = env_headers
            .values()
            .filter_map(|env_key| trim_nonempty(env_key))
            .collect();
        env_keys.sort();
        env_keys.dedup();

        if let Some(env_key) = env_keys
            .iter()
            .find(|env_key| env_var_looks_like_secret(env_key))
        {
            return Some(env_key.clone());
        }
        if let Some(env_key) = env_keys.first() {
            return Some(env_key.clone());
        }
    }

    match provider_id.trim().to_ascii_lowercase().as_str() {
        "openai" => Some("OPENAI_API_KEY".to_string()),
        "anthropic" => Some("ANTHROPIC_API_KEY".to_string()),
        _ => None,
    }
}

pub fn persist_provider_connection(
    savfox_home: &Path,
    provider_id: &str,
    provider_name: &str,
    models: &[Value],
    env_key: Option<&str>,
    api_key: Option<&str>,
) -> std::io::Result<()> {
    let mut file = load_provider_store_file(savfox_home, provider_id);
    file.version = PROVIDER_STORE_FILE_VERSION;
    file.provider_id = provider_id.to_string();
    file.display_name = if provider_name.trim().is_empty() {
        provider_id.to_string()
    } else {
        provider_name.to_string()
    };
    file.enabled_models = models.iter().filter_map(model_slug_from_entry).collect();
    file.models = models.to_vec();

    if let Some(api_key) = api_key.and_then(trim_nonempty) {
        file.auth = Some(ProviderStoreAuth {
            auth_type: "api_key".to_string(),
            env_key: env_key.and_then(trim_nonempty),
            api_key: Some(api_key),
            ..ProviderStoreAuth::default()
        });
    }

    save_provider_store_file(savfox_home, provider_id, &file)
}
