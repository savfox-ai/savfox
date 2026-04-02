#![allow(unused_imports)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use savfox_core::{AuthManager, SavfoxAuth};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::gateway_auth_manager;
use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_str, require_str};
use super::config::humanize_hyphenated_id;
use crate::channel::GatewayChannel;

// ── Models (per-provider store) ─────────────────────────────────────────

fn models_dir(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("models")
}

// ── Provider file v1 types ──────────────────────────────────────────────

fn default_version() -> u32 {
    1
}
fn default_auth_type() -> String {
    "api_key".to_owned()
}

/// On-disk representation of `models/{account_id}.json` (v1).
/// Persisted shape is `disabled_models` (blacklist) plus auth/provider metadata.
/// `models` is kept runtime-only for RPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderFile {
    #[serde(default = "default_version")]
    version: u32,
    /// Unique account identifier (filename stem). Defaults to `provider_id`.
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    provider_id: String,
    #[serde(default)]
    auth: Option<ProviderAuth>,
    #[serde(default, rename = "models", skip_serializing)]
    models: Vec<Value>,
    #[serde(default, alias = "enabled_models")]
    disabled_models: Vec<String>,
}

impl ProviderFile {
    /// Return the effective account id.
    fn account_id(&self) -> &str {
        let id = self.id.trim();
        if id.is_empty() {
            self.provider_id.trim()
        } else {
            id
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderAuth {
    #[serde(rename = "type", default = "default_auth_type")]
    auth_type: String,
    #[serde(default)]
    env_key: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

fn provider_models_from_slugs(provider_id: &str, slugs: &[String]) -> Vec<Value> {
    let canonical_provider = savfox_core::canonical_provider_id(provider_id);
    let default_slug = savfox_core::provider_default_model_slug(provider_id);
    let normalized: Vec<String> = slugs
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(
                savfox_core::parse_provider_prefixed_model(trimmed)
                    .map(|(_, model_slug)| model_slug.to_owned())
                    .unwrap_or_else(|| trimmed.to_owned()),
            )
        })
        .collect();

    savfox_core::provider_models_from_slugs(provider_id, &normalized)
        .into_iter()
        .map(|model| {
            let model_slug = model.slug;
            json!({
                "id": format!("{canonical_provider}/{model_slug}"),
                "name": model.name,
                "provider": canonical_provider.as_str(),
                "model_slug": model_slug,
                "is_default": default_slug == Some(model_slug.as_str()),
                "builtin": true,
            })
        })
        .collect()
}

fn hydrate_provider_file_disabled_models(file: &mut ProviderFile) {
    file.disabled_models = file
        .disabled_models
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(
                savfox_core::parse_provider_prefixed_model(trimmed)
                    .map(|(_, model_slug)| model_slug.to_owned())
                    .unwrap_or_else(|| trimmed.to_owned()),
            )
        })
        .collect();
}

fn hydrate_provider_file_models(file: &mut ProviderFile, provider_id_hint: &str) {
    if !file.models.is_empty() {
        return;
    }

    let provider_id = if file.provider_id.trim().is_empty() {
        provider_id_hint.trim().to_owned()
    } else {
        file.provider_id.trim().to_owned()
    };
    if provider_id.is_empty() {
        return;
    }

    if file.provider_id.trim().is_empty() {
        file.provider_id = provider_id.clone();
    }

    // Build models from all provider defaults, excluding disabled ones.
    let disabled: HashSet<&str> = file.disabled_models.iter().map(String::as_str).collect();
    let all_slugs: Vec<String> = savfox_model::provider_default_models(&provider_id)
        .iter()
        .filter(|m| !disabled.contains(m.slug.as_str()))
        .map(|m| m.slug.clone())
        .collect();
    file.models = provider_models_from_slugs(&provider_id, &all_slugs);
}

/// Load a provider file, auto-detecting v1 (bare array) vs v2 (object).
/// The `account_id` is the filename stem (may be a bare provider_id for
/// legacy files, or a full account id like `"openai-work"`).
async fn load_provider_file(channel: &GatewayChannel, account_id: &str) -> ProviderFile {
    let path = models_dir(channel).join(format!("{account_id}.json"));
    let Ok(data) = tokio::fs::read_to_string(&path).await else {
        return ProviderFile {
            version: 2,
            id: account_id.to_owned(),
            provider_id: account_id.to_owned(),
            name: String::new(),
            slug: String::new(),
            auth: None,
            models: Vec::new(),
            disabled_models: Vec::new(),
        };
    };

    // Try v2 (object with "models" key) first, then fall back to v1 (bare array).
    if let Ok(mut file) = serde_json::from_str::<ProviderFile>(&data) {
        // Populate id from filename when missing in JSON (backward compat).
        if file.id.trim().is_empty() {
            file.id = account_id.to_owned();
        }
        // Derive slug from name when missing (backward compat with pre-slug files).
        if file.slug.trim().is_empty() && !file.name.trim().is_empty() {
            file.slug = savfox_utils::string::normalize_slug(&file.name).unwrap_or_default();
        }
        hydrate_provider_file_disabled_models(&mut file);
        hydrate_provider_file_models(&mut file, account_id);
        return file;
    }
    if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
        return ProviderFile {
            version: 2,
            id: account_id.to_owned(),
            provider_id: account_id.to_owned(),
            name: String::new(),
            slug: String::new(),
            auth: None,
            models,
            disabled_models: Vec::new(),
        };
    }

    ProviderFile {
        version: 2,
        id: account_id.to_owned(),
        provider_id: account_id.to_owned(),
        name: String::new(),
        slug: String::new(),
        auth: None,
        models: Vec::new(),
        disabled_models: Vec::new(),
    }
}

/// Save a v2 provider file. Creates the `models/` dir on first write.
/// File is stored as `models/{account_id}.json`.
async fn save_provider_file(
    channel: &GatewayChannel,
    account_id: &str,
    file: &ProviderFile,
) -> Result<(), String> {
    let mut file_to_write = file.clone();
    hydrate_provider_file_disabled_models(&mut file_to_write);
    let dir = models_dir(channel);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("create models dir: {e}"))?;
    let path = dir.join(format!("{account_id}.json"));
    if file_to_write.models.is_empty()
        && file_to_write.disabled_models.is_empty()
        && file_to_write.auth.is_none()
    {
        let _ = tokio::fs::remove_file(&path).await;
        // If the removed provider was the default, fall back to another one.
        let savfox_home = channel.config().savfox_home.clone();
        let account = account_id.to_owned();
        tokio::task::spawn_blocking(move || {
            savfox_core::config::provider_store::fallback_default_provider_if_removed(
                &savfox_home,
                &account,
            );
        });
        return Ok(());
    }
    let data = serde_json::to_string_pretty(&file_to_write)
        .map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| format!("write error: {e}"))
}

/// Inject a single provider's auth into the runtime override maps.
///
/// Sets both the env-variable override (for providers with `env_key`) and
/// the bearer-token override (keyed by account id, for providers like
/// OpenAI whose built-in config has `env_key: None`). Also sets the
/// override for the bare provider_id for backward compat when they differ.
fn inject_provider_auth(file: &ProviderFile) {
    if let Some(auth) = &file.auth
        && let Some(api_key) = &auth.api_key
        && !api_key.is_empty()
    {
        // Set env-variable override (for providers that use env_key).
        if let Some(env_key) = &auth.env_key
            && !env_key.is_empty()
        {
            savfox_core::set_env_override(env_key, api_key);
        }
        // Set bearer-token override keyed by account id.
        let account_id = file.account_id();
        if !account_id.is_empty() {
            savfox_core::set_bearer_token_override(account_id, api_key);
        }
        // Also set for bare provider_id for backward compat.
        let provider_id = file.provider_id.trim();
        if !provider_id.is_empty() && provider_id != account_id {
            savfox_core::set_bearer_token_override(provider_id, api_key);
        }
    }
}

/// Read all `models/*.json` files and inject their auth into the runtime
/// env-override map. Called once at gateway startup.
pub(crate) async fn inject_all_provider_auth(channel: &GatewayChannel) {
    let dir = models_dir(channel);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let account_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        if account_id.is_empty() {
            continue;
        }
        let file = load_provider_file(channel, &account_id).await;
        inject_provider_auth(&file);
    }
}

#[derive(Clone, Debug)]
struct RemoteModelsRequest {
    provider: String,
    base_url: String,
    api_key: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug)]
struct RemoteModelsHttpResponse {
    url: String,
    status: reqwest::StatusCode,
    request_id: Option<String>,
    body: String,
}

fn canonical_models_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "zhipu" | "zhipu-ai" => "zhipuai".to_owned(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_owned(),
        "volc" | "volc-engine" | "ark" => "volcengine".to_owned(),
        "together" | "together-ai" => "togetherai".to_owned(),
        "gemini" => "google".to_owned(),
        "bedrock" => "amazon-bedrock".to_owned(),
        "qwen" => "alibaba".to_owned(),
        "googlevertex" | "google_vertex" => "google-vertex".to_owned(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_owned(),
        other => other.to_owned(),
    }
}

fn models_provider_display_name(provider_id: &str) -> String {
    let canonical = savfox_core::canonical_provider_id(provider_id);
    savfox_core::built_in_model_providers()
        .get(canonical.as_str())
        .map(|provider| provider.name.clone())
        .unwrap_or_else(|| humanize_hyphenated_id(canonical.as_str()))
}

pub(crate) fn model_test_default_base_url(provider_id: &str) -> Option<String> {
    savfox_core::provider_default_base_url(provider_id)
}

fn model_test_extract_count(payload: &Value) -> Option<usize> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| arr.len())
        .or_else(|| {
            payload
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| arr.len())
        })
}

fn model_test_extract_models_array(payload: &Value) -> Option<&Vec<Value>> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
}

fn model_test_extract_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("x-oai-request-id"))
        .or_else(|| headers.get("cf-ray"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn model_test_truncate_oneline(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let mut out = String::new();
    for (idx, ch) in collapsed.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn model_test_nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn model_test_remote_requested(params: &Value) -> bool {
    params.get("provider").is_some()
        || params.get("provider_id").is_some()
        || params.get("base_url").is_some()
        || params.get("api_key").is_some()
}

fn model_test_resolve_remote_request(
    params: &Value,
) -> Result<Option<RemoteModelsRequest>, (i64, String)> {
    if !model_test_remote_requested(params) {
        return Ok(None);
    }

    let provider = model_test_nonempty_string(params.get("provider"))
        .or_else(|| model_test_nonempty_string(params.get("provider_id")))
        .map(|value| canonical_models_provider_id(&value))
        .unwrap_or_default();
    let explicit_base_url = model_test_nonempty_string(params.get("base_url"));
    let base_url = explicit_base_url
        .or_else(|| model_test_default_base_url(&provider))
        .ok_or((
            INVALID_PARAMS,
            "missing 'base_url' parameter for model discovery".to_owned(),
        ))?;
    let api_key = model_test_nonempty_string(params.get("api_key"));

    Ok(Some(RemoteModelsRequest {
        provider,
        base_url,
        api_key,
        account_id: None,
    }))
}

fn model_test_is_openai_platform_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.openai.com" || normalized == "https://api.openai.com/v1"
}

fn model_test_chatgpt_codex_base_url(channel: &Arc<GatewayChannel>) -> String {
    let base = channel
        .config()
        .chatgpt_base_url
        .trim()
        .trim_end_matches('/');
    if base.ends_with("/codex") {
        base.to_owned()
    } else {
        format!("{base}/codex")
    }
}

async fn model_test_apply_openai_auth_fallback(
    request: &mut RemoteModelsRequest,
    channel: &Arc<GatewayChannel>,
) {
    if request.provider != "openai" || request.api_key.is_some() {
        return;
    }

    let auth_manager = gateway_auth_manager(channel);
    // Force reload to pick up freshly obtained ChatGPT OAuth tokens (the
    // login server may have just persisted them).
    if auth_manager.auth_cached().is_none() {
        auth_manager.reload();
    }
    let Some(auth) = auth_manager.auth().await else {
        return;
    };

    if let Ok(token) = auth.get_token() {
        let token = token.trim();
        if !token.is_empty() {
            request.api_key = Some(token.to_owned());
        }
    }

    if let Some(account_id) = auth.get_account_id() {
        let account_id = account_id.trim();
        if !account_id.is_empty() {
            request.account_id = Some(account_id.to_owned());
        }
    }

    if auth.is_chatgpt_auth() && model_test_is_openai_platform_base_url(&request.base_url) {
        request.base_url = model_test_chatgpt_codex_base_url(channel);
    }
}

async fn model_test_fetch_remote_models(
    request: &RemoteModelsRequest,
) -> Result<RemoteModelsHttpResponse, (i64, String)> {
    let client_version = savfox_core::models_manager::client_version_to_whole();
    let url = format!(
        "{}/models?client_version={client_version}",
        request.base_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to build model test client: {err}"),
            )
        })?;

    let mut http_request = client.get(&url);
    if let Some(key) = request.api_key.as_deref() {
        http_request = http_request.bearer_auth(key);
    }
    if let Some(account_id) = request.account_id.as_deref() {
        http_request = http_request.header("chatgpt-account-id", account_id);
    }

    let response = http_request.send().await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("connection request failed for {url}: {err}"),
        )
    })?;

    let status = response.status();
    let request_id = model_test_extract_request_id(response.headers());
    let body = response.text().await.unwrap_or_default();

    Ok(RemoteModelsHttpResponse {
        url,
        status,
        request_id,
        body,
    })
}

fn model_test_http_failure_message(prefix: &str, response: &RemoteModelsHttpResponse) -> String {
    let mut message = format!(
        "{prefix}: HTTP {} {}",
        response.status.as_u16(),
        response.status.canonical_reason().unwrap_or("Unknown"),
    );
    let body_preview = model_test_truncate_oneline(&response.body, 240);
    if !body_preview.is_empty() {
        message.push_str(&format!(": {body_preview}"));
    }
    message
}

fn model_reasoning_flag(item: &Value) -> bool {
    item.get("supports_reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || item
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || item
            .get("capabilities")
            .and_then(|value| value.get("reasoning"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || item
            .get("options")
            .and_then(|value| value.get("thinking"))
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .map(|value| matches!(value.trim(), "enabled" | "auto"))
            .unwrap_or(false)
}

fn binary_reasoning_levels_value() -> Value {
    json!([
        {
            "effort": "none",
            "description": "Disable additional reasoning for faster responses",
        },
        {
            "effort": "medium",
            "description": "Enable provider-managed reasoning",
        }
    ])
}

fn normalize_reasoning_effort_alias(value: &Value) -> Option<Value> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| json!(value))
}

fn normalize_reasoning_level_aliases(value: &Value) -> Option<Value> {
    let levels = value.as_array()?;
    let mut normalized = Vec::with_capacity(levels.len());
    for level in levels {
        if let Some(effort) = level
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            normalized.push(json!({
                "effort": effort,
                "description": "",
            }));
            continue;
        }

        let Some(level_obj) = level.as_object() else {
            continue;
        };
        let Some(effort) = level_obj
            .get("effort")
            .and_then(Value::as_str)
            .or_else(|| level_obj.get("reasoning_effort").and_then(Value::as_str))
            .or_else(|| level_obj.get("reasoningEffort").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let description = level_obj
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        normalized.push(json!({
            "effort": effort,
            "description": description,
        }));
    }

    (!normalized.is_empty()).then_some(Value::Array(normalized))
}

fn extract_model_provider_and_slug(entry: &Value) -> Option<(String, String)> {
    let raw_id = entry.get("id").and_then(Value::as_str).map(str::trim);
    let parsed_from_id = raw_id
        .filter(|value| !value.is_empty())
        .and_then(savfox_core::parse_provider_prefixed_model)
        .map(|(provider_id, model_slug)| {
            (
                canonical_models_provider_id(provider_id),
                model_slug.trim().to_owned(),
            )
        });

    let provider_id = entry
        .get("provider")
        .and_then(|value| match value {
            Value::String(provider) => Some(provider.clone()),
            Value::Object(provider) => provider
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            _ => None,
        })
        .map(|value| canonical_models_provider_id(&value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            parsed_from_id
                .as_ref()
                .map(|(provider_id, _)| provider_id.clone())
        })?;

    let model_slug = entry
        .get("model_slug")
        .and_then(Value::as_str)
        .or_else(|| entry.get("code").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| parsed_from_id.map(|(_, model_slug)| model_slug))?;

    Some((provider_id, model_slug))
}

pub(crate) fn enrich_model_reasoning_metadata(entry: &mut Value) {
    let provider_and_slug = extract_model_provider_and_slug(entry);
    let registry_model = provider_and_slug
        .as_ref()
        .and_then(|(provider_id, model_slug)| {
            savfox_model::provider_model_info(provider_id, model_slug)
        });
    let source_supports_reasoning = model_reasoning_flag(entry);

    let Some(model) = entry.as_object_mut() else {
        return;
    };

    if model.get("default_reasoning_level").is_none()
        && let Some(default_reasoning_level) = model
            .get("default_reasoning_effort")
            .or_else(|| model.get("defaultReasoningEffort"))
            .or_else(|| model.get("defaultReasoningLevel"))
            .and_then(normalize_reasoning_effort_alias)
    {
        model.insert(
            "default_reasoning_level".to_owned(),
            default_reasoning_level,
        );
    }

    if model.get("supported_reasoning_levels").is_none()
        && let Some(supported_reasoning_levels) = model
            .get("supported_reasoning_efforts")
            .or_else(|| model.get("supportedReasoningEfforts"))
            .or_else(|| model.get("supportedReasoningLevels"))
            .and_then(normalize_reasoning_level_aliases)
    {
        model.insert(
            "supported_reasoning_levels".to_owned(),
            supported_reasoning_levels,
        );
    }

    if let Some(registry_model) = registry_model {
        if model.get("default_reasoning_level").is_none()
            && let Some(default_reasoning_level) = registry_model.default_reasoning_level
        {
            model.insert(
                "default_reasoning_level".to_owned(),
                serde_json::to_value(default_reasoning_level).unwrap_or(Value::Null),
            );
        }

        let has_supported_levels = model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty());
        if !has_supported_levels && !registry_model.supported_reasoning_levels.is_empty() {
            model.insert(
                "supported_reasoning_levels".to_owned(),
                serde_json::to_value(registry_model.supported_reasoning_levels)
                    .unwrap_or(Value::Null),
            );
        }
    }

    let has_supported_levels = model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .is_some_and(|levels| !levels.is_empty());
    let has_default_reasoning = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());

    if source_supports_reasoning || has_supported_levels || has_default_reasoning {
        if !has_default_reasoning {
            model.insert("default_reasoning_level".to_owned(), json!("medium"));
        }
        if !has_supported_levels {
            model.insert(
                "supported_reasoning_levels".to_owned(),
                binary_reasoning_levels_value(),
            );
        }
    }
}

fn model_test_parse_remote_models(payload: &Value, provider_hint: &str) -> Vec<Value> {
    let mut parsed = savfox_core::remote_models::parse_remote_models(payload, provider_hint);
    for entry in &mut parsed {
        enrich_model_reasoning_metadata(entry);
    }
    parsed
}

pub(crate) async fn handle_models_test(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    use savfox_core::models_manager::manager::RefreshStrategy;

    if let Some(model_id) = params
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let models = channel
            .session_manager()
            .list_models(channel.config(), RefreshStrategy::OnlineIfUncached)
            .await;

        let ok = models.iter().any(|preset| {
            preset.id == model_id
                || preset.slug == model_id
                || preset.name.eq_ignore_ascii_case(model_id)
        });

        let message = if ok {
            format!("Model `{model_id}` is available.")
        } else {
            format!("Model `{model_id}` was not found in the available model list.")
        };

        return Ok(json!({
            "ok": ok,
            "message": message,
            "model": model_id,
        }));
    }

    let mut request = model_test_resolve_remote_request(params)?.ok_or((
        INVALID_PARAMS,
        "missing provider/base_url parameters for connection test".to_owned(),
    ))?;
    model_test_apply_openai_auth_fallback(&mut request, channel).await;
    let response = model_test_fetch_remote_models(&request).await?;

    if response.status.is_success() {
        let model_count = serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|payload| model_test_extract_count(&payload));
        let message = if let Some(count) = model_count {
            format!("Connection successful. Retrieved {count} model(s).")
        } else {
            "Connection successful.".to_owned()
        };

        return Ok(json!({
            "ok": true,
            "message": message,
            "status": response.status.as_u16(),
            "model_count": model_count,
            "base_url": request.base_url,
        }));
    }

    let message = model_test_http_failure_message("Connection failed", &response);

    Ok(json!({
        "ok": false,
        "message": message,
        "status": response.status.as_u16(),
        "base_url": request.base_url,
    }))
}

/// Load models for a single account (thin wrapper over `load_provider_file`).
async fn load_provider_models(channel: &GatewayChannel, account_id: &str) -> Vec<Value> {
    load_provider_file(channel, account_id).await.models
}

/// Load all models from all `models/*.json` files, merged into a flat HashMap keyed by model ID.
async fn load_all_provider_models(channel: &GatewayChannel) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let dir = models_dir(channel);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let account_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        if account_id.is_empty() {
            continue;
        }
        let file = load_provider_file(channel, &account_id).await;
        for model in file.models {
            if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                out.insert(id.to_owned(), model);
            }
        }
    }
    out
}

/// Save models array to `models/{account_id}.json`. Preserves existing auth.
async fn save_provider_models(
    channel: &GatewayChannel,
    account_id: &str,
    models: &[Value],
) -> Result<(), String> {
    let mut file = load_provider_file(channel, account_id).await;
    file.models = models.to_vec();
    save_provider_file(channel, account_id, &file).await
}

pub(crate) async fn handle_models_list(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    if let Some(mut request) = model_test_resolve_remote_request(params)? {
        model_test_apply_openai_auth_fallback(&mut request, channel).await;
        let response = model_test_fetch_remote_models(&request).await?;
        if !response.status.is_success() {
            return Err((
                INTERNAL_ERROR,
                model_test_http_failure_message("remote model list request failed", &response),
            ));
        }

        let payload = serde_json::from_str::<Value>(&response.body).map_err(|err| {
            let preview = model_test_truncate_oneline(&response.body, 240);
            (
                INTERNAL_ERROR,
                format!(
                    "failed to parse model list response from {}: {err}; body={preview}",
                    response.url
                ),
            )
        })?;
        let models = model_test_parse_remote_models(&payload, &request.provider);
        return Ok(json!({ "models": models }));
    }

    // Per-provider store is the source of truth for the models page / selector.
    // Built-in engine presets are intentionally omitted: they lack a provider prefix
    // in their IDs and would display as one-model "provider" groups.  The wizard
    // writes every selected model into the per-provider store, so anything the user
    // has configured will appear here.
    let mut models: Vec<Value> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let dir = models_dir(channel);
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let account_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if account_id.is_empty() {
                continue;
            }
            let file = load_provider_file(channel, &account_id).await;
            let file_slug = file.slug.trim().to_owned();
            let file_provider_id = file.provider_id.trim().to_owned();
            let file_name = file.name.trim().to_owned();
            let file_provider_name = if file_provider_id.is_empty() {
                String::new()
            } else {
                models_provider_display_name(&file_provider_id)
            };
            let has_distinct_account = account_id != file_provider_id;
            for mut model in file.models {
                // Ensure every model has an `id` field.  ChatGPT OAuth files
                // store "slug" without "id", so synthesize it from account_id.
                if model.get("id").and_then(Value::as_str).is_none() {
                    let slug = model
                        .get("slug")
                        .or_else(|| model.get("model_slug"))
                        .and_then(Value::as_str)
                        .map(String::from);
                    if let Some(slug) = slug
                        && let Value::Object(ref mut map) = model
                    {
                        map.insert("id".to_owned(), json!(format!("{account_id}/{slug}")));
                    }
                }

                // Normalise the id prefix and provider to the account_id
                // (the file-level "id" from models/*.json) so that the
                // frontend writes the correct provider value to config.toml.
                if let Value::Object(ref mut map) = model {
                    if let Some(current_id) =
                        map.get("id").and_then(|v| v.as_str()).map(String::from)
                        && let Some((_, model_slug)) = current_id.split_once('/')
                        && !current_id.starts_with(&format!("{account_id}/"))
                    {
                        map.insert("id".to_owned(), json!(format!("{account_id}/{model_slug}")));
                    }
                    map.insert("provider".to_owned(), json!(account_id.as_str()));
                }

                let Some(id) = model.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if !seen_ids.insert(id.to_owned()) {
                    continue;
                }
                let mut entry = model;
                if let Value::Object(ref mut map) = entry {
                    map.entry("builtin".to_owned()).or_insert(json!(false));
                    map.remove("api_key");
                    if has_distinct_account {
                        let slug = if file_slug.is_empty() {
                            account_id
                                .strip_prefix(&file_provider_id)
                                .and_then(|rest| rest.strip_prefix('-'))
                                .unwrap_or(&account_id)
                                .to_owned()
                        } else {
                            file_slug.clone()
                        };
                        map.entry("account_slug".to_owned())
                            .or_insert_with(|| json!(slug));
                    }
                    if !file_provider_name.is_empty() {
                        map.entry("provider_name".to_owned())
                            .or_insert_with(|| json!(file_provider_name.clone()));
                    }
                    if !file_name.is_empty() {
                        map.entry("account_name".to_owned())
                            .or_insert_with(|| json!(file_name));
                    }
                }
                enrich_model_reasoning_metadata(&mut entry);
                models.push(entry);
            }
        }
    }

    Ok(json!({ "models": models }))
}

pub(crate) async fn handle_models_add(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider = require_str(params, "provider")?;
    let model_slug = require_str(params, "model_slug")?;

    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| format!("{provider}/{model_slug}"));

    let mut entry = json!({
        "id": id,
        "provider": provider,
        "model_slug": model_slug,
        "is_default": false,
        "is_disabled": false,
        "builtin": false,
    });

    // Copy optional fields
    for field in &[
        "api_key",
        "base_url",
        "max_tokens",
        "temperature",
        "name",
        "is_disabled",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    // The provider field doubles as account_id for file routing.
    let mut models = load_provider_models(channel, provider).await;
    // Remove existing entry with same id if present
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(&id));
    models.push(entry);
    save_provider_models(channel, provider, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "added" }))
}

pub(crate) async fn handle_models_update(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = require_str(params, "id")?;

    // The prefix before '/' is the account id (which is also the file stem).
    let account_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, account_id).await;
    // Find existing entry or create new one
    let entry = if let Some(pos) = models
        .iter()
        .position(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        &mut models[pos]
    } else {
        models.push(json!({"id": id}));
        models
            .last_mut()
            .expect("just-pushed model entry should be present")
    };

    // Merge updatable fields
    for field in &[
        "provider",
        "model_slug",
        "api_key",
        "base_url",
        "name",
        "is_disabled",
        "max_tokens",
        "temperature",
        "cost_input_per_m",
        "cost_output_per_m",
        "cost_cache_read_per_m",
        "cost_cache_write_per_m",
        "context_window",
        "max_output_tokens",
        "auth_type",
        "custom_headers",
    ] {
        if let Some(v) = params.get(*field) {
            entry[*field] = v.clone();
        }
    }

    save_provider_models(channel, account_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "updated" }))
}

pub(crate) async fn handle_models_delete(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = require_str(params, "id")?;

    let account_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, account_id).await;
    models.retain(|m| m.get("id").and_then(|v| v.as_str()) != Some(id));
    save_provider_models(channel, account_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "deleted" }))
}

pub(crate) async fn handle_models_setdefault(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = require_str(params, "id")?;

    let account_id = id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");

    let mut models = load_provider_models(channel, account_id).await;

    // Clear default on all entries in this provider
    for m in models.iter_mut() {
        if let Value::Object(map) = m {
            map.insert("is_default".to_owned(), json!(false));
        }
    }

    // Set default on target (create entry if it doesn't exist yet)
    if let Some(entry) = models
        .iter_mut()
        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        entry["is_default"] = json!(true);
    } else {
        models.push(json!({"id": id, "is_default": true}));
    }

    save_provider_models(channel, account_id, &models)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "id": id, "status": "default_set" }))
}

pub(crate) async fn handle_models_import(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let provider_id = require_str(params, "provider_id")?;

    let models_arr = params.get("models").and_then(|v| v.as_array()).ok_or((
        INVALID_REQUEST,
        "missing or invalid 'models' array".to_owned(),
    ))?;
    if models_arr.is_empty() {
        return Err((INVALID_REQUEST, "'models' array is empty".to_owned()));
    }

    // Build auth section from params
    let api_key_val = opt_str(params, "api_key", "");
    let env_key_val = opt_str(params, "env_key", "");
    let display_name_val = opt_str(params, "name", provider_id);

    // Generate account id from provider_id and display name.
    let account_id =
        savfox_core::config::provider_store::slugify_account_id(provider_id, display_name_val);

    // Check for existing file that would be overwritten.
    let overwriting = {
        let path = models_dir(channel).join(format!("{account_id}.json"));
        path.exists()
    };

    // Build model entries, prefixing model IDs with account id.
    let mut entries: Vec<Value> = Vec::with_capacity(models_arr.len());
    for m in models_arr {
        let mut entry = m.clone();
        if let Value::Object(ref mut map) = entry {
            // Re-prefix model id with account_id if it currently uses provider_id.
            if let Some(id_val) = map.get("id").and_then(Value::as_str)
                && let Some((prefix, slug)) = savfox_core::parse_provider_prefixed_model(id_val)
                && prefix != account_id
            {
                map.insert("id".to_owned(), json!(format!("{account_id}/{slug}")));
            }
            map.entry("provider".to_owned())
                .or_insert_with(|| json!(account_id));
            map.entry("builtin".to_owned()).or_insert(json!(false));
        }
        entries.push(entry);
    }

    savfox_core::config::provider_store::persist_provider_connection(
        channel.config().savfox_home.as_path(),
        &account_id,
        provider_id,
        display_name_val,
        &entries,
        (!env_key_val.is_empty()).then_some(env_key_val),
        (!api_key_val.is_empty()).then_some(api_key_val),
    )
    .map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("failed to persist provider connection: {err}"),
        )
    })?;

    // Reload runtime auth overrides after the provider file has been written or migrated.
    savfox_core::inject_provider_auth_overrides_from_store(channel.config().savfox_home.as_path());

    let base_url = opt_str(params, "base_url", "");

    // Find the first default model (or first model)
    let default_entry = entries
        .iter()
        .find(|e| {
            e.get("is_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .or_else(|| entries.first());

    let mut config_edits = Vec::new();
    if !base_url.is_empty() {
        config_edits.push(savfox_core::config::edit::ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_owned(),
                account_id.clone(),
                "base_url".to_owned(),
            ],
            value: toml_edit::value(base_url.to_owned()),
        });
    }

    let model_to_persist = default_entry.and_then(|default| {
        let normalized_model = default
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .and_then(|id| {
                savfox_core::parse_provider_prefixed_model(id)
                    .map(|(_, model_slug)| model_slug.to_owned())
                    .or_else(|| Some(id.to_owned()))
            })
            .or_else(|| {
                default
                    .get("model_slug")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|slug| !slug.is_empty())
                    .map(ToString::to_string)
            })?;
        if normalized_model.trim().is_empty() {
            return None;
        }
        Some(format!("{account_id}/{}", normalized_model.trim()))
    });

    let mut config_builder =
        savfox_core::config::edit::ConfigEditsBuilder::new(channel.config().savfox_home.as_path())
            .with_edits(config_edits);
    if let Some(model_to_persist) = model_to_persist.as_deref() {
        config_builder = config_builder.set_model(Some(model_to_persist), None);
    }
    config_builder
        .apply()
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to update config: {err}")))?;

    let model_count = entries.len();
    let mut result = json!({
        "status": "imported",
        "provider_id": provider_id,
        "account_id": account_id,
        "model_count": model_count,
    });
    if overwriting {
        result["warning"] = json!("overwriting existing config");
    }
    Ok(result)
}

// ── Tools ───────────────────────────────────────────────────────────────────

pub(crate) async fn handle_tools_invoke(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let tool = require_str(params, "tool")?;
    let action = params.get("action").and_then(|v| v.as_str());
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    // Build a prompt that forces the agent to invoke the requested tool.
    let mut args = arguments.clone();
    if let Some(act) = action
        && let Value::Object(ref mut map) = args
    {
        map.entry("action")
            .or_insert_with(|| Value::String(act.to_owned()));
    }
    let args_str = serde_json::to_string_pretty(&args).unwrap_or_else(|_| "{}".to_owned());
    let prompt = format!(
        "Use the `{tool}` tool with exactly these arguments:\n\
         ```json\n{args_str}\n```\n\
         Return only the raw tool output. Do not add commentary."
    );

    match channel.invoke_agent_text(&prompt, "default").await {
        Ok(output) => {
            // Try to parse the output as JSON for a cleaner response.
            let result = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                Value::String(savfox_core::external_content::wrap_external_content(
                    &format!("tool:{tool}"),
                    &output,
                ))
            });
            Ok(json!({
                "ok": true,
                "tool": tool,
                "result": result,
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("tool invocation failed: {err}"))),
    }
}
