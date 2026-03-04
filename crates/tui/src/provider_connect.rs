use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use savfox_core::ModelProviderInfo;
#[cfg(test)]
use savfox_core::config::provider_store::{
    has_provider_store_configuration, persist_provider_connection,
};
use savfox_core::config::provider_store::{
    provider_env_key_for_store, read_provider_store_api_key,
};
use serde_json::{Value, json};

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_CHATGPT_OPENAI_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";
const DEFAULT_LMSTUDIO_BASE_URL: &str = "http://localhost:1234/v1";

const CONNECT_PROVIDER_PRIORITY: [&str; 15] = [
    "openai",
    "anthropic",
    "gemini",
    "openrouter",
    "groq",
    "xai",
    "deepseek",
    "mistral",
    "together",
    "qwen",
    "minimax",
    "bedrock",
    "ollama",
    "ollama-chat",
    "lmstudio",
];

#[derive(Debug, Clone)]
pub(crate) struct ConnectProviderCandidate {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) requires_api_key: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderConnectResult {
    pub(crate) provider_id: String,
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) env_key: Option<String>,
    pub(crate) models: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderConnectRuntimeAuth {
    pub(crate) bearer_token: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) use_chatgpt_openai_base_url: bool,
}

fn trim_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn connect_provider_candidates(
    model_providers: &HashMap<String, ModelProviderInfo>,
) -> Vec<ConnectProviderCandidate> {
    let mut ordered: Vec<&str> = Vec::new();
    for provider_id in CONNECT_PROVIDER_PRIORITY {
        if model_providers.contains_key(provider_id) {
            ordered.push(provider_id);
        }
    }

    let mut extras: Vec<&str> = model_providers
        .keys()
        .map(String::as_str)
        .filter(|provider_id| !ordered.contains(provider_id))
        .collect();
    extras.sort_unstable();
    ordered.extend(extras);

    ordered
        .into_iter()
        .filter_map(|provider_id| {
            let provider = model_providers.get(provider_id)?;
            let requires_api_key = provider_requires_api_key(provider_id);
            let mut description_parts: Vec<String> = Vec::new();
            description_parts.push(if requires_api_key {
                "API key".to_string()
            } else {
                "No API key".to_string()
            });
            if let Some(base_url) = resolve_provider_base_url(provider_id, provider) {
                description_parts.push(base_url);
            } else {
                description_parts.push("configure base URL before use".to_string());
            }
            let description = description_parts.join(" · ");
            Some(ConnectProviderCandidate {
                id: provider_id.to_string(),
                name: provider.display_name.clone(),
                description,
                requires_api_key,
            })
        })
        .collect()
}

pub(crate) fn provider_requires_api_key(provider_id: &str) -> bool {
    !matches!(
        provider_id.trim().to_ascii_lowercase().as_str(),
        "ollama" | "ollama-chat" | "lmstudio"
    )
}

fn resolve_provider_api_key_from_env(
    provider_id: &str,
    provider: &ModelProviderInfo,
) -> Option<String> {
    if let Some(env_key) = provider.env_key.as_deref().and_then(trim_nonempty)
        && let Ok(value) = std::env::var(&env_key)
        && let Some(value) = trim_nonempty(&value)
    {
        return Some(value);
    }

    if let Some(env_headers) = &provider.env_http_headers {
        for env_var in env_headers.values() {
            if !env_var_looks_like_secret(env_var) {
                continue;
            }
            if let Ok(value) = std::env::var(env_var)
                && let Some(value) = trim_nonempty(&value)
            {
                return Some(value);
            }
        }
    }

    if let Some(fallback_env_key) = provider_env_key_for_store(provider_id, provider)
        && let Ok(value) = std::env::var(&fallback_env_key)
        && let Some(value) = trim_nonempty(&value)
    {
        return Some(value);
    }

    None
}

pub(crate) fn provider_has_auth_in_env(provider_id: &str, provider: &ModelProviderInfo) -> bool {
    if let Some(env_key) = provider.env_key.as_deref()
        && std::env::var(env_key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    {
        return true;
    }

    if let Some(env_headers) = &provider.env_http_headers
        && env_headers.values().any(|env_key| {
            std::env::var(env_key)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        })
    {
        return true;
    }

    resolve_provider_api_key_from_env(provider_id, provider).is_some()
}

pub(crate) fn resolve_provider_base_url(
    provider_id: &str,
    provider: &ModelProviderInfo,
) -> Option<String> {
    if let Some(base_url) = provider.base_url.as_deref().and_then(trim_nonempty) {
        return Some(base_url);
    }

    match provider_id.trim().to_ascii_lowercase().as_str() {
        "openai" => Some(DEFAULT_OPENAI_BASE_URL.to_string()),
        "ollama" | "ollama-chat" => Some(DEFAULT_OLLAMA_BASE_URL.to_string()),
        "lmstudio" => Some(DEFAULT_LMSTUDIO_BASE_URL.to_string()),
        _ => None,
    }
}

fn resolve_provider_base_url_for_connect(
    provider_id: &str,
    provider: &ModelProviderInfo,
    runtime_auth: &ProviderConnectRuntimeAuth,
) -> Option<String> {
    if let Some(base_url) = provider.base_url.as_deref().and_then(trim_nonempty) {
        return Some(base_url);
    }

    match provider_id.trim().to_ascii_lowercase().as_str() {
        "openai" => {
            if runtime_auth.use_chatgpt_openai_base_url {
                Some(DEFAULT_CHATGPT_OPENAI_BASE_URL.to_string())
            } else {
                Some(DEFAULT_OPENAI_BASE_URL.to_string())
            }
        }
        "ollama" | "ollama-chat" => Some(DEFAULT_OLLAMA_BASE_URL.to_string()),
        "lmstudio" => Some(DEFAULT_LMSTUDIO_BASE_URL.to_string()),
        _ => None,
    }
}

#[derive(Debug)]
struct RemoteModelsHttpResponse {
    url: String,
    status: reqwest::StatusCode,
    request_id: Option<String>,
    body: String,
}

async fn fetch_remote_models(
    provider_id: &str,
    provider: &ModelProviderInfo,
    base_url: &str,
    bearer_token: Option<&str>,
    account_id: Option<&str>,
) -> Result<RemoteModelsHttpResponse, String> {
    let normalized_base_url = base_url.trim_end_matches('/');
    let mut url = format!("{normalized_base_url}/models");
    if provider_id.eq_ignore_ascii_case("openai")
        && normalized_base_url.eq_ignore_ascii_case(DEFAULT_CHATGPT_OPENAI_BASE_URL)
    {
        let client_version = savfox_core::models_manager::client_version_to_whole();
        let separator = if url.contains('?') { '&' } else { '?' };
        url.push(separator);
        url.push_str("client_version=");
        url.push_str(&client_version);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| format!("failed to build provider client: {err}"))?;

    let mut request = client.get(&url);
    if let Some(headers) = &provider.http_headers {
        for (name, value) in headers {
            request = request.header(name, value);
        }
    }

    if let Some(key) = bearer_token {
        request = request.bearer_auth(key);
        if provider_id.eq_ignore_ascii_case("anthropic") {
            request = request.header("x-api-key", key);
        }
        if let Some(env_headers) = &provider.env_http_headers {
            for (header_name, env_var) in env_headers {
                if env_var_looks_like_secret(env_var) {
                    request = request.header(header_name, key);
                }
            }
        }
    }
    if let Some(account_id) = account_id.and_then(trim_nonempty) {
        request = request.header("ChatGPT-Account-ID", account_id);
    }

    if provider_api_debug_enabled() {
        debug_provider_api_request(provider_id, &url, bearer_token, account_id);
    }

    let response = request
        .send()
        .await
        .map_err(|err| format!("connection request failed for {url}: {err}"))?;
    let status = response.status();
    let request_id = extract_request_id(response.headers());
    let body = response.text().await.unwrap_or_default();

    if provider_api_debug_enabled() {
        debug_provider_api_response(provider_id, &url, status, request_id.as_deref(), &body);
    }

    Ok(RemoteModelsHttpResponse {
        url,
        status,
        request_id,
        body,
    })
}

fn env_var_looks_like_secret(env_var: &str) -> bool {
    let env_var = env_var.to_ascii_uppercase();
    env_var.contains("API_KEY") || env_var.contains("TOKEN") || env_var.ends_with("_KEY")
}

fn extract_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("x-oai-request-id"))
        .or_else(|| headers.get("cf-ray"))
        .and_then(|value| value.to_str().ok())
        .and_then(trim_nonempty)
}

fn provider_api_debug_enabled() -> bool {
    std::env::var("SAVFOX_DEBUG_PROVIDER_API")
        .ok()
        .is_some_and(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
        })
}

#[allow(clippy::print_stdout)]
fn debug_provider_api_request(
    provider_id: &str,
    url: &str,
    bearer_token: Option<&str>,
    account_id: Option<&str>,
) {
    let has_bearer_token = bearer_token.is_some();
    let has_account_id = account_id.and_then(trim_nonempty).is_some();
    println!(
        "[savfox][provider-api][request] provider={provider_id} url={url} has_bearer_token={has_bearer_token} has_account_id={has_account_id}"
    );
}

#[allow(clippy::print_stdout)]
fn debug_provider_api_response(
    provider_id: &str,
    url: &str,
    status: reqwest::StatusCode,
    request_id: Option<&str>,
    body: &str,
) {
    let request_id = request_id.unwrap_or("-");
    let body_preview = truncate_oneline(body, 240);
    println!(
        "[savfox][provider-api][response] provider={provider_id} url={url} status={status} request_id={request_id} body={body_preview}"
    );
}

fn truncate_oneline(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let mut out = String::new();
    for (index, ch) in collapsed.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(trim_nonempty)
}

fn model_item_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| nonempty_string(item.get(*key)))
}

fn extract_models_array(payload: &Value) -> Option<&Vec<Value>> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
}

fn parse_remote_models(payload: &Value, provider_hint: &str) -> Vec<Value> {
    let models = match extract_models_array(payload) {
        Some(items) => items,
        None => return Vec::new(),
    };

    let hint = provider_hint.trim().to_ascii_lowercase();
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();

    for item in models {
        let (raw_id, display_name, provider_from_item, is_default) = if let Some(id) =
            model_item_field(item, &["id", "model", "model_id", "model_code", "slug"])
        {
            (
                id,
                model_item_field(item, &["name", "display_name", "label"]),
                model_item_field(item, &["provider"]),
                item.get("is_default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
        } else if let Some(id) = item.as_str().and_then(trim_nonempty) {
            (id, None, None, false)
        } else {
            continue;
        };

        let mut provider = provider_from_item.unwrap_or_default();
        let mut model_code = raw_id.clone();
        if let Some((provider_prefix, suffix)) = savfox_core::parse_provider_prefixed_model(&raw_id)
        {
            if provider.is_empty() {
                provider = provider_prefix.to_string();
            }
            model_code = suffix.to_string();
        } else if provider.is_empty() {
            provider = hint.clone();
        }

        if model_code.is_empty() {
            continue;
        }

        let model_id = if raw_id.contains('/') || provider.is_empty() {
            raw_id
        } else {
            format!("{provider}/{model_code}")
        };

        if !seen.insert(model_id.clone()) {
            continue;
        }

        let mut entry = serde_json::Map::new();
        entry.insert("id".to_string(), json!(model_id));
        entry.insert(
            "name".to_string(),
            json!(display_name.unwrap_or_else(|| model_code.clone())),
        );
        entry.insert("model_code".to_string(), json!(model_code));
        entry.insert("is_default".to_string(), json!(is_default));
        entry.insert("builtin".to_string(), json!(true));
        if !provider.is_empty() {
            entry.insert("provider".to_string(), json!(provider));
        }
        parsed.push(Value::Object(entry));
    }

    parsed
}

fn connection_failure_message(prefix: &str, response: &RemoteModelsHttpResponse) -> String {
    let mut message = format!(
        "{prefix}: HTTP {} {}",
        response.status.as_u16(),
        response.status.canonical_reason().unwrap_or("Unknown"),
    );
    let body_preview = truncate_oneline(&response.body, 240);
    if !body_preview.is_empty() {
        message.push_str(&format!(": {body_preview}"));
    }
    if let Some(request_id) = response.request_id.as_deref() {
        message.push_str(&format!(", request id: {request_id}"));
    }
    message
}

pub(crate) async fn connect_provider(
    savfox_home: PathBuf,
    provider_id: String,
    provider: ModelProviderInfo,
    api_key_input: Option<String>,
    runtime_auth: Option<ProviderConnectRuntimeAuth>,
) -> Result<ProviderConnectResult, String> {
    let runtime_auth = runtime_auth.unwrap_or_default();
    let base_url =
        resolve_provider_base_url_for_connect(&provider_id, &provider, &runtime_auth).ok_or_else(
            || {
                format!(
                    "Provider `{}` does not define a default base URL. Configure the provider endpoint first.",
                    provider.display_name
                )
            },
        )?;

    let should_ignore_saved_api_key =
        provider_id.eq_ignore_ascii_case("openai") && runtime_auth.use_chatgpt_openai_base_url;
    let existing_api_key = if should_ignore_saved_api_key {
        None
    } else {
        read_provider_store_api_key(savfox_home.as_path(), &provider_id)
    };

    let persisted_api_key = api_key_input
        .and_then(|value| trim_nonempty(&value))
        .or(existing_api_key);

    let runtime_bearer_token = if provider.requires_openai_auth {
        runtime_auth
            .bearer_token
            .clone()
            .and_then(|value| trim_nonempty(&value))
    } else {
        None
    };
    let request_bearer = persisted_api_key
        .clone()
        .or(runtime_bearer_token)
        .or_else(|| resolve_provider_api_key_from_env(&provider_id, &provider));

    if provider_requires_api_key(&provider_id)
        && request_bearer.is_none()
        && !provider_has_auth_in_env(&provider_id, &provider)
    {
        return Err(format!(
            "{} requires credentials. Enter an API key or set provider credentials in your environment.",
            provider.display_name
        ));
    }

    let response = fetch_remote_models(
        &provider_id,
        &provider,
        &base_url,
        request_bearer.as_deref(),
        runtime_auth.account_id.as_deref(),
    )
    .await?;
    if !response.status.is_success() {
        return Err(connection_failure_message("Connection failed", &response));
    }

    let payload = serde_json::from_str::<Value>(&response.body).map_err(|err| {
        format!(
            "failed to parse model list response from {}: {err}; body={}",
            response.url,
            truncate_oneline(&response.body, 240)
        )
    })?;
    let models = parse_remote_models(&payload, &provider_id);
    if models.is_empty() {
        return Err("Connection succeeded, but no models were returned.".to_string());
    }

    let env_key = provider_env_key_for_store(&provider_id, &provider);
    let provider_name = provider.display_name.clone();

    Ok(ProviderConnectResult {
        provider_id: provider_id.clone(),
        provider_name,
        base_url,
        api_key: persisted_api_key,
        env_key,
        models,
    })
}

fn model_id_from_entry(item: &Value, provider_id: &str) -> Option<String> {
    let raw = nonempty_string(item.get("id"))
        .or_else(|| nonempty_string(item.get("model")))
        .or_else(|| nonempty_string(item.get("model_code")))?;
    if raw.contains('/') || provider_id.trim().is_empty() {
        Some(raw)
    } else {
        Some(format!("{provider_id}/{raw}"))
    }
}

pub(crate) fn select_default_model(models: &[Value], provider_id: &str) -> Option<String> {
    models
        .iter()
        .find(|item| {
            item.get("is_default")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .and_then(|item| model_id_from_entry(item, provider_id))
        .or_else(|| {
            models
                .first()
                .and_then(|item| model_id_from_entry(item, provider_id))
        })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn parse_remote_models_reads_openai_shape() {
        let payload = json!({
            "data": [
                { "id": "gpt-5.1", "name": "GPT-5.1", "is_default": true },
                { "id": "gpt-4.1", "name": "GPT-4.1" }
            ]
        });

        let parsed = parse_remote_models(&payload, "openai");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].get("id").and_then(Value::as_str),
            Some("openai/gpt-5.1")
        );
        assert_eq!(
            parsed[1].get("id").and_then(Value::as_str),
            Some("openai/gpt-4.1")
        );
    }

    #[test]
    fn select_default_model_prefers_default_flag() {
        let models = vec![
            json!({"id": "openai/gpt-4.1"}),
            json!({"id": "openai/gpt-5.1", "is_default": true}),
        ];
        let selected = select_default_model(&models, "openai");
        assert_eq!(selected.as_deref(), Some("openai/gpt-5.1"));
    }

    #[test]
    fn provider_env_key_for_store_prefers_secret_like_env_headers() {
        let provider = ModelProviderInfo {
            slug: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            base_url: Some("https://api.anthropic.com".to_string()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: savfox_core::WireApi::Anthropic,
            query_params: None,
            http_headers: None,
            env_http_headers: Some(
                [
                    (
                        "anthropic-version".to_string(),
                        "ANTHROPIC_VERSION".to_string(),
                    ),
                    ("x-api-key".to_string(), "ANTHROPIC_API_KEY".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        let env_key = provider_env_key_for_store("anthropic", &provider);
        assert_eq!(env_key.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn parse_remote_models_reads_slug_shape() {
        let payload = json!({
            "models": [
                { "slug": "gpt-5", "display_name": "GPT-5", "is_default": true }
            ]
        });

        let parsed = parse_remote_models(&payload, "openai");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].get("id").and_then(Value::as_str),
            Some("openai/gpt-5")
        );
        assert_eq!(parsed[0].get("name").and_then(Value::as_str), Some("GPT-5"));
    }

    #[test]
    fn openai_chatgpt_connect_uses_chatgpt_base_url_default() {
        let mut provider = ModelProviderInfo::create_openai_provider();
        provider.base_url = None;

        let base_url = resolve_provider_base_url_for_connect(
            "openai",
            &provider,
            &ProviderConnectRuntimeAuth {
                bearer_token: None,
                account_id: None,
                use_chatgpt_openai_base_url: true,
            },
        );
        assert_eq!(base_url.as_deref(), Some(DEFAULT_CHATGPT_OPENAI_BASE_URL));
    }

    #[test]
    fn persist_provider_connection_writes_to_savfox_models_dir() {
        let tmp = TempDir::new().expect("temp root");
        let savfox_home = tmp.path().join(".savfox");
        std::fs::create_dir_all(&savfox_home).expect("create savfox home");

        let result = ProviderConnectResult {
            provider_id: "anthropic".to_string(),
            provider_name: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: Some("sk-anthropic".to_string()),
            env_key: Some("ANTHROPIC_API_KEY".to_string()),
            models: vec![json!({
                "id": "anthropic/claude-sonnet",
                "name": "Claude Sonnet",
                "is_default": true
            })],
        };

        persist_provider_connection(
            &savfox_home,
            &result.provider_id,
            &result.provider_name,
            &result.models,
            result.env_key.as_deref(),
            result.api_key.as_deref(),
        )
        .expect("persist provider");

        let savfox_file = tmp.path().join(".savfox/models/anthropic.json");
        assert!(
            savfox_file.exists(),
            "expected provider file in ~/.savfox/models"
        );
        let raw: Value = serde_json::from_str(
            &std::fs::read_to_string(&savfox_file).expect("read persisted provider file"),
        )
        .expect("parse persisted provider file");
        assert_eq!(
            raw.get("enabled_models")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str),
            Some("claude-sonnet")
        );
        assert!(
            raw.get("models").is_some(),
            "persisted provider file should include serialized model metadata"
        );
        assert_eq!(
            read_provider_store_api_key(&savfox_home, "anthropic").as_deref(),
            Some("sk-anthropic")
        );
    }

    #[test]
    fn has_provider_store_configuration_detects_saved_provider() {
        let tmp = TempDir::new().expect("temp root");
        let savfox_home = tmp.path().join(".savfox");
        std::fs::create_dir_all(&savfox_home).expect("create savfox home");
        let models_dir = savfox_home.join("models");
        std::fs::create_dir_all(&models_dir).expect("create models dir");
        std::fs::write(
            models_dir.join("zhipuai-coding-plan.json"),
            r#"{
  "version": 2,
  "provider_id": "zhipuai-coding-plan",
  "display_name": "ZhipuAI Coding Plan",
  "auth": {
    "type": "api_key",
    "env_key": "ZHIPUAI_API_KEY",
    "api_key": "sk-test-zhipu"
  },
  "enabled_models": [
    "zhipuai-coding-plan/glm-5"
  ]
}"#,
        )
        .expect("write provider file");

        assert!(has_provider_store_configuration(&savfox_home));
    }

    #[test]
    fn has_provider_store_configuration_is_false_without_models_dir() {
        let tmp = TempDir::new().expect("temp root");
        let savfox_home = tmp.path().join(".savfox");
        std::fs::create_dir_all(&savfox_home).expect("create savfox home");
        assert!(!has_provider_store_configuration(&savfox_home));
    }

    #[test]
    fn has_provider_store_configuration_detects_chatgpt_token_store() {
        let tmp = TempDir::new().expect("temp root");
        let savfox_home = tmp.path().join(".savfox");
        let models_dir = savfox_home.join("models");
        std::fs::create_dir_all(&models_dir).expect("create models dir");
        std::fs::write(
            models_dir.join("chatgpt.json"),
            r#"{
  "version": 2,
  "provider_id": "chatgpt",
  "display_name": "ChatGPT",
  "auth": {
    "auth_mode": "chatgpt",
    "tokens": {
      "id_token": "id-token",
      "access_token": "access-token"
    }
  },
  "enabled_models": []
}"#,
        )
        .expect("write chatgpt provider file");

        assert!(has_provider_store_configuration(&savfox_home));
    }

    #[test]
    fn has_provider_store_configuration_supports_models_field() {
        let tmp = TempDir::new().expect("temp root");
        let savfox_home = tmp.path().join(".savfox");
        let models_dir = savfox_home.join("models");
        std::fs::create_dir_all(&models_dir).expect("create models dir");
        std::fs::write(
            models_dir.join("legacy-provider.json"),
            r#"{
  "version": 2,
  "provider_id": "legacy-provider",
  "display_name": "Legacy Provider",
  "models": [
    { "id": "legacy-provider/model-a" }
  ]
}"#,
        )
        .expect("write legacy provider file");

        assert!(has_provider_store_configuration(&savfox_home));
    }
}
