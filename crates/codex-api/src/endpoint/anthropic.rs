use std::sync::Arc;

use savfox_client::{HttpTransport, RequestCompression, RequestTelemetry};

use crate::auth::AuthProvider;
use crate::common::{Prompt as ApiPrompt, ResponseStream};
use crate::endpoint::streaming::StreamingClient;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::AnthropicRequestBuilder;
use crate::sse::anthropic::spawn_anthropic_stream;
use crate::telemetry::SseTelemetry;

pub struct AnthropicClient<T: HttpTransport, A: AuthProvider> {
    streaming: StreamingClient<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> AnthropicClient<T, A> {
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

    pub async fn stream_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        max_tokens: i64,
    ) -> Result<ResponseStream, ApiError> {
        let request = AnthropicRequestBuilder::new(
            model,
            &prompt.instructions,
            &prompt.input,
            &prompt.tools,
            max_tokens,
        )
        .build(self.streaming.provider())?;

        self.streaming
            .stream(
                "v1/messages",
                request.body,
                request.headers,
                RequestCompression::None,
                spawn_anthropic_stream,
                None,
            )
            .await
    }
}
