#![allow(missing_debug_implementations)]

pub mod auth;
pub mod common;
pub mod endpoint;
pub mod error;
pub mod provider;
pub mod rate_limits;
pub mod requests;
pub mod sse;
pub mod telemetry;

pub use savfox_http_client::{RequestTelemetry, ReqwestTransport, TransportError};

pub use crate::auth::AuthProvider;
pub use crate::common::{
    CompactionInput, Prompt, ResponseAppendWsRequest, ResponseCreateWsRequest, ResponseEvent,
    ResponseStream, ResponsesApiRequest, create_text_param_for_request,
};
pub use crate::endpoint::anthropic::AnthropicClient;
pub use crate::endpoint::chat::{AggregateStreamExt, ChatClient};
pub use crate::endpoint::compact::CompactClient;
pub use crate::endpoint::models::ModelsClient;
pub use crate::endpoint::responses::{ResponsesClient, ResponsesOptions};
pub use crate::endpoint::responses_websocket::{
    ResponsesWebsocketClient, ResponsesWebsocketConnection,
};
pub use crate::error::ApiError;
pub use crate::provider::{Provider, WireApi, is_azure_responses_wire_base_url};
pub use crate::requests::headers::build_conversation_headers;
pub use crate::requests::{
    AnthropicRequest, AnthropicRequestBuilder, ChatRequest, ChatRequestBuilder, ResponsesRequest,
    ResponsesRequestBuilder,
};
pub use crate::sse::stream_from_fixture;
pub use crate::telemetry::{SseTelemetry, WebsocketTelemetry};
