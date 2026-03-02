use std::sync::{Arc, OnceLock};

use http::HeaderMap;
use savfox_client::{HttpTransport, RequestCompression, RequestTelemetry};
use savfox_protocol::protocol::SessionSource;
use serde_json::Value;
use tracing::{debug, error, instrument};

use crate::auth::AuthProvider;
use crate::common::{Prompt as ApiPrompt, Reasoning, ResponseStream, TextControls};
use crate::endpoint::streaming::StreamingClient;
use crate::error::ApiError;
use crate::provider::{Provider, WireApi};
use crate::requests::responses::Compression;
use crate::requests::{ResponsesRequest, ResponsesRequestBuilder};
use crate::sse::spawn_response_stream;
use crate::telemetry::SseTelemetry;

pub struct ResponsesClient<T: HttpTransport, A: AuthProvider> {
    streaming: StreamingClient<T, A>,
}

#[derive(Default)]
pub struct ResponsesOptions {
    pub reasoning: Option<Reasoning>,
    pub include: Vec<String>,
    pub prompt_cache_key: Option<String>,
    pub text: Option<TextControls>,
    pub store_override: Option<bool>,
    pub conversation_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

impl<T: HttpTransport, A: AuthProvider> ResponsesClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            streaming: StreamingClient::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            streaming: self.streaming.with_telemetry(request, sse),
        }
    }

    pub async fn stream_request(
        &self,
        request: ResponsesRequest,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        self.stream(
            request.body,
            request.headers,
            request.compression,
            turn_state,
        )
        .await
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn stream_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            reasoning,
            include,
            prompt_cache_key,
            text,
            store_override,
            conversation_id,
            session_source,
            extra_headers,
            compression,
            turn_state,
        } = options;
        let has_bearer_token = self.streaming.has_bearer_auth();
        let has_account_id = self.streaming.has_account_id();
        let conversation_id_set = conversation_id.is_some();
        debug!(
            provider = %self.streaming.provider().slug,
            url = %self.streaming.provider().url_for_path(self.path()),
            model,
            input_items = prompt.input.len(),
            tool_count = prompt.tools.len(),
            conversation_id_set,
            has_bearer_token,
            has_account_id,
            "preparing responses stream request"
        );

        let request = ResponsesRequestBuilder::new(model, &prompt.instructions, &prompt.input)
            .tools(&prompt.tools)
            .parallel_tool_calls(prompt.parallel_tool_calls)
            .reasoning(reasoning)
            .include(include)
            .prompt_cache_key(prompt_cache_key)
            .text(text)
            .conversation(conversation_id)
            .session_source(session_source)
            .store_override(store_override)
            .extra_headers(extra_headers)
            .compression(compression)
            .build(self.streaming.provider())?;
        let request_header_keys = request
            .headers
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect::<Vec<_>>();

        let stream_result = self.stream_request(request, turn_state).await;
        if let Err(err) = &stream_result {
            error!(
                provider = %self.streaming.provider().slug,
                url = %self.streaming.provider().url_for_path(self.path()),
                model,
                input_items = prompt.input.len(),
                tool_count = prompt.tools.len(),
                conversation_id_set,
                has_bearer_token,
                has_account_id,
                request_headers = ?request_header_keys,
                error = %err,
                "responses stream request failed"
            );
        }
        stream_result
    }

    fn path(&self) -> &'static str {
        match self.streaming.provider().wire {
            WireApi::Responses | WireApi::Compact | WireApi::Anthropic => "responses",
            WireApi::Chat => "chat/completions",
        }
    }

    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        self.streaming
            .stream(
                self.path(),
                body,
                extra_headers,
                compression,
                spawn_response_stream,
                turn_state,
            )
            .await
    }
}
