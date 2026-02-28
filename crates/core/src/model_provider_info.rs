//! Registry of model providers supported by Savfox.
//!
//! Providers can be defined in two places:
//!   1. Built-in defaults compiled into the binary so Savfox works out-of-the-box.
//!   2. User-defined entries inside `~/.savfox/config.toml` under the `model_providers` key. These
//!      override or extend the defaults at runtime.

use std::collections::HashMap;
use std::env::VarError;
use std::path::Path;
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use http::HeaderMap;
use http::header::{HeaderName, HeaderValue};
use savfox_api::provider::RetryConfig as ApiRetryConfig;
use savfox_api::{
    Provider as ApiProvider, WireApi as ApiWireApi, is_azure_responses_wire_base_url,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;
use crate::error::EnvVarError;

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/";
const DEFAULT_OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
/// Hard cap for user-configured `stream_max_retries`.
const MAX_STREAM_MAX_RETRIES: u64 = 100;
/// Hard cap for user-configured `request_max_retries`.
const MAX_REQUEST_MAX_RETRIES: u64 = 100;
pub const CHAT_WIRE_API_DEPRECATION_SUMMARY: &str = r#"Support for the "chat" wire API is deprecated and will soon be removed. Update your model provider definition in config.toml to use wire_api = "responses"."#;

// ── Runtime env-variable overrides ──────────────────────────────────────
//
// The gateway writes API keys to `config.toml` for restart persistence,
// but the core engine resolves keys via `std::env::var()` which is never
// updated at runtime.  This in-process map lets the gateway inject keys
// immediately so the engine can authenticate without a restart.

static ENV_OVERRIDES: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Bearer-token overrides keyed by **provider id** (e.g. `"openai"`,
/// `"anthropic"`).  Checked by `auth_provider_from_auth` as a last-resort
/// fallback when the provider's own `env_key` / `experimental_bearer_token`
/// / login auth all fail to yield a token.
static BEARER_TOKEN_OVERRIDES: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

const PROVIDER_MODELS_DIR: &str = "models";

/// Store an env-variable override that `api_key()` / `build_header_map()`
/// will consult before falling back to `std::env::var()`.
pub fn set_env_override(env_key: &str, value: &str) {
    if let Ok(mut map) = ENV_OVERRIDES.write() {
        map.insert(env_key.to_string(), value.to_string());
    }
}

/// Remove a previously set env-variable override.
pub fn remove_env_override(env_key: &str) {
    if let Ok(mut map) = ENV_OVERRIDES.write() {
        map.remove(env_key);
    }
}

fn get_env_override(env_key: &str) -> Option<String> {
    ENV_OVERRIDES
        .read()
        .ok()
        .and_then(|map| map.get(env_key).cloned())
        .filter(|v| !v.trim().is_empty())
}

/// Store a bearer-token override for a provider.  This is the primary
/// mechanism for the gateway to inject API keys at runtime for providers
/// whose built-in `ModelProviderInfo` has `env_key: None` (e.g. OpenAI).
pub fn set_bearer_token_override(provider_id: &str, token: &str) {
    if let Ok(mut map) = BEARER_TOKEN_OVERRIDES.write() {
        map.insert(provider_id.to_string(), token.to_string());
    }
}

/// Remove a previously set bearer-token override.
pub fn remove_bearer_token_override(provider_id: &str) {
    if let Ok(mut map) = BEARER_TOKEN_OVERRIDES.write() {
        map.remove(provider_id);
    }
}

/// Look up a bearer-token override for the given provider id.
pub fn get_bearer_token_override(provider_id: &str) -> Option<String> {
    BEARER_TOKEN_OVERRIDES
        .read()
        .ok()
        .and_then(|map| map.get(provider_id).cloned())
        .filter(|v| !v.trim().is_empty())
}

#[derive(Debug, Deserialize)]
struct ProviderStoreAuthFile {
    #[serde(default)]
    provider_id: String,
    #[serde(default)]
    auth: Option<ProviderStoreAuth>,
}

#[derive(Debug, Deserialize)]
struct ProviderStoreAuth {
    #[serde(default)]
    env_key: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

/// Load provider auth from `{savfox_home}/models/*.json` and inject runtime
/// overrides for API key resolution.
///
/// This enables TUI/CLI sessions to use provider keys stored by the gateway's
/// provider file format without requiring environment variables to be exported
/// in the parent shell.
pub fn inject_provider_auth_overrides_from_store(savfox_home: &Path) {
    let models_dir = savfox_home.join(PROVIDER_MODELS_DIR);
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let provider_id_from_filename = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::trim)
            .unwrap_or("");
        if provider_id_from_filename.is_empty() {
            continue;
        }

        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<ProviderStoreAuthFile>(&data) else {
            // Ignore legacy bare-array model files (no auth payload).
            continue;
        };
        let Some(auth) = file.auth else {
            continue;
        };
        let Some(api_key) = auth
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
        else {
            continue;
        };

        if let Some(env_key) = auth
            .env_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            set_env_override(env_key, &api_key);
        }

        let provider_id = file.provider_id.trim();
        let provider_id = if provider_id.is_empty() {
            provider_id_from_filename
        } else {
            provider_id
        };
        if !provider_id.is_empty() {
            set_bearer_token_override(provider_id, &api_key);
        }
    }
}

const OPENAI_PROVIDER_NAME: &str = "OpenAI";

/// Wire protocol that the provider speaks. Most third-party services only
/// implement the classic OpenAI Chat Completions JSON schema, whereas OpenAI
/// itself (and a handful of others) additionally expose the more modern
/// *Responses* API. The two protocols use different request/response shapes
/// and *cannot* be auto-detected at runtime, therefore each provider entry
/// must declare which one it expects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    /// The Responses API exposed by OpenAI at `/v1/responses`.
    Responses,

    /// Regular Chat Completions compatible with `/v1/chat/completions`.
    #[default]
    Chat,

    /// Anthropic Messages API at `/v1/messages`.
    Anthropic,
}

/// Serializable representation of a provider definition.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelProviderInfo {
    /// Friendly display name.
    pub name: String,
    /// Base URL for the provider's OpenAI-compatible API.
    pub base_url: Option<String>,
    /// Environment variable that stores the user's API key for this provider.
    pub env_key: Option<String>,

    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub env_key_instructions: Option<String>,

    /// Value to use with `Authorization: Bearer <token>` header. Use of this
    /// config is discouraged in favor of `env_key` for security reasons, but
    /// this may be necessary when using this programmatically.
    pub experimental_bearer_token: Option<String>,

    /// Which wire protocol this provider expects.
    #[serde(default)]
    pub wire_api: WireApi,

    /// Optional query parameters to append to the base URL.
    pub query_params: Option<HashMap<String, String>>,

    /// Additional HTTP headers to include in requests to this provider where
    /// the (key, value) pairs are the header name and value.
    pub http_headers: Option<HashMap<String, String>>,

    /// Optional HTTP headers to include in requests to this provider where the
    /// (key, value) pairs are the header name and _environment variable_ whose
    /// value should be used. If the environment variable is not set, or the
    /// value is empty, the header will not be included in the request.
    pub env_http_headers: Option<HashMap<String, String>>,

    /// Maximum number of times to retry a failed HTTP request to this provider.
    pub request_max_retries: Option<u64>,

    /// Number of times to retry reconnecting a dropped streaming response before failing.
    pub stream_max_retries: Option<u64>,

    /// Idle timeout (in milliseconds) to wait for activity on a streaming response before treating
    /// the connection as lost.
    pub stream_idle_timeout_ms: Option<u64>,

    /// Does this provider require an OpenAI API Key or ChatGPT login token? If true,
    /// user is presented with login screen on first run, and login preference and token/key
    /// are stored in auth storage. If false (which is the default), login screen is skipped,
    /// and API key (if needed) comes from the "env_key" environment variable.
    #[serde(default)]
    pub requires_openai_auth: bool,

    /// Whether this provider supports the Responses API WebSocket transport.
    #[serde(default)]
    pub supports_websockets: bool,
}

impl ModelProviderInfo {
    fn build_header_map(&self) -> crate::error::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(extra) = &self.http_headers {
            for (k, v) in extra {
                if let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                    headers.insert(name, value);
                }
            }
        }

        if let Some(env_headers) = &self.env_http_headers {
            for (header, env_var) in env_headers {
                let resolved = get_env_override(env_var)
                    .or_else(|| std::env::var(env_var).ok().filter(|v| !v.trim().is_empty()));
                if let Some(val) = resolved
                    && let (Ok(name), Ok(value)) =
                        (HeaderName::try_from(header), HeaderValue::try_from(val))
                {
                    headers.insert(name, value);
                }
            }
        }

        Ok(headers)
    }

    pub(crate) fn to_api_provider(
        &self,
        provider_id: Option<&str>,
        auth_mode: Option<AuthMode>,
        chatgpt_base_url: Option<&str>,
    ) -> crate::error::Result<ApiProvider> {
        let default_base_url = if self.base_url.is_some() {
            String::new()
        } else {
            let is_openai_provider = provider_id.is_some_and(is_openai_provider_id)
                || (provider_id.is_none() && self.is_openai());

            if matches!(auth_mode, Some(AuthMode::Chatgpt)) && is_openai_provider {
                default_chatgpt_openai_base_url(chatgpt_base_url)
            } else {
                provider_id
                    .and_then(provider_default_base_url)
                    .unwrap_or_else(|| DEFAULT_OPENAI_API_BASE_URL.to_string())
            }
        };
        let base_url = self.base_url.clone().unwrap_or(default_base_url);

        let headers = self.build_header_map()?;
        let retry = ApiRetryConfig {
            max_attempts: self.request_max_retries(),
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        };

        Ok(ApiProvider {
            name: self.name.clone(),
            base_url,
            query_params: self.query_params.clone(),
            wire: match self.wire_api {
                WireApi::Responses => ApiWireApi::Responses,
                WireApi::Chat => ApiWireApi::Chat,
                WireApi::Anthropic => ApiWireApi::Anthropic,
            },
            headers,
            retry,
            stream_idle_timeout: self.stream_idle_timeout(),
        })
    }

    pub(crate) fn is_azure_responses_endpoint(&self) -> bool {
        let wire = match self.wire_api {
            WireApi::Responses => ApiWireApi::Responses,
            WireApi::Chat => ApiWireApi::Chat,
            WireApi::Anthropic => ApiWireApi::Anthropic,
        };

        is_azure_responses_wire_base_url(wire, &self.name, self.base_url.as_deref())
    }

    /// If `env_key` is Some, returns the API key for this provider if present
    /// (and non-empty) in the environment. If `env_key` is required but
    /// cannot be found, returns an error.
    pub fn api_key(&self) -> crate::error::Result<Option<String>> {
        match &self.env_key {
            Some(env_key) => {
                // Check the in-process override map first (set by the gateway
                // when a provider is imported), then fall back to the real env.
                if let Some(val) = get_env_override(env_key) {
                    return Ok(Some(val));
                }

                let env_value = std::env::var(env_key);
                env_value
                    .and_then(|v| {
                        if v.trim().is_empty() {
                            Err(VarError::NotPresent)
                        } else {
                            Ok(Some(v))
                        }
                    })
                    .map_err(|_| {
                        crate::error::SavfoxError::EnvVar(EnvVarError {
                            var: env_key.clone(),
                            instructions: self.env_key_instructions.clone(),
                        })
                    })
            }
            None => Ok(None),
        }
    }

    /// Effective maximum number of request retries for this provider.
    pub fn request_max_retries(&self) -> u64 {
        self.request_max_retries
            .unwrap_or(DEFAULT_REQUEST_MAX_RETRIES)
            .min(MAX_REQUEST_MAX_RETRIES)
    }

    /// Effective maximum number of stream reconnection attempts for this provider.
    pub fn stream_max_retries(&self) -> u64 {
        self.stream_max_retries
            .unwrap_or(DEFAULT_STREAM_MAX_RETRIES)
            .min(MAX_STREAM_MAX_RETRIES)
    }

    /// Effective idle timeout for streaming responses.
    pub fn stream_idle_timeout(&self) -> Duration {
        self.stream_idle_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_STREAM_IDLE_TIMEOUT_MS))
    }
    pub fn create_openai_provider() -> ModelProviderInfo {
        ModelProviderInfo {
            name: OPENAI_PROVIDER_NAME.into(),
            // Allow users to override the default OpenAI endpoint by
            // exporting `OPENAI_BASE_URL`. This is useful when pointing
            // Savfox at a proxy, mock server, or Azure-style deployment
            // without requiring a full TOML override for the built-in
            // OpenAI provider.
            base_url: std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: Some(
                [("version".to_string(), env!("CARGO_PKG_VERSION").to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: Some(
                [
                    (
                        "OpenAI-Organization".to_string(),
                        "OPENAI_ORGANIZATION".to_string(),
                    ),
                    ("OpenAI-Project".to_string(), "OPENAI_PROJECT".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            // Use global defaults for retry/timeout unless overridden in config.toml.
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: true,
            supports_websockets: true,
        }
    }

    pub fn is_openai(&self) -> bool {
        self.name == OPENAI_PROVIDER_NAME
    }
}

fn is_openai_provider_id(provider_id: &str) -> bool {
    canonical_provider_id(provider_id) == "openai"
}

fn canonical_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "zhipu" | "zhipu-ai" => "zhipuai".to_string(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_string(),
        "together" | "together-ai" => "togetherai".to_string(),
        "gemini" => "google".to_string(),
        "bedrock" => "amazon-bedrock".to_string(),
        "qwen" => "alibaba".to_string(),
        "googlevertex" | "google_vertex" => "google-vertex".to_string(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_string(),
        other => other.to_string(),
    }
}

fn built_in_provider_key(provider_id: &str) -> Option<String> {
    match provider_id {
        "alibaba" | "alibaba-cn" => Some("qwen".to_string()),
        "amazon-bedrock" => Some("bedrock".to_string()),
        "google" | "google-vertex" | "google-vertex-anthropic" => Some("gemini".to_string()),
        "togetherai" => Some("together".to_string()),
        "zhipuai" | "zhipuai-coding-plan" => None,
        other => Some(other.to_string()),
    }
}

fn known_provider_base_url_map(provider_id: &str) -> Option<Option<&'static str>> {
    match provider_id {
        "abacus" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "aihubmix" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "alibaba" => Some(Some("https://dashscope.aliyuncs.com/compatible-mode/v1")),
        "alibaba-cn" => Some(Some("https://dashscope.aliyuncs.com/compatible-mode/v1")),
        "amazon-bedrock" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "anthropic" => Some(Some("https://api.anthropic.com")),
        "azure" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "azure-cognitive-services" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "bailing" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "baseten" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "cerebras" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "chutes" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "cloudflare-ai-gateway" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "cloudflare-workers-ai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "cohere" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "cortecs" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "deepinfra" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "deepseek" => Some(Some("https://api.deepseek.com/v1")),
        "fastrouter" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "fireworks-ai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "friendli" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "github-copilot" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "github-copilot-enterprise" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "github-models" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "gitlab" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "google" => Some(Some("https://generativelanguage.googleapis.com/v1beta/openai")),
        "google-vertex" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "google-vertex-anthropic" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "groq" => Some(Some("https://api.groq.com/openai/v1")),
        "helicone" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "huggingface" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "iflowcn" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "inception" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "inference" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "io-net" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "kimi-for-coding" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "llama" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "lmstudio" => Some(Some("http://localhost:1234/v1")),
        "lucidquery" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "minimax" => Some(Some("https://api.minimax.chat/v1")),
        "minimax-cn" => Some(Some("https://api.minimax.chat/v1")),
        "mistral" => Some(Some("https://api.mistral.ai/v1")),
        "modelscope" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "moonshotai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "moonshotai-cn" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "morph" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "nano-gpt" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "nebius" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "nvidia" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "ollama" => Some(Some("http://localhost:11434/v1")),
        "ollama-chat" => Some(Some("http://localhost:11434/v1")),
        "ollama-cloud" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "openai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "opencode" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "openrouter" => Some(Some("https://openrouter.ai/api/v1")),
        "other" => Some(None),
        "ovhcloud" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "perplexity" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "poe" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "requesty" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "sap-ai-core" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "scaleway" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "siliconflow" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "siliconflow-cn" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "togetherai" => Some(Some("https://api.together.xyz/v1")),
        "upstage" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "v0" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "venice" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "vercel" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "vultr" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "wandb" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "xai" => Some(Some("https://api.x.ai/v1")),
        "xiaomi" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "zai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "zai-coding-plan" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "zenmux" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "zhipuai" => Some(Some("https://open.bigmodel.cn/api/paas/v4")),
        "zhipuai-coding-plan" => Some(Some("https://open.bigmodel.cn/api/coding/paas/v4")),
        _ => None,
    }
}

pub fn provider_default_base_url(provider_id: &str) -> Option<String> {
    let canonical = canonical_provider_id(provider_id);
    let mapped_default = known_provider_base_url_map(canonical.as_str())?;

    let provider_key = built_in_provider_key(&canonical);
    if let Some(provider_key) = provider_key {
        let built_in = built_in_model_providers();
        if let Some(info) = built_in.get(provider_key.as_str()) {
            if let Some(base_url) = info
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(base_url.to_string());
            }
        }
    }

    mapped_default.map(str::to_string)
}

fn normalize_chatgpt_base_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if (normalized.starts_with("https://taidge.com")
        || normalized.starts_with("https://chat.openai.com"))
        && !normalized.contains("/backend-api")
    {
        normalized = format!("{normalized}/backend-api");
    }
    normalized
}

fn default_chatgpt_openai_base_url(chatgpt_base_url: Option<&str>) -> String {
    let configured = chatgpt_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CHATGPT_BASE_URL);
    let base = normalize_chatgpt_base_url(configured);
    if base.ends_with("/savfox") {
        return base;
    }
    if base.contains("/backend-api") {
        return format!("{base}/savfox");
    }
    if base.contains("/api/savfox") {
        return base;
    }
    format!("{base}/api/savfox")
}

pub const DEFAULT_LMSTUDIO_PORT: u16 = 1234;
pub const DEFAULT_OLLAMA_PORT: u16 = 11434;

pub const LMSTUDIO_OSS_PROVIDER_ID: &str = "lmstudio";
pub const OLLAMA_OSS_PROVIDER_ID: &str = "ollama";
pub const OLLAMA_CHAT_PROVIDER_ID: &str = "ollama-chat";

/// Built-in default provider list.
pub fn built_in_model_providers() -> HashMap<String, ModelProviderInfo> {
    use ModelProviderInfo as P;

    // We do not want to be in the business of adjucating which third-party
    // providers are bundled with Savfox CLI, so we only include the OpenAI and
    // open source ("oss") providers by default. Users are encouraged to add to
    // `model_providers` in config.toml to add their own providers.
    [
        ("openai", P::create_openai_provider()),
        (
            OLLAMA_OSS_PROVIDER_ID,
            create_oss_provider(DEFAULT_OLLAMA_PORT, WireApi::Responses),
        ),
        (
            OLLAMA_CHAT_PROVIDER_ID,
            create_oss_provider(DEFAULT_OLLAMA_PORT, WireApi::Chat),
        ),
        (
            LMSTUDIO_OSS_PROVIDER_ID,
            create_oss_provider(DEFAULT_LMSTUDIO_PORT, WireApi::Responses),
        ),
        ("anthropic", create_anthropic_provider()),
        (
            "groq",
            create_cloud_chat_provider("Groq", "https://api.groq.com/openai/v1", "GROQ_API_KEY"),
        ),
        (
            "xai",
            create_cloud_chat_provider("xAI", "https://api.x.ai/v1", "XAI_API_KEY"),
        ),
        (
            "deepseek",
            create_cloud_chat_provider(
                "DeepSeek",
                "https://api.deepseek.com/v1",
                "DEEPSEEK_API_KEY",
            ),
        ),
        (
            "mistral",
            create_cloud_chat_provider("Mistral", "https://api.mistral.ai/v1", "MISTRAL_API_KEY"),
        ),
        (
            "together",
            create_cloud_chat_provider(
                "Together",
                "https://api.together.xyz/v1",
                "TOGETHER_API_KEY",
            ),
        ),
        ("gemini", create_gemini_provider()),
        ("bedrock", create_bedrock_provider()),
        (
            "openrouter",
            create_cloud_chat_provider(
                "OpenRouter",
                "https://openrouter.ai/api/v1",
                "OPENROUTER_API_KEY",
            ),
        ),
        (
            "qwen",
            create_cloud_chat_provider(
                "Qwen",
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "DASHSCOPE_API_KEY",
            ),
        ),
        (
            "minimax",
            create_cloud_chat_provider("Minimax", "https://api.minimax.chat/v1", "MINIMAX_API_KEY"),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

pub fn create_oss_provider(default_provider_port: u16, wire_api: WireApi) -> ModelProviderInfo {
    // These SAVFOX_OSS_ environment variables are experimental: we may
    // switch to reading values from config.toml instead.
    let savfox_oss_base_url = match std::env::var("SAVFOX_OSS_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        Some(url) => url,
        None => format!(
            "http://localhost:{port}/v1",
            port = std::env::var("SAVFOX_OSS_PORT")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(default_provider_port)
        ),
    };
    create_oss_provider_with_base_url(&savfox_oss_base_url, wire_api)
}

fn create_anthropic_provider() -> ModelProviderInfo {
    ModelProviderInfo {
        name: "Anthropic".into(),
        base_url: Some("https://api.anthropic.com".into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Anthropic,
        query_params: None,
        http_headers: Some(
            [("anthropic-version".to_string(), "2023-06-01".to_string())]
                .into_iter()
                .collect(),
        ),
        env_http_headers: Some(
            [("x-api-key".to_string(), "ANTHROPIC_API_KEY".to_string())]
                .into_iter()
                .collect(),
        ),
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

fn create_cloud_chat_provider(name: &str, base_url: &str, env_key: &str) -> ModelProviderInfo {
    ModelProviderInfo {
        name: name.into(),
        base_url: Some(base_url.into()),
        env_key: Some(env_key.into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Chat,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

fn create_gemini_provider() -> ModelProviderInfo {
    // Google Gemini provides an OpenAI-compatible endpoint.
    // The API key is passed via the `key` query parameter OR via Bearer token.
    // We use the OpenAI-compatible endpoint which accepts Bearer auth.
    ModelProviderInfo {
        name: "Gemini".into(),
        base_url: Some(
            std::env::var("GEMINI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| {
                    "https://generativelanguage.googleapis.com/v1beta/openai".into()
                }),
        ),
        env_key: Some("GEMINI_API_KEY".into()),
        env_key_instructions: Some(
            "Get your Gemini API key from https://aistudio.google.com/apikey".into(),
        ),
        experimental_bearer_token: None,
        wire_api: WireApi::Chat,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

fn create_bedrock_provider() -> ModelProviderInfo {
    // AWS Bedrock provides an OpenAI-compatible proxy via the Bedrock Runtime
    // "Converse" API when accessed through an API Gateway or compatible proxy.
    // Users must set up their own Bedrock endpoint URL and provide credentials.
    //
    // For direct Bedrock access, users typically deploy a proxy like
    // `bedrock-access-gateway` or use LiteLLM as a middleware.
    ModelProviderInfo {
        name: "Bedrock".into(),
        base_url: std::env::var("BEDROCK_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        env_key: Some("BEDROCK_API_KEY".into()),
        env_key_instructions: Some(
            "Set BEDROCK_BASE_URL to your Bedrock-compatible proxy endpoint. \
             Set BEDROCK_API_KEY to the proxy's API key. \
             For direct AWS access, deploy bedrock-access-gateway or use LiteLLM."
                .into(),
        ),
        experimental_bearer_token: None,
        wire_api: WireApi::Chat,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

pub fn create_oss_provider_with_base_url(base_url: &str, wire_api: WireApi) -> ModelProviderInfo {
    ModelProviderInfo {
        name: "gpt-oss".into(),
        base_url: Some(base_url.into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_deserialize_ollama_model_provider_toml() {
        let azure_provider_toml = r#"
name = "Ollama"
base_url = "http://localhost:11434/v1"
        "#;
        let expected_provider = ModelProviderInfo {
            name: "Ollama".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Chat,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
        assert_eq!(expected_provider, provider);
    }

    #[test]
    fn test_deserialize_azure_model_provider_toml() {
        let azure_provider_toml = r#"
name = "Azure"
base_url = "https://xxxxx.openai.azure.com/openai"
env_key = "AZURE_OPENAI_API_KEY"
query_params = { api-version = "2025-04-01-preview" }
        "#;
        let expected_provider = ModelProviderInfo {
            name: "Azure".into(),
            base_url: Some("https://xxxxx.openai.azure.com/openai".into()),
            env_key: Some("AZURE_OPENAI_API_KEY".into()),
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Chat,
            query_params: Some(maplit::hashmap! {
                "api-version".to_string() => "2025-04-01-preview".to_string(),
            }),
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
        assert_eq!(expected_provider, provider);
    }

    #[test]
    fn test_deserialize_anthropic_model_provider_toml() {
        let anthropic_provider_toml = r#"
name = "Anthropic"
base_url = "https://api.anthropic.com"
wire_api = "anthropic"
http_headers = { "anthropic-version" = "2023-06-01" }
env_http_headers = { "x-api-key" = "ANTHROPIC_API_KEY" }
        "#;
        let expected_provider = ModelProviderInfo {
            name: "Anthropic".into(),
            base_url: Some("https://api.anthropic.com".into()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Anthropic,
            query_params: None,
            http_headers: Some(maplit::hashmap! {
                "anthropic-version".to_string() => "2023-06-01".to_string(),
            }),
            env_http_headers: Some(maplit::hashmap! {
                "x-api-key".to_string() => "ANTHROPIC_API_KEY".to_string(),
            }),
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        let provider: ModelProviderInfo = toml::from_str(anthropic_provider_toml).unwrap();
        assert_eq!(expected_provider, provider);
    }

    #[test]
    fn test_deserialize_example_model_provider_toml() {
        let azure_provider_toml = r#"
name = "Example"
base_url = "https://example.com"
env_key = "API_KEY"
http_headers = { "X-Example-Header" = "example-value" }
env_http_headers = { "X-Example-Env-Header" = "EXAMPLE_ENV_VAR" }
        "#;
        let expected_provider = ModelProviderInfo {
            name: "Example".into(),
            base_url: Some("https://example.com".into()),
            env_key: Some("API_KEY".into()),
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Chat,
            query_params: None,
            http_headers: Some(maplit::hashmap! {
                "X-Example-Header".to_string() => "example-value".to_string(),
            }),
            env_http_headers: Some(maplit::hashmap! {
                "X-Example-Env-Header".to_string() => "EXAMPLE_ENV_VAR".to_string(),
            }),
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
        assert_eq!(expected_provider, provider);
    }

    #[test]
    fn built_in_providers_include_gemini_and_bedrock() {
        let providers = built_in_model_providers();
        assert!(providers.contains_key("gemini"), "gemini provider missing");
        assert!(
            providers.contains_key("bedrock"),
            "bedrock provider missing"
        );

        let gemini = &providers["gemini"];
        assert_eq!(gemini.name, "Gemini");
        assert_eq!(gemini.wire_api, WireApi::Chat);
        assert_eq!(gemini.env_key.as_deref(), Some("GEMINI_API_KEY"));
        assert!(
            gemini
                .base_url
                .as_deref()
                .unwrap()
                .contains("generativelanguage.googleapis.com")
        );

        let bedrock = &providers["bedrock"];
        assert_eq!(bedrock.name, "Bedrock");
        assert_eq!(bedrock.wire_api, WireApi::Chat);
        assert_eq!(bedrock.env_key.as_deref(), Some("BEDROCK_API_KEY"));
    }

    #[test]
    fn built_in_providers_all_have_names() {
        let providers = built_in_model_providers();
        for (id, provider) in &providers {
            assert!(!provider.name.is_empty(), "provider {id} has empty name");
        }
    }

    #[test]
    fn test_deserialize_gemini_model_provider_toml() {
        let gemini_toml = r#"
name = "Gemini"
base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
env_key = "GEMINI_API_KEY"
        "#;
        let provider: ModelProviderInfo = toml::from_str(gemini_toml).unwrap();
        assert_eq!(provider.name, "Gemini");
        assert_eq!(provider.wire_api, WireApi::Chat);
        assert_eq!(provider.env_key.as_deref(), Some("GEMINI_API_KEY"));
    }

    #[test]
    fn inject_provider_auth_overrides_reads_models_store_auth() {
        let tmp = tempdir().expect("temp savfox home");
        let models_dir = tmp.path().join("models");
        std::fs::create_dir_all(&models_dir).expect("create models dir");

        let provider_id = "zhipuai-test-provider";
        let env_key = "SAVFOX_TEST_ZHIPUAI_API_KEY";
        let api_key = "sk-test-zhipuai";

        // Ensure test starts from a clean state.
        remove_env_override(env_key);
        remove_bearer_token_override(provider_id);

        std::fs::write(
            models_dir.join(format!("{provider_id}.json")),
            format!(
                r#"{{
  "version": 2,
  "provider_id": "{provider_id}",
  "display_name": "ZhipuAI",
  "auth": {{
    "type": "api_key",
    "env_key": "{env_key}",
    "api_key": "{api_key}"
  }},
  "models": []
}}"#
            ),
        )
        .expect("write provider file");

        inject_provider_auth_overrides_from_store(tmp.path());

        let provider = ModelProviderInfo {
            name: "ZhipuAI".to_string(),
            base_url: Some("https://open.bigmodel.cn/api/paas/v4".to_string()),
            env_key: Some(env_key.to_string()),
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Chat,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };

        assert_eq!(
            provider.api_key().expect("api key resolves"),
            Some(api_key.to_string())
        );
        assert_eq!(
            get_bearer_token_override(provider_id),
            Some(api_key.to_string())
        );

        // Cleanup global override maps to avoid cross-test leakage.
        remove_env_override(env_key);
        remove_bearer_token_override(provider_id);
    }

    #[test]
    fn to_api_provider_derives_chatgpt_backend_models_base_url() {
        let provider = ModelProviderInfo::create_openai_provider();
        let api_provider = provider
            .to_api_provider(
                Some("openai"),
                Some(AuthMode::Chatgpt),
                Some("https://chatgpt.com/backend-api/"),
            )
            .expect("convert provider");
        assert_eq!(
            api_provider.base_url,
            "https://chatgpt.com/backend-api/savfox"
        );
    }

    #[test]
    fn to_api_provider_derives_savfox_api_models_base_url() {
        let provider = ModelProviderInfo::create_openai_provider();
        let api_provider = provider
            .to_api_provider(
                Some("openai"),
                Some(AuthMode::Chatgpt),
                Some("https://api.taidge.com"),
            )
            .expect("convert provider");
        assert_eq!(api_provider.base_url, "https://api.taidge.com/api/savfox");
    }

    #[test]
    fn to_api_provider_uses_known_provider_default_base_url_when_missing() {
        let provider = ModelProviderInfo {
            name: "Anthropic".to_string(),
            base_url: None,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Anthropic,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };
        let api_provider = provider
            .to_api_provider(Some("anthropic"), None, None)
            .expect("convert provider");
        assert_eq!(api_provider.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn to_api_provider_uses_known_provider_alias_default_base_url_when_missing() {
        let provider = ModelProviderInfo {
            name: "Qwen".to_string(),
            base_url: None,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Chat,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        };
        let api_provider = provider
            .to_api_provider(Some("alibaba"), None, None)
            .expect("convert provider");
        assert_eq!(
            api_provider.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn provider_default_base_url_covers_all_known_provider_ids() {
        let known_provider_ids = [
            "abacus",
            "aihubmix",
            "alibaba",
            "alibaba-cn",
            "amazon-bedrock",
            "anthropic",
            "azure",
            "azure-cognitive-services",
            "bailing",
            "baseten",
            "cerebras",
            "chutes",
            "cloudflare-ai-gateway",
            "cloudflare-workers-ai",
            "cohere",
            "cortecs",
            "deepinfra",
            "deepseek",
            "fastrouter",
            "fireworks-ai",
            "friendli",
            "github-copilot",
            "github-copilot-enterprise",
            "github-models",
            "gitlab",
            "google",
            "google-vertex",
            "google-vertex-anthropic",
            "groq",
            "helicone",
            "huggingface",
            "iflowcn",
            "inception",
            "inference",
            "io-net",
            "kimi-for-coding",
            "llama",
            "lmstudio",
            "lucidquery",
            "minimax",
            "minimax-cn",
            "mistral",
            "modelscope",
            "moonshotai",
            "moonshotai-cn",
            "morph",
            "nano-gpt",
            "nebius",
            "nvidia",
            "ollama",
            "ollama-chat",
            "ollama-cloud",
            "openai",
            "opencode",
            "openrouter",
            "other",
            "ovhcloud",
            "perplexity",
            "poe",
            "requesty",
            "sap-ai-core",
            "scaleway",
            "siliconflow",
            "siliconflow-cn",
            "togetherai",
            "upstage",
            "v0",
            "venice",
            "vercel",
            "vultr",
            "wandb",
            "xai",
            "xiaomi",
            "zai",
            "zai-coding-plan",
            "zenmux",
            "zhipuai",
            "zhipuai-coding-plan",
        ];

        for provider_id in known_provider_ids {
            assert!(
                provider_default_base_url(provider_id).is_some() || provider_id == "other",
                "expected default base URL mapping for known provider id {provider_id}"
            );
        }
    }
}
