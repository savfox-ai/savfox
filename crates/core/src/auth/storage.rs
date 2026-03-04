use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use savfox_app_server_protocol::AuthMode;
use savfox_keyring_store::{DefaultKeyringStore, KeyringStore};
use savfox_model::provider_default_models;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::config::provider_store::{
    PROVIDER_STORE_FILE_VERSION, ProviderStoreAuth, ProviderStoreFile, provider_store_path,
};
use crate::token_data::TokenData;

/// Determine where Savfox should store CLI auth credentials.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuthCredentialsStoreMode {
    #[default]
    /// Persist credentials in SAVFOX_HOME/models/openai.json.
    File,
    /// Persist credentials in the keyring. Fail if unavailable.
    Keyring,
    /// Use keyring when available; otherwise, fall back to a file in SAVFOX_HOME.
    Auto,
    /// Store credentials in memory only for the current process.
    Ephemeral,
}

/// Internal auth payload used by runtime auth flows.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,

    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,
}

const AUTH_PROVIDER_ID: &str = "openai";
const AUTH_PROVIDER_DISPLAY_NAME: &str = "OpenAI";
const LEGACY_CHATGPT_PROVIDER_ID: &str = "chatgpt";

impl From<&AuthDotJson> for ProviderStoreFile {
    fn from(auth: &AuthDotJson) -> Self {
        let auth_type = if auth.tokens.is_some() || auth.auth_mode == Some(AuthMode::Chatgpt) {
            "chatgpt_oauth".to_string()
        } else {
            "api_key".to_string()
        };
        Self {
            version: PROVIDER_STORE_FILE_VERSION,
            provider_id: AUTH_PROVIDER_ID.to_string(),
            name: AUTH_PROVIDER_DISPLAY_NAME.to_string(),
            auth: Some(ProviderStoreAuth {
                auth_type,
                env_key: Some("OPENAI_API_KEY".to_string()),
                api_key: auth.openai_api_key.clone(),
                auth_mode: auth.auth_mode,
                tokens: auth.tokens.clone(),
                last_refresh: auth.last_refresh,
            }),
            enabled_models: default_openai_enabled_models(),
            models: Vec::new(),
        }
    }
}

fn default_openai_enabled_models() -> Vec<String> {
    provider_default_models(AUTH_PROVIDER_ID)
        .iter()
        .map(|model| model.slug.clone())
        .collect()
}

impl ProviderStoreFile {
    fn into_auth_dot_json(self) -> Option<AuthDotJson> {
        let auth = self.auth?;
        if auth.api_key.is_none() && auth.tokens.is_none() && auth.auth_mode.is_none() {
            return None;
        }

        Some(AuthDotJson {
            auth_mode: auth.auth_mode,
            openai_api_key: auth.api_key,
            tokens: auth.tokens,
            last_refresh: auth.last_refresh,
        })
    }
}

pub(super) fn get_auth_file(savfox_home: &Path) -> PathBuf {
    provider_store_path(savfox_home, AUTH_PROVIDER_ID)
}

fn get_legacy_chatgpt_provider_auth_file(savfox_home: &Path) -> PathBuf {
    provider_store_path(savfox_home, LEGACY_CHATGPT_PROVIDER_ID)
}

fn get_legacy_auth_file(savfox_home: &Path) -> PathBuf {
    savfox_home.join("auth.json")
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub(super) fn delete_file_if_exists(savfox_home: &Path) -> std::io::Result<bool> {
    let current_removed = remove_file_if_exists(get_auth_file(savfox_home).as_path())?;
    let legacy_chatgpt_removed =
        remove_file_if_exists(get_legacy_chatgpt_provider_auth_file(savfox_home).as_path())?;
    let legacy_removed = remove_file_if_exists(get_legacy_auth_file(savfox_home).as_path())?;
    Ok(current_removed || legacy_chatgpt_removed || legacy_removed)
}

pub(super) trait AuthStorageBackend: Debug + Send + Sync {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>>;
    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()>;
    fn delete(&self) -> std::io::Result<bool>;
}

#[derive(Clone, Debug)]
pub(super) struct FileAuthStorage {
    savfox_home: PathBuf,
}

impl FileAuthStorage {
    pub(super) fn new(savfox_home: PathBuf) -> Self {
        Self { savfox_home }
    }

    /// Attempt to read and parse a persisted auth file in the given `SAVFOX_HOME` directory.
    pub(super) fn try_read_auth_json(&self, auth_file: &Path) -> std::io::Result<AuthDotJson> {
        let mut file = File::open(auth_file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        if let Ok(provider_file) = serde_json::from_str::<ProviderStoreFile>(&contents) {
            if let Some(auth) = provider_file.into_auth_dot_json() {
                return Ok(auth);
            }
        }
        serde_json::from_str(&contents).map_err(Into::into)
    }
}

impl AuthStorageBackend for FileAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let auth_dot_json =
            match self.try_read_auth_json(get_auth_file(&self.savfox_home).as_path()) {
                Ok(auth) => auth,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    let legacy_chatgpt_auth_file =
                        get_legacy_chatgpt_provider_auth_file(&self.savfox_home);
                    match self.try_read_auth_json(&legacy_chatgpt_auth_file) {
                        Ok(auth) => auth,
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            let legacy_auth_file = get_legacy_auth_file(&self.savfox_home);
                            match self.try_read_auth_json(&legacy_auth_file) {
                                Ok(auth) => auth,
                                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                    return Ok(None);
                                }
                                Err(err) => return Err(err),
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(err) => return Err(err),
            };
        Ok(Some(auth_dot_json))
    }

    fn save(&self, auth_dot_json: &AuthDotJson) -> std::io::Result<()> {
        let auth_file = get_auth_file(&self.savfox_home);

        if let Some(parent) = auth_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let provider_file = ProviderStoreFile::from(auth_dot_json);
        let json_data = serde_json::to_string_pretty(&provider_file)?;
        let mut options = OpenOptions::new();
        options.truncate(true).write(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(auth_file)?;
        file.write_all(json_data.as_bytes())?;
        file.flush()?;
        if let Err(err) = remove_file_if_exists(
            get_legacy_chatgpt_provider_auth_file(&self.savfox_home).as_path(),
        ) {
            warn!("failed to remove legacy chatgpt provider auth file: {err}");
        }
        if let Err(err) = remove_file_if_exists(get_legacy_auth_file(&self.savfox_home).as_path()) {
            warn!("failed to remove legacy auth.json: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        delete_file_if_exists(&self.savfox_home)
    }
}

const KEYRING_SERVICE: &str = "Savfox Auth";

// turns savfox_home path into a stable, short key string
fn compute_store_key(savfox_home: &Path) -> std::io::Result<String> {
    let canonical = savfox_home
        .canonicalize()
        .unwrap_or_else(|_| savfox_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("cli|{truncated}"))
}

#[derive(Clone, Debug)]
struct KeyringAuthStorage {
    savfox_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
}

impl KeyringAuthStorage {
    fn new(savfox_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            savfox_home,
            keyring_store,
        }
    }

    fn load_from_keyring(&self, key: &str) -> std::io::Result<Option<AuthDotJson>> {
        match self.keyring_store.load(KEYRING_SERVICE, key) {
            Ok(Some(serialized)) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                std::io::Error::other(format!(
                    "failed to deserialize CLI auth from keyring: {err}"
                ))
            }),
            Ok(None) => Ok(None),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to load CLI auth from keyring: {}",
                error.message()
            ))),
        }
    }

    fn save_to_keyring(&self, key: &str, value: &str) -> std::io::Result<()> {
        match self.keyring_store.save(KEYRING_SERVICE, key, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = format!(
                    "failed to write OAuth tokens to keyring: {}",
                    error.message()
                );
                warn!("{message}");
                Err(std::io::Error::other(message))
            }
        }
    }
}

impl AuthStorageBackend for KeyringAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let key = compute_store_key(&self.savfox_home)?;
        self.load_from_keyring(&key)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let key = compute_store_key(&self.savfox_home)?;
        // Simpler error mapping per style: prefer method reference over closure
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.save_to_keyring(&key, &serialized)?;
        if let Err(err) = delete_file_if_exists(&self.savfox_home) {
            warn!("failed to remove CLI auth fallback file: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let key = compute_store_key(&self.savfox_home)?;
        let keyring_removed = self
            .keyring_store
            .delete(KEYRING_SERVICE, &key)
            .map_err(|err| {
                std::io::Error::other(format!("failed to delete auth from keyring: {err}"))
            })?;
        let file_removed = delete_file_if_exists(&self.savfox_home)?;
        Ok(keyring_removed || file_removed)
    }
}

#[derive(Clone, Debug)]
struct AutoAuthStorage {
    keyring_storage: Arc<KeyringAuthStorage>,
    file_storage: Arc<FileAuthStorage>,
}

impl AutoAuthStorage {
    fn new(savfox_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            keyring_storage: Arc::new(KeyringAuthStorage::new(savfox_home.clone(), keyring_store)),
            file_storage: Arc::new(FileAuthStorage::new(savfox_home)),
        }
    }
}

impl AuthStorageBackend for AutoAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        match self.keyring_storage.load() {
            Ok(Some(auth)) => Ok(Some(auth)),
            Ok(None) => self.file_storage.load(),
            Err(err) => {
                warn!("failed to load CLI auth from keyring, falling back to file storage: {err}");
                self.file_storage.load()
            }
        }
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        match self.keyring_storage.save(auth) {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!("failed to save auth to keyring, falling back to file storage: {err}");
                self.file_storage.save(auth)
            }
        }
    }

    fn delete(&self) -> std::io::Result<bool> {
        // Keyring storage will delete from disk as well
        self.keyring_storage.delete()
    }
}

// A global in-memory store for mapping savfox_home -> AuthDotJson.
static EPHEMERAL_AUTH_STORE: Lazy<Mutex<HashMap<String, AuthDotJson>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct EphemeralAuthStorage {
    savfox_home: PathBuf,
}

impl EphemeralAuthStorage {
    fn new(savfox_home: PathBuf) -> Self {
        Self { savfox_home }
    }

    fn with_store<F, T>(&self, action: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut HashMap<String, AuthDotJson>, String) -> std::io::Result<T>,
    {
        let key = compute_store_key(&self.savfox_home)?;
        let mut store = EPHEMERAL_AUTH_STORE
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock ephemeral auth storage"))?;
        action(&mut store, key)
    }
}

impl AuthStorageBackend for EphemeralAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.with_store(|store, key| Ok(store.get(&key).cloned()))
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.with_store(|store, key| {
            store.insert(key, auth.clone());
            Ok(())
        })
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.with_store(|store, key| Ok(store.remove(&key).is_some()))
    }
}

pub(super) fn create_auth_storage(
    savfox_home: PathBuf,
    mode: AuthCredentialsStoreMode,
) -> Arc<dyn AuthStorageBackend> {
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
    create_auth_storage_with_keyring_store(savfox_home, mode, keyring_store)
}

fn create_auth_storage_with_keyring_store(
    savfox_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> Arc<dyn AuthStorageBackend> {
    match mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAuthStorage::new(savfox_home)),
        AuthCredentialsStoreMode::Keyring => {
            Arc::new(KeyringAuthStorage::new(savfox_home, keyring_store))
        }
        AuthCredentialsStoreMode::Auto => {
            Arc::new(AutoAuthStorage::new(savfox_home, keyring_store))
        }
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAuthStorage::new(savfox_home)),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;
    use base64::Engine;
    use keyring::Error as KeyringError;
    use pretty_assertions::assert_eq;
    use savfox_keyring_store::tests::MockKeyringStore;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::token_data::IdTokenInfo;

    #[tokio::test]
    async fn file_storage_load_returns_auth_dot_json() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let storage = FileAuthStorage::new(savfox_home.path().to_path_buf());
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("test-key".to_string()),
            tokens: None,
            last_refresh: Some(Utc::now()),
        };

        storage
            .save(&auth_dot_json)
            .context("failed to save auth file")?;

        let loaded = storage.load().context("failed to load auth file")?;
        assert_eq!(Some(auth_dot_json), loaded);
        Ok(())
    }

    #[tokio::test]
    async fn file_storage_load_accepts_legacy_chatgpt_provider_file() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let storage = FileAuthStorage::new(savfox_home.path().to_path_buf());
        let auth_file = get_legacy_chatgpt_provider_auth_file(savfox_home.path());
        if let Some(parent) = auth_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let provider_file = json!({
            "version": 2,
            "provider_id": "chatgpt",
            "name": "ChatGPT",
            "auth": {
                "type": "api_key",
                "env_key": "OPENAI_API_KEY",
                "api_key": "legacy-key"
            },
            "models": [
                {
                    "id": "openai/gpt-5.2-codex",
                    "name": "gpt-5.2-codex",
                    "provider": "openai",
                    "model_slug": "gpt-5.2-codex",
                    "is_default": true,
                    "builtin": true
                }
            ]
        });
        std::fs::write(&auth_file, serde_json::to_string_pretty(&provider_file)?)?;

        let loaded = storage.load().context("failed to load provider file")?;
        assert_eq!(
            loaded
                .as_ref()
                .and_then(|auth| auth.openai_api_key.as_deref()),
            Some("legacy-key")
        );
        Ok(())
    }

    #[tokio::test]
    async fn file_storage_save_persists_auth_dot_json() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let storage = FileAuthStorage::new(savfox_home.path().to_path_buf());
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("test-key".to_string()),
            tokens: None,
            last_refresh: Some(Utc::now()),
        };

        let file = get_auth_file(savfox_home.path());
        storage
            .save(&auth_dot_json)
            .context("failed to save auth file")?;

        let same_auth_dot_json = storage
            .try_read_auth_json(&file)
            .context("failed to read auth file after save")?;
        assert_eq!(auth_dot_json, same_auth_dot_json);

        let raw_file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
        assert_eq!(raw_file["version"], 2);
        assert_eq!(raw_file["provider_id"], "openai");
        assert_eq!(raw_file["name"], "OpenAI");
        assert_eq!(raw_file["auth"]["type"], "api_key");
        assert_eq!(raw_file["auth"]["env_key"], "OPENAI_API_KEY");
        assert_eq!(raw_file["auth"]["api_key"], "test-key");
        let enabled_models = raw_file["enabled_models"]
            .as_array()
            .expect("provider store file should include an enabled_models array");
        assert!(
            !enabled_models.is_empty(),
            "provider store file should include default OpenAI enabled models"
        );
        assert_eq!(enabled_models[0], json!("gpt-5.3-codex"));
        assert!(
            enabled_models
                .iter()
                .any(|model_slug| model_slug == &json!("gpt-5.3-codex")),
            "provider store file should include gpt-5.3-codex in default OpenAI enabled models"
        );
        Ok(())
    }

    #[tokio::test]
    async fn file_storage_save_removes_legacy_chatgpt_provider_file() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let storage = FileAuthStorage::new(savfox_home.path().to_path_buf());
        let legacy_file = get_legacy_chatgpt_provider_auth_file(savfox_home.path());
        if let Some(parent) = legacy_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&legacy_file, r#"{"provider_id":"chatgpt"}"#)?;
        assert!(
            legacy_file.exists(),
            "legacy chatgpt provider file should exist"
        );

        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("test-key".to_string()),
            tokens: None,
            last_refresh: Some(Utc::now()),
        };
        storage
            .save(&auth_dot_json)
            .context("failed to save auth file")?;

        assert!(
            !legacy_file.exists(),
            "saving auth should remove legacy chatgpt provider file"
        );
        Ok(())
    }

    #[test]
    fn file_storage_delete_removes_auth_file() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test-key".to_string()),
            tokens: None,
            last_refresh: None,
        };
        let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
        storage.save(&auth_dot_json)?;
        assert!(get_auth_file(dir.path()).exists());
        let storage = FileAuthStorage::new(dir.path().to_path_buf());
        let removed = storage.delete()?;
        assert!(removed);
        assert!(!get_auth_file(dir.path()).exists());
        Ok(())
    }

    #[test]
    fn ephemeral_storage_save_load_delete_is_in_memory_only() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let storage = create_auth_storage(
            dir.path().to_path_buf(),
            AuthCredentialsStoreMode::Ephemeral,
        );
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-ephemeral".to_string()),
            tokens: None,
            last_refresh: Some(Utc::now()),
        };

        storage.save(&auth_dot_json)?;
        let loaded = storage.load()?;
        assert_eq!(Some(auth_dot_json), loaded);

        let removed = storage.delete()?;
        assert!(removed);
        let loaded = storage.load()?;
        assert_eq!(None, loaded);
        assert!(!get_auth_file(dir.path()).exists());
        Ok(())
    }

    fn seed_keyring_and_fallback_auth_file_for_delete<F>(
        mock_keyring: &MockKeyringStore,
        savfox_home: &Path,
        compute_key: F,
    ) -> anyhow::Result<(String, PathBuf)>
    where
        F: FnOnce() -> std::io::Result<String>,
    {
        let key = compute_key()?;
        mock_keyring.save(KEYRING_SERVICE, &key, "{}")?;
        let auth_file = get_auth_file(savfox_home);
        if let Some(parent) = auth_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&auth_file, "stale")?;
        Ok((key, auth_file))
    }

    fn seed_keyring_with_auth<F>(
        mock_keyring: &MockKeyringStore,
        compute_key: F,
        auth: &AuthDotJson,
    ) -> anyhow::Result<()>
    where
        F: FnOnce() -> std::io::Result<String>,
    {
        let key = compute_key()?;
        let serialized = serde_json::to_string(auth)?;
        mock_keyring.save(KEYRING_SERVICE, &key, &serialized)?;
        Ok(())
    }

    fn assert_keyring_saved_auth_and_removed_fallback(
        mock_keyring: &MockKeyringStore,
        key: &str,
        savfox_home: &Path,
        expected: &AuthDotJson,
    ) {
        let saved_value = mock_keyring
            .saved_value(key)
            .expect("keyring entry should exist");
        let expected_serialized = serde_json::to_string(expected).expect("serialize expected auth");
        assert_eq!(saved_value, expected_serialized);
        let auth_file = get_auth_file(savfox_home);
        assert!(
            !auth_file.exists(),
            "fallback auth file should be removed after keyring save"
        );
    }

    fn id_token_with_prefix(prefix: &str) -> IdTokenInfo {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = json!({
            "email": format!("{prefix}@example.com"),
            "https://api.openai.com/auth": {
                "chatgpt_account_id": format!("{prefix}-account"),
            },
        });
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize header"));
        let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize payload"));
        let signature_b64 = encode(b"sig");
        let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

        crate::token_data::parse_id_token(&fake_jwt).expect("fake JWT should parse")
    }

    fn auth_with_prefix(prefix: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some(format!("{prefix}-api-key")),
            tokens: Some(TokenData {
                id_token: id_token_with_prefix(prefix),
                access_token: format!("{prefix}-access"),
                refresh_token: format!("{prefix}-refresh"),
                account_id: Some(format!("{prefix}-account-id")),
            }),
            last_refresh: None,
        }
    }

    #[test]
    fn keyring_auth_storage_load_returns_deserialized_auth() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let expected = AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test".to_string()),
            tokens: None,
            last_refresh: None,
        };
        seed_keyring_with_auth(
            &mock_keyring,
            || compute_store_key(savfox_home.path()),
            &expected,
        )?;

        let loaded = storage.load()?;
        assert_eq!(Some(expected), loaded);
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_compute_store_key_for_home_directory() -> anyhow::Result<()> {
        let savfox_home = PathBuf::from("~/.savfox");

        let key = compute_store_key(savfox_home.as_path())?;

        assert!(key.starts_with("cli|"));
        assert_eq!(key.len(), "cli|940db7b1d0e4eb40".len());
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_save_persists_and_removes_fallback_file() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let auth_file = get_auth_file(savfox_home.path());
        if let Some(parent) = auth_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&auth_file, "stale")?;
        let auth = AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: Default::default(),
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some("account".to_string()),
            }),
            last_refresh: Some(Utc::now()),
        };

        storage.save(&auth)?;

        let key = compute_store_key(savfox_home.path())?;
        assert_keyring_saved_auth_and_removed_fallback(
            &mock_keyring,
            &key,
            savfox_home.path(),
            &auth,
        );
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let (key, auth_file) = seed_keyring_and_fallback_auth_file_for_delete(
            &mock_keyring,
            savfox_home.path(),
            || compute_store_key(savfox_home.path()),
        )?;

        let removed = storage.delete()?;

        assert!(removed, "delete should report removal");
        assert!(
            !mock_keyring.contains(&key),
            "keyring entry should be removed"
        );
        assert!(
            !auth_file.exists(),
            "fallback auth file should be removed after keyring delete"
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_prefers_keyring_value() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let keyring_auth = auth_with_prefix("keyring");
        seed_keyring_with_auth(
            &mock_keyring,
            || compute_store_key(savfox_home.path()),
            &keyring_auth,
        )?;

        let file_auth = auth_with_prefix("file");
        storage.file_storage.save(&file_auth)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(keyring_auth));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_uses_file_when_keyring_empty() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage =
            AutoAuthStorage::new(savfox_home.path().to_path_buf(), Arc::new(mock_keyring));

        let expected = auth_with_prefix("file-only");
        storage.file_storage.save(&expected)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(expected));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_falls_back_when_keyring_errors() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(savfox_home.path())?;
        mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "load".into()));

        let expected = auth_with_prefix("fallback");
        storage.file_storage.save(&expected)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(expected));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_save_prefers_keyring() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(savfox_home.path())?;

        let stale = auth_with_prefix("stale");
        storage.file_storage.save(&stale)?;

        let expected = auth_with_prefix("to-save");
        storage.save(&expected)?;

        assert_keyring_saved_auth_and_removed_fallback(
            &mock_keyring,
            &key,
            savfox_home.path(),
            &expected,
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_save_falls_back_when_keyring_errors() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(savfox_home.path())?;
        mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "save".into()));

        let auth = auth_with_prefix("fallback");
        storage.save(&auth)?;

        let auth_file = get_auth_file(savfox_home.path());
        assert!(
            auth_file.exists(),
            "fallback auth file should be created when keyring save fails"
        );
        let saved = storage
            .file_storage
            .load()?
            .context("fallback auth should exist")?;
        assert_eq!(saved, auth);
        assert!(
            mock_keyring.saved_value(&key).is_none(),
            "keyring should not contain value when save fails"
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
        let savfox_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            savfox_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let (key, auth_file) = seed_keyring_and_fallback_auth_file_for_delete(
            &mock_keyring,
            savfox_home.path(),
            || compute_store_key(savfox_home.path()),
        )?;

        let removed = storage.delete()?;

        assert!(removed, "delete should report removal");
        assert!(
            !mock_keyring.contains(&key),
            "keyring entry should be removed"
        );
        assert!(
            !auth_file.exists(),
            "fallback auth file should be removed after delete"
        );
        Ok(())
    }
}
