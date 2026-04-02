use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use savfox_api_client::{ModelsClient, ReqwestTransport};
use savfox_protocol::config_types::CollaborationModeMask;
use savfox_protocol::openai_models::{ModelInfo, ModelPreset};
use tokio::sync::{RwLock, TryLockError};
use tokio::time::timeout;
use tracing::{debug, error};

use super::cache::ModelsCacheManager;
use crate::api_bridge::{auth_provider_from_auth, map_api_error};
use crate::auth::{AuthManager, AuthMode};
use crate::config::Config;
use crate::default_client::build_reqwest_client;
use crate::error::{Result as CoreResult, SavfoxError};
use crate::features::Feature;
use crate::model_provider_info::ModelProviderInfo;
use crate::models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use crate::models_manager::model_info;
use crate::models_manager::model_presets::builtin_model_presets;

const MODEL_CACHE_FILE: &str = "models_cache.json";
const DEFAULT_MODEL_CACHE_TTL: Duration = Duration::from_secs(300);
const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

/// Strategy for refreshing available models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStrategy {
    /// Always fetch from the network, ignoring cache.
    Online,
    /// Only use cached data, never fetch from the network.
    Offline,
    /// Use cache if available and fresh, otherwise fetch from the network.
    OnlineIfUncached,
}

/// Coordinates remote model discovery plus cached metadata on disk.
#[derive(Debug)]
pub struct ModelsManager {
    savfox_home: PathBuf,
    local_models: Vec<ModelPreset>,
    remote_models: RwLock<Vec<ModelInfo>>,
    auth_manager: Arc<AuthManager>,
    etag: RwLock<Option<String>>,
    cache_manager: ModelsCacheManager,
    provider: ModelProviderInfo,
}

impl ModelsManager {
    /// Construct a manager scoped to the provided `AuthManager`.
    ///
    /// Uses `savfox_home` to store cached model metadata and initializes with built-in presets.
    pub fn new(savfox_home: PathBuf, auth_manager: Arc<AuthManager>) -> Self {
        let cache_path = savfox_home.join(MODEL_CACHE_FILE);
        let cache_manager = ModelsCacheManager::new(cache_path, DEFAULT_MODEL_CACHE_TTL);
        Self {
            savfox_home,
            local_models: builtin_model_presets(auth_manager.get_internal_auth_mode()),
            remote_models: RwLock::new(Vec::new()),
            auth_manager,
            etag: RwLock::new(None),
            cache_manager,
            provider: ModelProviderInfo::create_openai_provider(),
        }
    }

    /// List all available models, refreshing according to the specified strategy.
    ///
    /// Returns model presets sorted by priority and filtered by auth mode and visibility.
    pub async fn list_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> Vec<ModelPreset> {
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        self.build_available_models(remote_models)
    }

    /// List collaboration mode presets.
    ///
    /// Returns a static set of presets seeded with the configured model.
    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    /// Attempt to list models without blocking, using the current cached state.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    pub fn try_list_models(&self, config: &Config) -> Result<Vec<ModelPreset>, TryLockError> {
        let remote_models = self.try_get_remote_models(config)?;
        Ok(self.build_available_models(remote_models))
    }

    // todo(aibrahim): should be visible to core only and sent on session_configured event
    /// Get the model identifier to use, refreshing according to the specified strategy.
    ///
    /// If `model` is provided, returns it directly. Otherwise selects the default based on
    /// auth mode and available models.
    pub async fn get_default_model(
        &self,
        model: &Option<String>,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> String {
        if let Some(model) = model.as_ref() {
            return model.clone();
        }
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        let available = self.build_available_models(remote_models);
        available
            .iter()
            .find(|model| model.is_default)
            .or_else(|| available.first())
            .map(|model| model.slug.clone())
            .unwrap_or_default()
    }

    // todo(aibrahim): look if we can tighten it to pub(crate)
    /// Look up model metadata, applying remote overrides and config adjustments.
    pub async fn get_model_info(&self, model: &str, config: &Config) -> ModelInfo {
        let remote = self
            .get_remote_models(config)
            .await
            .into_iter()
            .find(|m| m.slug == model);
        let model = if let Some(remote) = remote {
            remote
        } else {
            model_info::find_model_info_for_slug(model)
        };
        model_info::with_config_overrides(model, config)
    }

    /// Refresh models if the provided ETag differs from the cached ETag.
    ///
    /// Uses `Online` strategy to fetch latest models when ETags differ.
    pub(crate) async fn refresh_if_new_etag(&self, etag: String, config: &Config) {
        let current_etag = self.get_etag().await;
        if current_etag.clone().is_some() && current_etag.as_deref() == Some(etag.as_str()) {
            if let Err(err) = self.cache_manager.renew_cache_ttl().await {
                error!("failed to renew cache TTL: {err}");
            }
            return;
        }
        if let Err(err) = self
            .refresh_available_models(config, RefreshStrategy::Online)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
    }

    /// Refresh available models according to the specified strategy.
    async fn refresh_available_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> CoreResult<()> {
        if !config.features.enabled(Feature::RemoteModels)
            || self.auth_manager.get_internal_auth_mode() == Some(AuthMode::ApiKey)
        {
            return Ok(());
        }

        match refresh_strategy {
            RefreshStrategy::Offline => {
                // Try cache first, then provider store files (no network).
                if !self.try_load_cache().await {
                    self.try_load_from_provider_store().await;
                }
                Ok(())
            }
            RefreshStrategy::OnlineIfUncached => {
                // Try cache first, then fresh provider store, then online.
                if self.try_load_cache().await {
                    return Ok(());
                }
                if self.try_load_fresh_provider_store().await {
                    return Ok(());
                }
                if let Err(err) = self.fetch_and_update_models(config).await {
                    debug!("remote fetch failed, falling back to provider store: {err}");
                    self.try_load_from_provider_store().await;
                }
                Ok(())
            }
            RefreshStrategy::Online => {
                // Always fetch from network, fall back to provider store on failure.
                if let Err(err) = self.fetch_and_update_models(config).await {
                    debug!("remote fetch failed, falling back to provider store: {err}");
                    self.try_load_from_provider_store().await;
                }
                Ok(())
            }
        }
    }

    async fn fetch_and_update_models(&self, config: &Config) -> CoreResult<()> {
        let _timer =
            savfox_otel::start_global_timer("savfox.remote_models.fetch_update.duration_ms", &[]);
        let auth = self.auth_manager.auth().await;
        let auth_mode = self.auth_manager.get_internal_auth_mode();
        let api_provider = self.provider.to_api_provider(
            Some("openai"),
            auth_mode,
            Some(config.chatgpt_base_url.as_str()),
        )?;
        let api_auth = auth_provider_from_auth(auth.clone(), &self.provider, "openai")?;
        let provider_id = api_provider.id.clone();
        let models_url = api_provider.url_for_path("models");
        let has_bearer_token = api_auth.has_bearer_token();
        let has_account_id = api_auth.has_account_id();
        if self.provider.requires_openai_auth && !has_bearer_token && !has_account_id {
            debug!(
                provider = %provider_id,
                url = %models_url,
                auth_mode = ?auth_mode,
                has_bearer_token,
                has_account_id,
                "skipping remote model refresh because provider requires auth but no credentials are available"
            );
            return Ok(());
        }
        let transport = ReqwestTransport::new(build_reqwest_client());
        let client = ModelsClient::new(transport, api_provider, api_auth);

        let client_version = crate::models_manager::client_version_to_whole();
        debug!(
            provider = %provider_id,
            url = %models_url,
            auth_mode = ?auth_mode,
            has_bearer_token,
            has_account_id,
            client_version,
            "refreshing remote models"
        );

        let list_result = timeout(
            MODELS_REFRESH_TIMEOUT,
            client.list_models(&client_version, HeaderMap::new()),
        )
        .await
        .map_err(|_| SavfoxError::Timeout)?;
        let (models, etag) = match list_result {
            Ok(data) => data,
            Err(err) => {
                let mapped = map_api_error(err);
                error!(
                    provider = %provider_id,
                    url = %models_url,
                    auth_mode = ?auth_mode,
                    has_bearer_token,
                    has_account_id,
                    client_version,
                    error = %mapped,
                    "remote model refresh request failed"
                );
                return Err(mapped);
            }
        };

        self.apply_remote_models(models.clone()).await;
        *self.etag.write().await = etag.clone();
        self.cache_manager
            .persist_cache(&models, etag, client_version)
            .await;

        // Persist fetched models into provider store files so they serve as a
        // long-term cache (survives cache invalidation / version bumps).
        let model_values: Vec<serde_json::Value> = models
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        if let Err(err) = crate::config::provider_store::update_provider_store_models(
            &self.savfox_home,
            &provider_id,
            &model_values,
        ) {
            error!("failed to update provider store with fetched models: {err}");
        }
        Ok(())
    }

    async fn get_etag(&self) -> Option<String> {
        self.etag.read().await.clone()
    }

    /// Replace the cached remote models.
    async fn apply_remote_models(&self, models: Vec<ModelInfo>) {
        *self.remote_models.write().await = models;
    }

    /// Load models from provider store files (`~/.savfox/models/*.json`).
    ///
    /// When `require_fresh` is `true`, only returns models from store files
    /// whose `models_fetched_at` is within `DEFAULT_MODEL_CACHE_TTL`.
    fn load_models_from_provider_store(
        savfox_home: &std::path::Path,
        require_fresh: bool,
    ) -> Vec<ModelInfo> {
        use crate::config::provider_store::list_provider_store_files;

        let files = list_provider_store_files(savfox_home);
        let mut all_models = Vec::new();

        for file in files {
            if require_fresh {
                let fetched_at = match file.models_fetched_at {
                    Some(ts) => ts,
                    None => continue,
                };
                let age = chrono::Utc::now().signed_duration_since(fetched_at);
                let ttl = chrono::Duration::from_std(DEFAULT_MODEL_CACHE_TTL).unwrap_or_default();
                if age > ttl {
                    continue;
                }
            }
            for model_value in &file.models {
                if let Ok(model_info) = serde_json::from_value::<ModelInfo>(model_value.clone()) {
                    all_models.push(model_info);
                }
            }
        }
        all_models
    }

    /// Attempt to satisfy the refresh from the cache when it matches the provider and TTL.
    async fn try_load_cache(&self) -> bool {
        let _timer =
            savfox_otel::start_global_timer("savfox.remote_models.load_cache.duration_ms", &[]);
        let client_version = crate::models_manager::client_version_to_whole();
        let cache = match self.cache_manager.load_fresh(&client_version).await {
            Some(cache) => cache,
            None => return false,
        };
        let models = cache.models.clone();
        *self.etag.write().await = cache.etag.clone();
        self.apply_remote_models(models).await;
        true
    }

    /// Try loading fresh models (within TTL) from provider store files.
    async fn try_load_fresh_provider_store(&self) -> bool {
        let models = Self::load_models_from_provider_store(&self.savfox_home, true);
        if models.is_empty() {
            return false;
        }
        debug!(
            count = models.len(),
            "loaded fresh models from provider store files"
        );
        self.apply_remote_models(models).await;
        true
    }

    /// Fall back to any models in provider store files (ignoring freshness).
    async fn try_load_from_provider_store(&self) {
        let models = Self::load_models_from_provider_store(&self.savfox_home, false);
        if !models.is_empty() {
            debug!(
                count = models.len(),
                "loaded models from provider store files (stale fallback)"
            );
            self.apply_remote_models(models).await;
        }
    }

    /// Merge remote model metadata into picker-ready presets, preserving existing entries.
    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by(|a, b| a.priority.cmp(&b.priority));

        let remote_presets: Vec<ModelPreset> = remote_models.into_iter().map(Into::into).collect();
        let existing_presets = self.local_models.clone();
        let mut merged_presets = ModelPreset::merge(remote_presets, existing_presets);
        let chatgpt_mode = matches!(
            self.auth_manager.get_internal_auth_mode(),
            Some(AuthMode::Chatgpt)
        );
        merged_presets = ModelPreset::filter_by_auth(merged_presets, chatgpt_mode);

        for preset in &mut merged_presets {
            preset.is_default = false;
        }
        if let Some(default) = merged_presets
            .iter_mut()
            .find(|preset| preset.show_in_picker)
        {
            default.is_default = true;
        } else if let Some(default) = merged_presets.first_mut() {
            default.is_default = true;
        }

        merged_presets
    }

    async fn get_remote_models(&self, config: &Config) -> Vec<ModelInfo> {
        if config.features.enabled(Feature::RemoteModels) {
            self.remote_models.read().await.clone()
        } else {
            Vec::new()
        }
    }

    fn try_get_remote_models(&self, config: &Config) -> Result<Vec<ModelInfo>, TryLockError> {
        if config.features.enabled(Feature::RemoteModels) {
            Ok(self.remote_models.try_read()?.clone())
        } else {
            Ok(Vec::new())
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct a manager with a specific provider for testing.
    pub fn with_provider(
        savfox_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        provider: ModelProviderInfo,
    ) -> Self {
        let cache_path = savfox_home.join(MODEL_CACHE_FILE);
        let cache_manager = ModelsCacheManager::new(cache_path, DEFAULT_MODEL_CACHE_TTL);
        Self {
            savfox_home,
            local_models: builtin_model_presets(auth_manager.get_internal_auth_mode()),
            remote_models: RwLock::new(Vec::new()),
            auth_manager,
            etag: RwLock::new(None),
            cache_manager,
            provider,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Get model identifier without consulting remote state or cache.
    #[must_use] 
    pub fn get_model_offline(model: Option<&str>) -> String {
        if let Some(model) = model {
            return model.to_owned();
        }
        let presets = builtin_model_presets(None);
        presets
            .iter()
            .find(|preset| preset.show_in_picker)
            .or_else(|| presets.first())
            .map(|preset| preset.slug.clone())
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Build `ModelInfo` without consulting remote state or cache.
    #[must_use] 
    pub fn construct_model_info_offline(model: &str, config: &Config) -> ModelInfo {
        model_info::with_config_overrides(model_info::find_model_info_for_slug(model), config)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use core_test_support::responses::mount_models_once;
    use pretty_assertions::assert_eq;
    use savfox_protocol::openai_models::ModelsResponse;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::MockServer;

    use super::*;
    use crate::SavfoxAuth;
    use crate::auth::AuthCredentialsStoreMode;
    use crate::config::ConfigBuilder;
    use crate::features::Feature;
    use crate::model_provider_info::WireApi;

    fn remote_model(slug: &str, display: &str, priority: i32) -> ModelInfo {
        remote_model_with_visibility(slug, display, priority, "list")
    }

    fn remote_model_with_visibility(
        slug: &str,
        display: &str,
        priority: i32,
        visibility: &str,
    ) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "name": display,
            "description": format!("{display} desc"),
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}],
            "shell_type": "shell_command",
            "visibility": visibility,
            "minimal_client_version": [0, 1, 0],
            "supported_in_api": true,
            "priority": priority,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    fn assert_models_contain(actual: &[ModelInfo], expected: &[ModelInfo]) {
        for model in expected {
            assert!(
                actual.iter().any(|candidate| candidate.slug == model.slug),
                "expected model {} in cached list",
                model.slug
            );
        }
    }

    fn provider_for(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            id: "mock".into(),
            name: "mock".into(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    #[tokio::test]
    async fn refresh_available_models_sorts_by_priority() {
        let server = MockServer::start().await;
        let remote_models = vec![
            remote_model("priority-low", "Low", 1),
            remote_model("priority-high", "High", 0),
        ];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("refresh succeeds");
        let cached_remote = manager.get_remote_models(&config).await;
        assert_models_contain(&cached_remote, &remote_models);

        let available = manager
            .list_models(&config, RefreshStrategy::OnlineIfUncached)
            .await;
        let high_idx = available
            .iter()
            .position(|model| model.slug == "priority-high")
            .expect("priority-high should be listed");
        let low_idx = available
            .iter()
            .position(|model| model.slug == "priority-low")
            .expect("priority-low should be listed");
        assert!(
            high_idx < low_idx,
            "higher priority should be listed before lower priority"
        );
        assert_eq!(
            models_mock.requests().len(),
            1,
            "expected a single /models request"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_uses_cache_when_fresh() {
        let server = MockServer::start().await;
        let remote_models = vec![remote_model("cached", "Cached", 5)];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            savfox_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("first refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);

        // Second call should read from cache and avoid the network.
        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("cached refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);
        assert_eq!(
            models_mock.requests().len(),
            1,
            "cache hit should avoid a second /models request"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_skips_remote_fetch_when_auth_required_but_missing() {
        let server = MockServer::start().await;
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: vec![remote_model("should-not-fetch", "Should Not Fetch", 1)],
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);

        let auth_manager = Arc::new(AuthManager::new(
            savfox_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let mut provider = provider_for(server.uri());
        provider.requires_openai_auth = true;
        let manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("refresh should not fail when auth is missing");

        assert_eq!(
            models_mock.requests().len(),
            0,
            "no /models request should be sent without credentials for auth-required providers"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_refetches_when_cache_stale() {
        let server = MockServer::start().await;
        let initial_models = vec![remote_model("stale", "Stale", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models.clone(),
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            savfox_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        // Rewrite cache with an old timestamp so it is treated as stale.
        manager
            .cache_manager
            .manipulate_cache_for_test(|fetched_at| {
                *fetched_at = Utc::now() - chrono::Duration::hours(1);
            })
            .await
            .expect("cache manipulation succeeds");

        let updated_models = vec![remote_model("fresh", "Fresh", 9)];
        server.reset().await;
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: updated_models.clone(),
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &updated_models);
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "stale cache refresh should fetch /models once"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_refetches_when_version_mismatch() {
        let server = MockServer::start().await;
        let initial_models = vec![remote_model("old", "Old", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models.clone(),
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            savfox_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        manager
            .cache_manager
            .mutate_cache_for_test(|cache| {
                let client_version = crate::models_manager::client_version_to_whole();
                cache.client_version = Some(format!("{client_version}-mismatch"));
            })
            .await
            .expect("cache mutation succeeds");

        let updated_models = vec![remote_model("new", "New", 2)];
        server.reset().await;
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: updated_models.clone(),
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &updated_models);
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "version mismatch should fetch /models once"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_drops_removed_remote_models() {
        let server = MockServer::start().await;
        let initial_models = vec![remote_model("remote-old", "Remote Old", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models,
            },
        )
        .await;

        let savfox_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let mut manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);
        manager.cache_manager.set_ttl(Duration::ZERO);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        server.reset().await;
        let refreshed_models = vec![remote_model("remote-new", "Remote New", 1)];
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: refreshed_models,
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");

        let available = manager
            .try_list_models(&config)
            .expect("models should be available");
        assert!(
            available.iter().any(|preset| preset.slug == "remote-new"),
            "new remote model should be listed"
        );
        assert!(
            !available.iter().any(|preset| preset.slug == "remote-old"),
            "removed remote model should not be listed"
        );
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "second refresh should only hit /models once"
        );
    }

    #[test]
    fn build_available_models_picks_default_after_hiding_hidden_models() {
        let savfox_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("Test API Key"));
        let provider = provider_for("http://example.test".to_string());
        let mut manager =
            ModelsManager::with_provider(savfox_home.path().to_path_buf(), auth_manager, provider);
        manager.local_models = Vec::new();

        let hidden_model = remote_model_with_visibility("hidden", "Hidden", 0, "hide");
        let visible_model = remote_model_with_visibility("visible", "Visible", 1, "list");

        let expected_hidden = ModelPreset::from(hidden_model.clone());
        let mut expected_visible = ModelPreset::from(visible_model.clone());
        expected_visible.is_default = true;

        let available = manager.build_available_models(vec![hidden_model, visible_model]);

        assert_eq!(available, vec![expected_hidden, expected_visible]);
    }
}
