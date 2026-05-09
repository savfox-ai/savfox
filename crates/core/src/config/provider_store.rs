use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use savfox_app_server_protocol::AuthMode;
pub use savfox_utils::string::slugify_account_id;
use savfox_utils::string::normalize_slug;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model_provider_info::{ModelProviderInfo, built_in_model_providers};
use crate::token_data::TokenData;

pub const PROVIDER_STORE_FILE_VERSION: u32 = 2;
const PROVIDER_MODELS_DIR_NAME: &str = "models";

fn default_provider_file_version() -> u32 {
    PROVIDER_STORE_FILE_VERSION
}

fn default_auth_type() -> String {
    "api_key".to_owned()
}

/// Persisted provider store file under `SAVFOX_HOME/models/<id>.json`.
///
/// Each file represents a single provider account. The `id` is the unique
/// account identifier (e.g. `"openai-work"`) used as the filename stem and
/// as a synthetic provider id for model routing. The underlying `provider_id`
/// (e.g. `"openai"`) determines wire protocol, base URL defaults, etc.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ProviderStoreFile {
    #[serde(default = "default_provider_file_version")]
    pub version: u32,
    /// Unique account identifier. Defaults to `provider_id` for legacy files.
    #[serde(default)]
    pub id: String,
    /// Normalized slug derived from `name` (e.g. "work-account").
    /// `id` is `{provider_id}-{slug}` when slug is non-empty.
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub auth: Option<ProviderStoreAuth>,
    #[serde(default)]
    pub disabled_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Value>,
    /// Timestamp of the last successful remote model fetch for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_fetched_at: Option<DateTime<Utc>>,
}

impl ProviderStoreFile {
    pub fn empty(provider_id: impl Into<String>) -> Self {
        let pid = provider_id.into();
        Self {
            id: pid.clone(),
            provider_id: pid,
            ..Self::default()
        }
    }

    /// Return the effective account id: the explicit `id` field if non-empty,
    /// otherwise fall back to `provider_id`.
    #[must_use]
    pub fn account_id(&self) -> &str {
        let id = self.id.trim();
        if id.is_empty() {
            self.provider_id.trim()
        } else {
            id
        }
    }

    /// Strip provider prefixes from each disabled-model entry and drop any
    /// empty / whitespace-only entries. Idempotent — safe to call on read,
    /// before write, and after a user mutation.
    pub fn normalize_disabled_models(&mut self) {
        self.disabled_models = self
            .disabled_models
            .iter()
            .filter_map(|slug| normalize_model_slug(slug))
            .collect();
    }
}

impl Default for ProviderStoreFile {
    fn default() -> Self {
        Self {
            version: PROVIDER_STORE_FILE_VERSION,
            id: String::new(),
            provider_id: String::new(),
            name: String::new(),
            slug: String::new(),
            auth: None,
            disabled_models: Vec::new(),
            models: Vec::new(),
            models_fetched_at: None,
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
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn normalize_model_slug(raw: &str) -> Option<String> {
    let raw = trim_nonempty(raw)?;
    if let Some((_provider_id, model_slug)) = crate::parse_provider_prefixed_model(&raw) {
        return Some(model_slug.to_owned());
    }
    Some(raw)
}

fn env_var_looks_like_secret(env_var: &str) -> bool {
    let env_var = env_var.to_ascii_uppercase();
    env_var.contains("API_KEY") || env_var.contains("TOKEN") || env_var.ends_with("_KEY")
}

#[must_use]
pub fn provider_models_store_dir(savfox_home: &Path) -> PathBuf {
    savfox_home.join(PROVIDER_MODELS_DIR_NAME)
}

#[must_use]
pub fn provider_store_path(savfox_home: &Path, account_id: &str) -> PathBuf {
    provider_models_store_dir(savfox_home).join(format!("{account_id}.json"))
}

/// Normalize a human-readable name into a URL/filename-safe slug.
///
/// Lowercases, replaces non-alphanumeric chars with hyphens, collapses
/// consecutive hyphens, and trims leading/trailing hyphens.
/// Check whether an account id already has a corresponding store file on disk.
#[must_use]
pub fn account_id_exists(savfox_home: &Path, account_id: &str) -> bool {
    provider_store_path(savfox_home, account_id).exists()
}

/// List all provider store files found in `SAVFOX_HOME/models/*.json`.
pub fn list_provider_store_files(savfox_home: &Path) -> Vec<ProviderStoreFile> {
    let dir = provider_models_store_dir(savfox_home);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let id_from_filename = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::trim)
            .unwrap_or("");
        if id_from_filename.is_empty() {
            continue;
        }
        let file = load_provider_store_file(savfox_home, id_from_filename);
        files.push(file);
    }
    files
}

/// Load a provider store file. The `account_id` is the filename stem (which
/// may be a bare `provider_id` for legacy single-account files, or a full
/// account id like `"openai-work"` for multi-account setups).
#[must_use]
pub fn load_provider_store_file(savfox_home: &Path, account_id: &str) -> ProviderStoreFile {
    let path = provider_store_path(savfox_home, account_id);
    let data = std::fs::read_to_string(&path);
    let Ok(data) = data else {
        return ProviderStoreFile::empty(account_id);
    };

    if let Ok(mut file) = serde_json::from_str::<ProviderStoreFile>(&data) {
        // Populate id from filename when missing in JSON (backward compat).
        if file.id.trim().is_empty() {
            file.id = account_id.to_owned();
        }
        if file.provider_id.trim().is_empty() {
            file.provider_id = account_id.to_owned();
        }
        // Derive slug from name when missing (backward compat with pre-slug files).
        if file.slug.trim().is_empty() && !file.name.trim().is_empty() {
            file.slug = normalize_slug(&file.name).unwrap_or_default();
        }
        file.normalize_disabled_models();
        return file;
    }

    if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
        let mut file = ProviderStoreFile::empty(account_id);
        file.disabled_models = Vec::new();
        file.models = models;
        return file;
    }

    ProviderStoreFile::empty(account_id)
}

pub fn save_provider_store_file(
    savfox_home: &Path,
    account_id: &str,
    file: &ProviderStoreFile,
) -> std::io::Result<()> {
    let dir = provider_models_store_dir(savfox_home);
    std::fs::create_dir_all(&dir)?;
    let path = provider_store_path(savfox_home, account_id);
    let data = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    std::fs::write(path, data)
}

#[must_use]
pub fn read_provider_store_api_key(savfox_home: &Path, provider_id: &str) -> Option<String> {
    load_provider_store_file(savfox_home, provider_id)
        .auth
        .and_then(|auth| auth.api_key)
        .and_then(|api_key| trim_nonempty(&api_key))
}

#[must_use]
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
        let has_models = !file.models.is_empty();
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
        "openai" => Some("OPENAI_API_KEY".to_owned()),
        "anthropic" => Some("ANTHROPIC_API_KEY".to_owned()),
        _ => None,
    }
}

/// Update provider store files with freshly fetched remote models.
///
/// For each store file whose `provider_id` matches `target_provider_id`
/// (canonicalized), the `models` and `models_fetched_at`
/// fields are overwritten with the supplied data.
pub fn update_provider_store_models(
    savfox_home: &Path,
    target_provider_id: &str,
    models: &[Value],
) -> std::io::Result<()> {
    let canonical = crate::canonical_provider_id(target_provider_id);
    let now = Utc::now();
    for mut file in list_provider_store_files(savfox_home) {
        let file_canonical = crate::canonical_provider_id(&file.provider_id);
        if file_canonical != canonical {
            continue;
        }
        file.models = models.to_vec();
        file.models_fetched_at = Some(now);
        save_provider_store_file(savfox_home, file.account_id(), &file)?;
    }
    Ok(())
}

pub fn persist_provider_connection(
    savfox_home: &Path,
    account_id: &str,
    provider_id: &str,
    account_name: &str,
    models: &[Value],
    env_key: Option<&str>,
    api_key: Option<&str>,
) -> std::io::Result<()> {
    let mut file = load_provider_store_file(savfox_home, account_id);
    let mut migrated_auth_from_provider_file = false;
    file.version = PROVIDER_STORE_FILE_VERSION;
    file.id = account_id.to_owned();
    file.provider_id = provider_id.to_owned();
    file.name = if account_name.trim().is_empty() {
        provider_id.to_owned()
    } else {
        account_name.trim().to_owned()
    };
    // slug = normalized(name), always derived from the effective name
    file.slug = normalize_slug(&file.name).unwrap_or_default();
    file.models = models.to_vec();

    if let Some(api_key) = api_key.and_then(trim_nonempty) {
        file.auth = Some(ProviderStoreAuth {
            auth_type: "api_key".to_owned(),
            env_key: env_key.and_then(trim_nonempty),
            api_key: Some(api_key),
            ..ProviderStoreAuth::default()
        });
    } else if file.auth.is_none() && account_id != provider_id {
        let source = load_provider_store_file(savfox_home, provider_id);
        if source.auth.is_some() {
            file.auth = source.auth;
            migrated_auth_from_provider_file = true;
        }
    }

    save_provider_store_file(savfox_home, account_id, &file)?;

    if migrated_auth_from_provider_file {
        let source_path = provider_store_path(savfox_home, provider_id);
        let target_path = provider_store_path(savfox_home, account_id);
        if source_path != target_path {
            match std::fs::remove_file(source_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
    }

    Ok(())
}

/// When `removed_account_id` was the active default provider, pick another
/// available provider and persist the change to `config.toml`.
///
/// Preference order for the replacement:
/// 1. Store-based providers (they have auth / models on disk).
/// 2. Built-in providers.
///
/// This is a best-effort operation: if the config cannot be read or the
/// removed provider was not the default, this is a no-op.
pub fn fallback_default_provider_if_removed(savfox_home: &Path, removed_account_id: &str) {
    use savfox_config::CONFIG_TOML_FILE;

    // 1. Read config.toml to see if the removed provider is actually the default.
    let config_path = savfox_home.join(CONFIG_TOML_FILE);
    let contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return, // No config file — nothing to fix.
    };
    let cfg: crate::config::ConfigToml = match toml::from_str(&contents) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Determine the currently configured provider id.
    let effective_provider: Option<String> = cfg
        .model_provider
        .clone()
        .or_else(|| cfg.model.as_ref().and_then(|m| m.normalized_provider()));

    // If no provider is configured explicitly, there is nothing stale to fix.
    let effective_provider = match effective_provider {
        Some(p) => p,
        None => return,
    };

    if !effective_provider.eq_ignore_ascii_case(removed_account_id) {
        return; // The removed provider is not the default — nothing to do.
    }

    // 2. Collect available providers and pick a replacement. Prefer store-based providers (they
    //    have auth) over bare built-ins.
    let store_files = list_provider_store_files(savfox_home);
    for file in &store_files {
        if !file.id.eq_ignore_ascii_case(removed_account_id)
            && (!file.models.is_empty() || file.auth.is_some())
        {
            apply_provider_fallback(savfox_home, &file.id);
            return;
        }
    }

    // Fallback to any built-in provider that isn't the removed one.
    let builtins = built_in_model_providers();
    for key in builtins.keys() {
        if !key.eq_ignore_ascii_case(removed_account_id) {
            apply_provider_fallback(savfox_home, key);
            return;
        }
    }

    // No providers left — clear config so next load picks whatever is
    // available at that point.
    let edits = vec![
        crate::config::edit::ConfigEdit::ClearPath {
            segments: vec!["model_provider".to_owned()],
        },
        crate::config::edit::ConfigEdit::ClearPath {
            segments: vec!["model".to_owned()],
        },
    ];
    let _ = crate::config::edit::ConfigEditsBuilder::new(savfox_home)
        .with_edits(edits)
        .apply_blocking();
}

/// Write a new default provider (and clear the model selection) to config.toml.
fn apply_provider_fallback(savfox_home: &Path, new_provider_id: &str) {
    use toml_edit::value;

    let edits = vec![
        crate::config::edit::ConfigEdit::SetPath {
            segments: vec!["model_provider".to_owned()],
            value: value(new_provider_id),
        },
        // Clear the model selection so the system can pick the default
        // model for the new provider on next load.
        crate::config::edit::ConfigEdit::ClearPath {
            segments: vec!["model".to_owned()],
        },
    ];
    let _ = crate::config::edit::ConfigEditsBuilder::new(savfox_home)
        .with_edits(edits)
        .apply_blocking();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_account_id_basic() {
        assert_eq!(
            slugify_account_id("openai", "Work Account"),
            "openai-work-account"
        );
    }

    #[test]
    fn slugify_account_id_same_as_provider() {
        assert_eq!(slugify_account_id("openai", "OpenAI"), "openai-openai");
    }

    #[test]
    fn slugify_account_id_empty_name() {
        assert_eq!(slugify_account_id("openai", ""), "openai");
        assert_eq!(slugify_account_id("openai", "  "), "openai");
    }

    #[test]
    fn slugify_account_id_special_chars() {
        assert_eq!(
            slugify_account_id("openai", "My  Work---Account!"),
            "openai-my-work-account"
        );
    }

    #[test]
    fn account_id_exists_check() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        assert!(!account_id_exists(savfox_home, "openai-work"));

        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("openai-work.json"), "{}").expect("write file");
        assert!(account_id_exists(savfox_home, "openai-work"));
    }

    #[test]
    fn list_provider_store_files_returns_all() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");

        std::fs::write(
            dir.join("openai.json"),
            r#"{"version":2,"provider_id":"openai","name":"OpenAI","disabled_models":[]}"#,
        )
        .expect("write");
        std::fs::write(
            dir.join("openai-work.json"),
            r#"{"version":2,"id":"openai-work","provider_id":"openai","name":"Work","disabled_models":[]}"#,
        )
        .expect("write");

        let files = list_provider_store_files(savfox_home);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn persist_provider_connection_migrates_auth_from_bare_provider_file() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");

        std::fs::write(
            dir.join("openai.json"),
            r#"{
  "version": 2,
  "id": "openai",
  "provider_id": "openai",
  "name": "OpenAI",
  "auth": {
    "type": "chatgpt_oauth",
    "env_key": "OPENAI_API_KEY",
    "api_key": "sk-test"
  },
  "disabled_models": []
}"#,
        )
        .expect("write source auth file");

        let models = vec![serde_json::json!({
            "id": "openai/gpt-5.2",
            "model_slug": "gpt-5.2",
            "name": "GPT-5.2",
            "is_default": true
        })];

        persist_provider_connection(
            savfox_home,
            "openai-work",
            "openai",
            "Work",
            &models,
            Some("OPENAI_API_KEY"),
            None,
        )
        .expect("persist provider");

        let migrated = load_provider_store_file(savfox_home, "openai-work");
        assert_eq!(migrated.id, "openai-work");
        assert_eq!(migrated.slug, "work");
        assert_eq!(migrated.name, "Work");
        assert_eq!(
            migrated
                .auth
                .as_ref()
                .and_then(|auth| auth.api_key.as_deref()),
            Some("sk-test")
        );
        assert!(
            !dir.join("openai.json").exists(),
            "bare provider auth file should be removed after migration"
        );
    }

    #[test]
    fn fallback_noop_when_removed_provider_is_not_default() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        std::fs::write(
            savfox_home.join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("write config");

        // Removing "anthropic" should not change config since default is "openai".
        fallback_default_provider_if_removed(savfox_home, "anthropic");

        let contents = std::fs::read_to_string(savfox_home.join("config.toml")).expect("read");
        assert!(
            contents.contains("model_provider = \"openai\""),
            "config should remain unchanged"
        );
    }

    #[test]
    fn fallback_prefers_store_provider_over_builtin() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        std::fs::write(
            savfox_home.join("config.toml"),
            "model_provider = \"openai-lucas\"\n",
        )
        .expect("write config");

        // Create another store-based provider so it can be picked.
        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("openai-work.json"),
            r#"{"version":2,"id":"openai-work","provider_id":"openai","name":"Work","auth":{"type":"api_key","api_key":"sk-test"},"disabled_models":[],"models":[]}"#,
        )
        .expect("write store");

        fallback_default_provider_if_removed(savfox_home, "openai-lucas");

        let contents = std::fs::read_to_string(savfox_home.join("config.toml")).expect("read");
        assert!(
            contents.contains("model_provider = \"openai-work\""),
            "should fall back to store-based provider, got: {contents}"
        );
    }

    #[test]
    fn fallback_uses_builtin_when_no_store_providers() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        std::fs::write(
            savfox_home.join("config.toml"),
            "model_provider = \"openai-lucas\"\n",
        )
        .expect("write config");

        // No store files — should fall back to a built-in provider.
        fallback_default_provider_if_removed(savfox_home, "openai-lucas");

        let contents = std::fs::read_to_string(savfox_home.join("config.toml")).expect("read");
        // Should pick some built-in; must not be the removed one.
        assert!(
            !contents.contains("openai-lucas"),
            "should not keep removed provider, got: {contents}"
        );
        assert!(
            contents.contains("model_provider"),
            "should set a new model_provider, got: {contents}"
        );
    }

    #[test]
    fn fallback_picks_store_provider_when_default_removed() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();
        std::fs::write(
            savfox_home.join("config.toml"),
            "model_provider = \"openai-lucas\"\n",
        )
        .expect("write config");

        // Create a remaining provider store file.
        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("anthropic-work.json"),
            r#"{"version":2,"id":"anthropic-work","provider_id":"anthropic","name":"Work","auth":{"type":"api_key","api_key":"sk-test"},"disabled_models":[],"models":[]}"#,
        )
        .expect("write store");

        fallback_default_provider_if_removed(savfox_home, "openai-lucas");

        let contents = std::fs::read_to_string(savfox_home.join("config.toml")).expect("read");
        assert!(
            contents.contains("model_provider = \"anthropic-work\""),
            "should fall back to remaining store provider, got: {contents}"
        );
    }

    #[test]
    fn fallback_clears_model_selection() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let savfox_home = tmp.path();

        // Create a store provider to fall back to.
        let dir = provider_models_store_dir(savfox_home);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("openai-work.json"),
            r#"{"version":2,"id":"openai-work","provider_id":"openai","name":"Work","auth":{"type":"api_key","api_key":"sk-test"},"disabled_models":[],"models":[]}"#,
        )
        .expect("write store");

        std::fs::write(
            savfox_home.join("config.toml"),
            concat!(
                "model_provider = \"openai-lucas\"\n",
                "\n",
                "[model]\n",
                "slug = \"gpt-4\"\n",
                "provider = \"openai-lucas\"\n",
            ),
        )
        .expect("write config");

        fallback_default_provider_if_removed(savfox_home, "openai-lucas");

        let contents = std::fs::read_to_string(savfox_home.join("config.toml")).expect("read");
        assert!(
            contents.contains("model_provider = \"openai-work\""),
            "should fall back to store provider, got: {contents}"
        );
        // The [model] table should be cleared since the old model belonged
        // to the removed provider.
        assert!(
            !contents.contains("slug = \"gpt-4\""),
            "model selection should be cleared, got: {contents}"
        );
    }
}
