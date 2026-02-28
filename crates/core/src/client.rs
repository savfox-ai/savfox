use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use eventsource_stream::{Event, EventStreamError};
use futures::StreamExt;
use http::{HeaderMap as ApiHeaderMap, HeaderValue, StatusCode as HttpStatusCode};
use reqwest::StatusCode;
use savfox_api::common::{Reasoning, ResponsesWsRequest};
use savfox_api::error::ApiError;
use savfox_api::requests::responses::Compression;
use savfox_api::{
    AggregateStreamExt, AnthropicClient as ApiAnthropicClient, ChatClient as ApiChatClient,
    CompactClient as ApiCompactClient, CompactionInput as ApiCompactionInput, Prompt as ApiPrompt,
    RequestTelemetry, ReqwestTransport, ResponseAppendWsRequest, ResponseCreateWsRequest,
    ResponseStream as ApiResponseStream, ResponsesClient as ApiResponsesClient,
    ResponsesOptions as ApiResponsesOptions,
    ResponsesWebsocketClient as ApiWebSocketResponsesClient,
    ResponsesWebsocketConnection as ApiWebSocketConnection, SseTelemetry, TransportError,
    WebsocketTelemetry, build_conversation_headers, create_text_param_for_request,
};
use savfox_otel::OtelManager;
use savfox_protocol::SessionId;
use savfox_protocol::config_types::{ReasoningSummary as ReasoningSummaryConfig, WebSearchMode};
use savfox_protocol::models::ResponseItem;
use savfox_protocol::openai_models::{
    ModelInfo, ReasoningEffort as ReasoningEffortConfig, ReasoningEffortPreset,
};
use savfox_protocol::protocol::SessionSource;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Error, Message};
use tracing::{debug, error, warn};

use crate::api_bridge::{CoreAuthProvider, auth_provider_from_auth, map_api_error};
use crate::auth::{RefreshTokenError, SavfoxAuth, UnauthorizedRecovery};
use crate::client_common::{Prompt, ResponseEvent, ResponseStream};
use crate::config::Config;
use crate::default_client::build_reqwest_client;
use crate::error::{Result, SavfoxError};
use crate::features::{FEATURES, Feature};
use crate::flags::SAVFOX_RS_SSE_FIXTURE;
use crate::model_provider_info::{ModelProviderInfo, WireApi};
use crate::tools::spec::{
    create_tools_json_for_anthropic_api, create_tools_json_for_chat_completions_api,
    create_tools_json_for_responses_api,
};
use crate::transport_manager::TransportManager;
use crate::turn_metadata::build_turn_metadata_header;
use crate::{AuthManager, request_model_for_provider};

pub const WEB_SEARCH_ELIGIBLE_HEADER: &str = "x-oai-web-search-eligible";
pub const X_SAVFOX_TURN_STATE_HEADER: &str = "x-savfox-turn-state";
pub const X_SAVFOX_TURN_METADATA_HEADER: &str = "x-savfox-turn-metadata";

#[derive(Debug, Default)]
struct TurnMetadataCache {
    cwd: Option<PathBuf>,
    header: Option<HeaderValue>,
}

#[derive(Debug)]
struct ModelClientState {
    config: Arc<Config>,
    auth_manager: Option<Arc<AuthManager>>,
    model_info: ModelInfo,
    otel_manager: OtelManager,
    provider: ModelProviderInfo,
    conversation_id: SessionId,
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
    session_source: SessionSource,
    transport_manager: TransportManager,
    turn_metadata_cache: Arc<RwLock<TurnMetadataCache>>,
}

#[derive(Debug, Clone)]
pub struct ModelClient {
    state: Arc<ModelClientState>,
}

pub struct ModelClientSession {
    state: Arc<ModelClientState>,
    connection: Option<ApiWebSocketConnection>,
    websocket_last_items: Vec<ResponseItem>,
    transport_manager: TransportManager,
    /// Turn state for sticky routing.
    ///
    /// This is an `OnceLock` that stores the turn state value received from the server
    /// on turn start via the `x-savfox-turn-state` response header. Once set, this value
    /// should be sent back to the server in the `x-savfox-turn-state` request header for
    /// all subsequent requests within the same turn to maintain sticky routing.
    ///
    /// This is a contract between the client and server: we receive it at turn start,
    /// keep sending it unchanged between turn requests (e.g., for retries, incremental
    /// appends, or continuation requests), and must not send it between different turns.
    turn_state: Arc<OnceLock<String>>,
}

#[allow(clippy::too_many_arguments)]
impl ModelClient {
    fn request_model_slug(&self) -> String {
        request_model_for_provider(
            &self.state.model_info.slug,
            &self.state.config.model_provider_id,
        )
    }

    pub fn new(
        config: Arc<Config>,
        auth_manager: Option<Arc<AuthManager>>,
        model_info: ModelInfo,
        otel_manager: OtelManager,
        provider: ModelProviderInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        conversation_id: SessionId,
        session_source: SessionSource,
        transport_manager: TransportManager,
    ) -> Self {
        Self {
            state: Arc::new(ModelClientState {
                config,
                auth_manager,
                model_info,
                otel_manager,
                provider,
                conversation_id,
                effort,
                summary,
                session_source,
                transport_manager,
                turn_metadata_cache: Arc::new(RwLock::new(TurnMetadataCache::default())),
            }),
        }
    }

    pub fn new_session(&self, turn_metadata_cwd: Option<PathBuf>) -> ModelClientSession {
        self.prewarm_turn_metadata_header(turn_metadata_cwd);
        ModelClientSession {
            state: Arc::clone(&self.state),
            connection: None,
            websocket_last_items: Vec::new(),
            transport_manager: self.state.transport_manager.clone(),
            turn_state: Arc::new(OnceLock::new()),
        }
    }

    /// Refresh turn metadata in the background and update a cached header that request
    /// builders can read without blocking.
    fn prewarm_turn_metadata_header(&self, turn_metadata_cwd: Option<PathBuf>) {
        let turn_metadata_cwd =
            turn_metadata_cwd.map(|cwd| std::fs::canonicalize(&cwd).unwrap_or(cwd));

        if let Ok(mut cache) = self.state.turn_metadata_cache.write()
            && cache.cwd != turn_metadata_cwd
        {
            cache.cwd = turn_metadata_cwd.clone();
            cache.header = None;
        }

        let Some(cwd) = turn_metadata_cwd else {
            return;
        };
        let turn_metadata_cache = Arc::clone(&self.state.turn_metadata_cache);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let _task = handle.spawn(async move {
                let header = build_turn_metadata_header(cwd.as_path())
                    .await
                    .and_then(|value| HeaderValue::from_str(value.as_str()).ok());

                if let Ok(mut cache) = turn_metadata_cache.write()
                    && cache.cwd.as_ref() == Some(&cwd)
                {
                    cache.header = header;
                }
            });
        }
    }
}

impl ModelClient {
    pub fn get_model_context_window(&self) -> Option<i64> {
        let model_info = &self.state.model_info;
        let effective_context_window_percent = model_info.effective_context_window_percent;
        model_info.context_window.map(|context_window| {
            context_window.saturating_mul(effective_context_window_percent) / 100
        })
    }

    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.state.config)
    }

    pub fn provider(&self) -> &ModelProviderInfo {
        &self.state.provider
    }

    pub fn get_provider(&self) -> ModelProviderInfo {
        self.state.provider.clone()
    }

    pub fn get_otel_manager(&self) -> OtelManager {
        self.state.otel_manager.clone()
    }

    pub fn get_session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub(crate) fn transport_manager(&self) -> TransportManager {
        self.state.transport_manager.clone()
    }

    /// Returns the currently configured model slug.
    pub fn get_model(&self) -> String {
        self.state.model_info.slug.clone()
    }

    pub fn get_model_info(&self) -> ModelInfo {
        self.state.model_info.clone()
    }

    /// Returns the current reasoning effort setting.
    pub fn get_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.state.effort
    }

    /// Returns the current reasoning summary setting.
    pub fn get_reasoning_summary(&self) -> ReasoningSummaryConfig {
        self.state.summary
    }

    pub fn get_auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.state.auth_manager.clone()
    }

    /// Compacts the current conversation history using the Compact endpoint.
    ///
    /// This is a unary call (no streaming) that returns a new list of
    /// `ResponseItem`s representing the compacted transcript.
    pub async fn compact_conversation_history(&self, prompt: &Prompt) -> Result<Vec<ResponseItem>> {
        if prompt.input.is_empty() {
            return Ok(Vec::new());
        }
        let auth_manager = self.state.auth_manager.clone();
        let auth = match auth_manager.as_ref() {
            Some(manager) => manager.auth().await,
            None => None,
        };
        let api_provider = self
            .state
            .provider
            .to_api_provider(auth.as_ref().map(SavfoxAuth::internal_auth_mode))?;
        let api_auth = auth_provider_from_auth(
            auth.clone(),
            &self.state.provider,
            &self.state.config.model_provider_id,
        )?;
        let transport = ReqwestTransport::new(build_reqwest_client());
        let request_telemetry = self.build_request_telemetry();
        let client = ApiCompactClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry));

        let instructions = prompt.base_instructions.text.clone();
        let request_model = self.request_model_slug();
        let payload = ApiCompactionInput {
            model: request_model.as_str(),
            input: &prompt.input,
            instructions: &instructions,
        };

        let mut extra_headers = ApiHeaderMap::new();
        if let SessionSource::SubAgent(sub) = &self.state.session_source {
            let subagent = match sub {
                crate::protocol::SubAgentSource::Review => "review".to_string(),
                crate::protocol::SubAgentSource::Compact => "compact".to_string(),
                crate::protocol::SubAgentSource::SessionSpawn { .. } => "collab_spawn".to_string(),
                crate::protocol::SubAgentSource::Other(label) => label.clone(),
            };
            if let Ok(val) = HeaderValue::from_str(&subagent) {
                extra_headers.insert("x-openai-subagent", val);
            }
        }
        client
            .compact_input(&payload, extra_headers)
            .await
            .map_err(map_api_error)
    }
}

impl ModelClientSession {
    fn request_model_slug(&self) -> String {
        request_model_for_provider(
            &self.state.model_info.slug,
            &self.state.config.model_provider_id,
        )
    }

    fn turn_metadata_header(&self) -> Option<HeaderValue> {
        self.state
            .turn_metadata_cache
            .try_read()
            .ok()
            .and_then(|cache| cache.header.clone())
    }

    /// Streams a single model turn using either the Responses or Chat
    /// Completions wire API, depending on the configured provider.
    ///
    /// For Chat providers, the underlying stream is optionally aggregated
    /// based on the `show_raw_agent_reasoning` flag in the config.
    pub async fn stream(&mut self, prompt: &Prompt) -> Result<ResponseStream> {
        let wire_api = self.state.provider.wire_api;
        match wire_api {
            WireApi::Responses => {
                let websocket_enabled = self.responses_websocket_enabled()
                    && !self.transport_manager.disable_websockets();

                let responses_result = if websocket_enabled {
                    self.stream_responses_websocket(prompt).await
                } else {
                    self.stream_responses_api(prompt).await
                };

                match responses_result {
                    Ok(stream) => Ok(stream),
                    Err(err) if self.should_fallback_to_chat_wire(prompt, &err) => {
                        warn!(
                            provider = %self.state.provider.name,
                            model = %self.state.model_info.slug,
                            error = %err,
                            "responses wire appears unsupported, falling back to chat/completions for this turn"
                        );
                        self.stream_chat_wire(prompt).await
                    }
                    Err(err) => Err(err),
                }
            }
            WireApi::Chat => self.stream_chat_wire(prompt).await,
            WireApi::Anthropic => {
                let api_stream = self.stream_anthropic_messages(prompt).await?;
                Ok(map_response_stream(
                    api_stream,
                    self.state.otel_manager.clone(),
                ))
            }
        }
    }

    pub(crate) fn try_switch_fallback_transport(&mut self) -> bool {
        let websocket_enabled = self.responses_websocket_enabled();
        let activated = self
            .transport_manager
            .activate_http_fallback(websocket_enabled);
        if activated {
            warn!("falling back to HTTP");
            self.state.otel_manager.counter(
                "savfox.transport.fallback_to_http",
                1,
                &[("from_wire_api", "responses_websocket")],
            );

            self.connection = None;
            self.websocket_last_items.clear();
        }
        activated
    }

    fn responses_websocket_enabled(&self) -> bool {
        self.state.provider.supports_websockets
            && self
                .state
                .config
                .features
                .enabled(Feature::ResponsesWebsockets)
    }

    fn build_responses_request(&self, prompt: &Prompt) -> Result<ApiPrompt> {
        let instructions = prompt.base_instructions.text.clone();
        let tools_json: Vec<Value> = create_tools_json_for_responses_api(&prompt.tools)?;
        Ok(build_api_prompt(prompt, instructions, tools_json))
    }

    fn build_responses_options(
        &self,
        prompt: &Prompt,
        compression: Compression,
    ) -> ApiResponsesOptions {
        let turn_metadata_header = self.turn_metadata_header();
        let model_info = &self.state.model_info;

        let default_reasoning_effort = model_info.default_reasoning_level;
        let requested_effort = self.state.effort.or(default_reasoning_effort);
        let normalized_effort =
            normalize_reasoning_effort(requested_effort, &model_info.supported_reasoning_levels);
        if requested_effort != normalized_effort {
            warn!(
                model = %model_info.slug,
                requested = ?requested_effort,
                normalized = ?normalized_effort,
                "reasoning effort normalized to supported levels"
            );
        }
        let reasoning = if model_info.supports_reasoning_summaries {
            Some(Reasoning {
                effort: normalized_effort,
                summary: if self.state.summary == ReasoningSummaryConfig::None {
                    None
                } else {
                    Some(self.state.summary)
                },
            })
        } else {
            None
        };

        let include = if reasoning.is_some() {
            vec!["reasoning.encrypted_content".to_string()]
        } else {
            Vec::new()
        };

        let verbosity = if model_info.support_verbosity {
            self.state
                .config
                .model_verbosity
                .or(model_info.default_verbosity)
        } else {
            if self.state.config.model_verbosity.is_some() {
                warn!(
                    "model_verbosity is set but ignored as the model does not support verbosity: {}",
                    model_info.slug
                );
            }
            None
        };

        let text = create_text_param_for_request(verbosity, &prompt.output_schema);
        let conversation_id = self.state.conversation_id.to_string();

        ApiResponsesOptions {
            reasoning,
            include,
            prompt_cache_key: Some(conversation_id.clone()),
            text,
            store_override: None,
            conversation_id: Some(conversation_id),
            session_source: Some(self.state.session_source.clone()),
            extra_headers: build_responses_headers(
                &self.state.config,
                Some(&self.turn_state),
                turn_metadata_header.as_ref(),
            ),
            compression,
            turn_state: Some(Arc::clone(&self.turn_state)),
        }
    }

    fn get_incremental_items(&self, input_items: &[ResponseItem]) -> Option<Vec<ResponseItem>> {
        // Checks whether the current request input is an incremental append to the previous
        // request. If items in the new request contain all the items from the previous
        // request we build a response.append request otherwise we start with a fresh
        // response.create request.
        let previous_len = self.websocket_last_items.len();
        let can_append = previous_len > 0
            && input_items.starts_with(&self.websocket_last_items)
            && previous_len < input_items.len();
        if can_append {
            Some(input_items[previous_len..].to_vec())
        } else {
            None
        }
    }

    async fn stream_chat_wire(&self, prompt: &Prompt) -> Result<ResponseStream> {
        let api_stream = self.stream_chat_completions(prompt).await?;

        if self.state.config.show_raw_agent_reasoning {
            Ok(map_response_stream(
                api_stream.streaming_mode(),
                self.state.otel_manager.clone(),
            ))
        } else {
            Ok(map_response_stream(
                api_stream.aggregate(),
                self.state.otel_manager.clone(),
            ))
        }
    }

    fn should_fallback_to_chat_wire(&self, prompt: &Prompt, err: &SavfoxError) -> bool {
        if prompt.output_schema.is_some() {
            return false;
        }

        responses_wire_unsupported(err)
    }

    fn prepare_websocket_request(
        &self,
        api_prompt: &ApiPrompt,
        options: &ApiResponsesOptions,
    ) -> ResponsesWsRequest {
        if let Some(append_items) = self.get_incremental_items(&api_prompt.input) {
            return ResponsesWsRequest::ResponseAppend(ResponseAppendWsRequest {
                input: append_items,
            });
        }

        let ApiResponsesOptions {
            reasoning,
            include,
            prompt_cache_key,
            text,
            store_override,
            ..
        } = options;

        let store = store_override.unwrap_or(false);
        let payload = ResponseCreateWsRequest {
            model: self.request_model_slug(),
            instructions: api_prompt.instructions.clone(),
            input: api_prompt.input.clone(),
            tools: api_prompt.tools.clone(),
            tool_choice: "auto".to_string(),
            parallel_tool_calls: api_prompt.parallel_tool_calls,
            reasoning: reasoning.clone(),
            store,
            stream: true,
            include: include.clone(),
            prompt_cache_key: prompt_cache_key.clone(),
            text: text.clone(),
        };

        ResponsesWsRequest::ResponseCreate(payload)
    }

    fn log_responses_request_debug(
        &self,
        transport: &'static str,
        api_provider: &savfox_api::Provider,
        api_auth: &CoreAuthProvider,
        api_prompt: &ApiPrompt,
        options: &ApiResponsesOptions,
    ) {
        let request_model = self.request_model_slug();
        let extra_header_keys = options
            .extra_headers
            .keys()
            .map(http::header::HeaderName::as_str)
            .collect::<Vec<_>>();

        debug!(
            transport,
            provider = %api_provider.name,
            url = %api_provider.url_for_path("responses"),
            model = %request_model,
            input_items = api_prompt.input.len(),
            tool_count = api_prompt.tools.len(),
            conversation_id_set = options.conversation_id.is_some(),
            has_turn_state_header = options.extra_headers.contains_key(X_SAVFOX_TURN_STATE_HEADER),
            has_bearer_token = api_auth.has_bearer_token(),
            has_account_id = api_auth.has_account_id(),
            extra_headers = ?extra_header_keys,
            "sending responses request"
        );
    }

    fn log_websocket_session_input_debug(&self, request: &ResponsesWsRequest) {
        match request {
            ResponsesWsRequest::ResponseCreate(payload) => {
                debug!(
                    request_type = "response.create",
                    model = %payload.model,
                    input_items = payload.input.len(),
                    tool_count = payload.tools.len(),
                    include_count = payload.include.len(),
                    has_prompt_cache_key = payload.prompt_cache_key.is_some(),
                    "sending websocket session input payload"
                );
            }
            ResponsesWsRequest::ResponseAppend(payload) => {
                debug!(
                    request_type = "response.append",
                    input_items = payload.input.len(),
                    "sending websocket session input payload"
                );
            }
        }
    }

    async fn websocket_connection(
        &mut self,
        api_provider: savfox_api::Provider,
        api_auth: CoreAuthProvider,
        options: &ApiResponsesOptions,
    ) -> std::result::Result<&ApiWebSocketConnection, ApiError> {
        let needs_new = match self.connection.as_ref() {
            Some(conn) => conn.is_closed().await,
            None => true,
        };

        if needs_new {
            let mut headers = options.extra_headers.clone();
            headers.extend(build_conversation_headers(options.conversation_id.clone()));
            let websocket_telemetry = self.build_websocket_telemetry();
            let new_conn: ApiWebSocketConnection =
                ApiWebSocketResponsesClient::new(api_provider, api_auth)
                    .connect(
                        headers,
                        options.turn_state.clone(),
                        Some(websocket_telemetry),
                    )
                    .await?;
            self.connection = Some(new_conn);
        }

        self.connection.as_ref().ok_or(ApiError::Stream(
            "websocket connection is unavailable".to_string(),
        ))
    }

    fn responses_request_compression(&self, auth: Option<&crate::auth::SavfoxAuth>) -> Compression {
        if self
            .state
            .config
            .features
            .enabled(Feature::EnableRequestCompression)
            && auth.is_some_and(SavfoxAuth::is_chatgpt_auth)
            && self.state.provider.is_openai()
        {
            Compression::Zstd
        } else {
            Compression::None
        }
    }

    /// Streams a turn via the OpenAI Chat Completions API.
    ///
    /// This path is only used when the provider is configured with
    /// `WireApi::Chat`; it does not support `output_schema` today.
    async fn stream_chat_completions(&self, prompt: &Prompt) -> Result<ApiResponseStream> {
        if prompt.output_schema.is_some() {
            return Err(SavfoxError::UnsupportedOperation(
                "output_schema is not supported for Chat Completions API".to_string(),
            ));
        }

        let auth_manager = self.state.auth_manager.clone();
        let request_model = self.request_model_slug();
        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_chat_completions_api(&prompt.tools)?;
        let api_prompt = build_api_prompt(prompt, instructions, tools_json);
        let conversation_id = self.state.conversation_id.to_string();
        let session_source = self.state.session_source.clone();

        let mut auth_recovery = auth_manager
            .as_ref()
            .map(super::auth::AuthManager::unauthorized_recovery);
        loop {
            let auth = match auth_manager.as_ref() {
                Some(manager) => manager.auth().await,
                None => None,
            };
            let api_provider = self
                .state
                .provider
                .to_api_provider(auth.as_ref().map(SavfoxAuth::internal_auth_mode))?;
            let api_auth = auth_provider_from_auth(
                auth.clone(),
                &self.state.provider,
                &self.state.config.model_provider_id,
            )?;
            let transport = ReqwestTransport::new(build_reqwest_client());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let client = ApiChatClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            let stream_result = client
                .stream_prompt(
                    request_model.as_str(),
                    &api_prompt,
                    Some(conversation_id.clone()),
                    Some(session_source.clone()),
                )
                .await;

            match stream_result {
                Ok(stream) => return Ok(stream),
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(map_api_error(err)),
            }
        }
    }

    /// Streams a turn via the Anthropic Messages API.
    ///
    /// This path is only used when the provider is configured with
    /// `WireApi::Anthropic`.
    async fn stream_anthropic_messages(&self, prompt: &Prompt) -> Result<ApiResponseStream> {
        if prompt.output_schema.is_some() {
            return Err(SavfoxError::UnsupportedOperation(
                "output_schema is not supported for Anthropic Messages API".to_string(),
            ));
        }

        let auth_manager = self.state.auth_manager.clone();
        let request_model = self.request_model_slug();
        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_anthropic_api(&prompt.tools)?;
        let api_prompt = build_api_prompt(prompt, instructions, tools_json);

        // Anthropic requires max_tokens on every request.
        let max_tokens = self
            .state
            .model_info
            .context_window
            .map(|cw| (cw / 4).max(4096))
            .unwrap_or(8192);

        let mut auth_recovery = auth_manager
            .as_ref()
            .map(super::auth::AuthManager::unauthorized_recovery);
        loop {
            let auth = match auth_manager.as_ref() {
                Some(manager) => manager.auth().await,
                None => None,
            };
            let api_provider = self
                .state
                .provider
                .to_api_provider(auth.as_ref().map(SavfoxAuth::internal_auth_mode))?;
            let api_auth = auth_provider_from_auth(
                auth.clone(),
                &self.state.provider,
                &self.state.config.model_provider_id,
            )?;
            let transport = ReqwestTransport::new(build_reqwest_client());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let client = ApiAnthropicClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            let stream_result = client
                .stream_prompt(request_model.as_str(), &api_prompt, max_tokens)
                .await;

            match stream_result {
                Ok(stream) => return Ok(stream),
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(map_api_error(err)),
            }
        }
    }

    /// Streams a turn via the OpenAI Responses API.
    ///
    /// Handles SSE fixtures, reasoning summaries, verbosity, and the
    /// `text` controls used for output schemas.
    async fn stream_responses_api(&self, prompt: &Prompt) -> Result<ResponseStream> {
        if let Some(path) = &*SAVFOX_RS_SSE_FIXTURE {
            warn!(path, "Streaming from fixture");
            let stream =
                savfox_api::stream_from_fixture(path, self.state.provider.stream_idle_timeout())
                    .map_err(map_api_error)?;
            return Ok(map_response_stream(stream, self.state.otel_manager.clone()));
        }

        let auth_manager = self.state.auth_manager.clone();
        let request_model = self.request_model_slug();
        let api_prompt = self.build_responses_request(prompt)?;

        let mut auth_recovery = auth_manager
            .as_ref()
            .map(super::auth::AuthManager::unauthorized_recovery);
        loop {
            let auth = match auth_manager.as_ref() {
                Some(manager) => manager.auth().await,
                None => None,
            };
            let api_provider = self
                .state
                .provider
                .to_api_provider(auth.as_ref().map(SavfoxAuth::internal_auth_mode))?;
            let api_auth = auth_provider_from_auth(
                auth.clone(),
                &self.state.provider,
                &self.state.config.model_provider_id,
            )?;
            let transport = ReqwestTransport::new(build_reqwest_client());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let compression = self.responses_request_compression(auth.as_ref());
            let has_bearer_token = api_auth.has_bearer_token();
            let has_account_id = api_auth.has_account_id();
            let options = self.build_responses_options(prompt, compression);
            self.log_responses_request_debug(
                "responses_http",
                &api_provider,
                &api_auth,
                &api_prompt,
                &options,
            );

            let client = ApiResponsesClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            let stream_result = client
                .stream_prompt(request_model.as_str(), &api_prompt, options)
                .await;

            match stream_result {
                Ok(stream) => {
                    return Ok(map_response_stream(stream, self.state.otel_manager.clone()));
                }
                Err(err @ ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    error!(
                        transport = "responses_http",
                        provider = %self.state.provider.name,
                        model = %request_model,
                        input_items = api_prompt.input.len(),
                        has_bearer_token,
                        has_account_id,
                        error = %err,
                        "responses request unauthorized"
                    );
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(map_api_error(err)),
            }
        }
    }

    /// Streams a turn via the Responses API over WebSocket transport.
    async fn stream_responses_websocket(&mut self, prompt: &Prompt) -> Result<ResponseStream> {
        let auth_manager = self.state.auth_manager.clone();
        let request_model = self.request_model_slug();
        let api_prompt = self.build_responses_request(prompt)?;

        let mut auth_recovery = auth_manager
            .as_ref()
            .map(super::auth::AuthManager::unauthorized_recovery);
        loop {
            let auth = match auth_manager.as_ref() {
                Some(manager) => manager.auth().await,
                None => None,
            };
            let api_provider = self
                .state
                .provider
                .to_api_provider(auth.as_ref().map(SavfoxAuth::internal_auth_mode))?;
            let api_auth = auth_provider_from_auth(
                auth.clone(),
                &self.state.provider,
                &self.state.config.model_provider_id,
            )?;
            let compression = self.responses_request_compression(auth.as_ref());

            let options = self.build_responses_options(prompt, compression);
            let request = self.prepare_websocket_request(&api_prompt, &options);
            self.log_websocket_session_input_debug(&request);
            self.log_responses_request_debug(
                "responses_websocket",
                &api_provider,
                &api_auth,
                &api_prompt,
                &options,
            );

            let connection = match self
                .websocket_connection(api_provider.clone(), api_auth.clone(), &options)
                .await
            {
                Ok(connection) => connection,
                Err(err @ ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    error!(
                        transport = "responses_websocket",
                        provider = %self.state.provider.name,
                        model = %request_model,
                        input_items = api_prompt.input.len(),
                        has_bearer_token = api_auth.has_bearer_token(),
                        error = %err,
                        "responses websocket connection unauthorized"
                    );
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(map_api_error(err)),
            };

            let stream_result = connection
                .stream_request(request)
                .await
                .map_err(map_api_error)?;
            self.websocket_last_items = api_prompt.input.clone();

            return Ok(map_response_stream(
                stream_result,
                self.state.otel_manager.clone(),
            ));
        }
    }

    /// Builds request and SSE telemetry for streaming API calls (Chat/Responses).
    fn build_streaming_telemetry(&self) -> (Arc<dyn RequestTelemetry>, Arc<dyn SseTelemetry>) {
        let telemetry = Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()));
        let request_telemetry: Arc<dyn RequestTelemetry> = telemetry.clone();
        let sse_telemetry: Arc<dyn SseTelemetry> = telemetry;
        (request_telemetry, sse_telemetry)
    }

    /// Builds telemetry for the Responses API WebSocket transport.
    fn build_websocket_telemetry(&self) -> Arc<dyn WebsocketTelemetry> {
        let telemetry = Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()));
        let websocket_telemetry: Arc<dyn WebsocketTelemetry> = telemetry;
        websocket_telemetry
    }
}

impl ModelClient {
    /// Builds request telemetry for unary API calls (e.g., Compact endpoint).
    fn build_request_telemetry(&self) -> Arc<dyn RequestTelemetry> {
        let telemetry = Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()));
        let request_telemetry: Arc<dyn RequestTelemetry> = telemetry;
        request_telemetry
    }
}

/// Adapts the core `Prompt` type into the `savfox-api` payload shape.
fn build_api_prompt(prompt: &Prompt, instructions: String, tools_json: Vec<Value>) -> ApiPrompt {
    ApiPrompt {
        instructions,
        input: prompt.get_formatted_input(),
        tools: tools_json,
        parallel_tool_calls: prompt.parallel_tool_calls,
        output_schema: prompt.output_schema.clone(),
    }
}

fn normalize_reasoning_effort(
    requested: Option<ReasoningEffortConfig>,
    supported: &[ReasoningEffortPreset],
) -> Option<ReasoningEffortConfig> {
    let requested = requested?;
    if supported.is_empty() {
        return Some(requested);
    }
    if supported.iter().any(|preset| preset.effort == requested) {
        return Some(requested);
    }

    // Binary-mode providers expose both `none` and one non-none level:
    // any non-off request should map to the supported non-none level.
    let non_none_supported: Vec<ReasoningEffortConfig> = supported
        .iter()
        .map(|preset| preset.effort)
        .filter(|effort| *effort != ReasoningEffortConfig::None)
        .collect();
    let candidate_levels: Vec<ReasoningEffortConfig> =
        if requested != ReasoningEffortConfig::None && !non_none_supported.is_empty() {
            non_none_supported
        } else {
            supported.iter().map(|preset| preset.effort).collect()
        };

    nearest_reasoning_effort(requested, &candidate_levels).or(Some(requested))
}

fn nearest_reasoning_effort(
    requested: ReasoningEffortConfig,
    candidates: &[ReasoningEffortConfig],
) -> Option<ReasoningEffortConfig> {
    let requested_rank = reasoning_effort_rank(requested);
    let mut best: Option<(ReasoningEffortConfig, u8, u8)> = None;
    for candidate in candidates {
        let candidate_rank = reasoning_effort_rank(*candidate);
        let distance = requested_rank.abs_diff(candidate_rank);
        match best {
            None => best = Some((*candidate, distance, candidate_rank)),
            Some((_, best_distance, best_rank)) => {
                if distance < best_distance
                    || (distance == best_distance && candidate_rank < best_rank)
                {
                    best = Some((*candidate, distance, candidate_rank));
                }
            }
        }
    }
    best.map(|(candidate, ..)| candidate)
}

fn reasoning_effort_rank(effort: ReasoningEffortConfig) -> u8 {
    match effort {
        ReasoningEffortConfig::None => 0,
        ReasoningEffortConfig::Minimal => 1,
        ReasoningEffortConfig::Low => 2,
        ReasoningEffortConfig::Medium => 3,
        ReasoningEffortConfig::High => 4,
        ReasoningEffortConfig::XHigh => 5,
    }
}

fn experimental_feature_headers(config: &Config) -> ApiHeaderMap {
    let enabled = FEATURES
        .iter()
        .filter_map(|spec| {
            if spec.stage.experimental_menu_description().is_some()
                && config.features.enabled(spec.id)
            {
                Some(spec.key)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let value = enabled.join(",");
    let mut headers = ApiHeaderMap::new();
    if !value.is_empty()
        && let Ok(header_value) = HeaderValue::from_str(value.as_str())
    {
        headers.insert("x-savfox-beta-features", header_value);
    }
    headers
}

fn build_responses_headers(
    config: &Config,
    turn_state: Option<&Arc<OnceLock<String>>>,
    turn_metadata_header: Option<&HeaderValue>,
) -> ApiHeaderMap {
    let mut headers = experimental_feature_headers(config);
    headers.insert(
        WEB_SEARCH_ELIGIBLE_HEADER,
        HeaderValue::from_static(
            if matches!(config.web_search_mode, Some(WebSearchMode::Disabled)) {
                "false"
            } else {
                "true"
            },
        ),
    );
    if let Some(turn_state) = turn_state
        && let Some(state) = turn_state.get()
        && let Ok(header_value) = HeaderValue::from_str(state)
    {
        headers.insert(X_SAVFOX_TURN_STATE_HEADER, header_value);
    }
    if let Some(header_value) = turn_metadata_header {
        headers.insert(X_SAVFOX_TURN_METADATA_HEADER, header_value.clone());
    }
    headers
}

fn map_response_stream<S>(api_stream: S, otel_manager: OtelManager) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, ApiError>>
        + Unpin
        + Send
        + 'static,
{
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

    tokio::spawn(async move {
        let mut logged_error = false;
        let mut api_stream = api_stream;
        while let Some(event) = api_stream.next().await {
            match event {
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }) => {
                    if let Some(usage) = &token_usage {
                        otel_manager.sse_event_completed(
                            usage.input_tokens,
                            usage.output_tokens,
                            Some(usage.cached_input_tokens),
                            Some(usage.reasoning_output_tokens),
                            usage.total_tokens,
                        );
                    }
                    if tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(event) => {
                    if tx_event.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let mapped = map_api_error(err);
                    if !logged_error {
                        otel_manager.see_event_completed_failed(&mapped);
                        logged_error = true;
                    }
                    if tx_event.send(Err(mapped)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    ResponseStream { rx_event }
}

fn responses_wire_unsupported(err: &SavfoxError) -> bool {
    match err {
        SavfoxError::UnexpectedStatus(unexpected) => matches!(
            unexpected.status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use savfox_protocol::openai_models::{ReasoningEffort, ReasoningEffortPreset};

    use super::{normalize_reasoning_effort, responses_wire_unsupported};
    use crate::error::{SavfoxError, UnexpectedResponseError};

    fn preset(effort: ReasoningEffort) -> ReasoningEffortPreset {
        ReasoningEffortPreset {
            effort,
            description: effort.to_string(),
        }
    }

    #[test]
    fn normalize_reasoning_effort_binary_mode_maps_non_off_to_on() {
        let supported = vec![preset(ReasoningEffort::None), preset(ReasoningEffort::High)];
        let normalized = normalize_reasoning_effort(Some(ReasoningEffort::Minimal), &supported);
        assert_eq!(normalized, Some(ReasoningEffort::High));
    }

    #[test]
    fn normalize_reasoning_effort_binary_mode_keeps_off() {
        let supported = vec![preset(ReasoningEffort::None), preset(ReasoningEffort::High)];
        let normalized = normalize_reasoning_effort(Some(ReasoningEffort::None), &supported);
        assert_eq!(normalized, Some(ReasoningEffort::None));
    }

    #[test]
    fn normalize_reasoning_effort_chooses_nearest_supported_level() {
        let supported = vec![preset(ReasoningEffort::Low), preset(ReasoningEffort::High)];
        let normalized = normalize_reasoning_effort(Some(ReasoningEffort::XHigh), &supported);
        assert_eq!(normalized, Some(ReasoningEffort::High));
    }

    #[test]
    fn responses_wire_unsupported_for_endpoint_statuses() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_IMPLEMENTED,
        ] {
            let err = SavfoxError::UnexpectedStatus(UnexpectedResponseError {
                status,
                body: "unsupported".to_string(),
                url: Some("http://example.test/v1/responses".to_string()),
                request_id: None,
            });
            assert!(
                responses_wire_unsupported(&err),
                "status {status} should trigger chat fallback"
            );
        }
    }

    #[test]
    fn responses_wire_unsupported_ignores_auth_and_validation_errors() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::BAD_REQUEST,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let err = SavfoxError::UnexpectedStatus(UnexpectedResponseError {
                status,
                body: "unexpected".to_string(),
                url: Some("http://example.test/v1/responses".to_string()),
                request_id: None,
            });
            assert!(
                !responses_wire_unsupported(&err),
                "status {status} should not trigger chat fallback"
            );
        }
    }
}

/// Handles a 401 response by optionally refreshing ChatGPT tokens once.
///
/// When refresh succeeds, the caller should retry the API call; otherwise
/// the mapped `SavfoxError` is returned to the caller.
async fn handle_unauthorized(
    status: StatusCode,
    auth_recovery: &mut Option<UnauthorizedRecovery>,
) -> Result<()> {
    if let Some(recovery) = auth_recovery
        && recovery.has_next()
    {
        return match recovery.next().await {
            Ok(_) => Ok(()),
            Err(RefreshTokenError::Permanent(failed)) => {
                Err(SavfoxError::RefreshTokenFailed(failed))
            }
            Err(RefreshTokenError::Transient(other)) => Err(SavfoxError::Io(other)),
        };
    }

    Err(map_unauthorized_status(status))
}

fn map_unauthorized_status(status: StatusCode) -> SavfoxError {
    map_api_error(ApiError::Transport(TransportError::Http {
        status,
        url: None,
        headers: None,
        body: None,
    }))
}

struct ApiTelemetry {
    otel_manager: OtelManager,
}

impl ApiTelemetry {
    fn new(otel_manager: OtelManager) -> Self {
        Self { otel_manager }
    }
}

impl RequestTelemetry for ApiTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<HttpStatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let error_message = error.map(std::string::ToString::to_string);
        self.otel_manager.record_api_request(
            attempt,
            status.map(|s| s.as_u16()),
            error_message.as_deref(),
            duration,
        );
    }
}

impl SseTelemetry for ApiTelemetry {
    fn on_sse_poll(
        &self,
        result: &std::result::Result<
            Option<std::result::Result<Event, EventStreamError<TransportError>>>,
            tokio::time::error::Elapsed,
        >,
        duration: Duration,
    ) {
        self.otel_manager.log_sse_event(result, duration);
    }
}

impl WebsocketTelemetry for ApiTelemetry {
    fn on_ws_request(&self, duration: Duration, error: Option<&ApiError>) {
        let error_message = error.map(std::string::ToString::to_string);
        self.otel_manager
            .record_websocket_request(duration, error_message.as_deref());
    }

    fn on_ws_event(
        &self,
        result: &std::result::Result<Option<std::result::Result<Message, Error>>, ApiError>,
        duration: Duration,
    ) {
        self.otel_manager.record_websocket_event(result, duration);
    }
}
