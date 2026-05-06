//! Multi-provider streaming HTTP client for LLM APIs.
//!
//! `savfox-api-client` is the transport layer beneath the rest of the
//! Savfox engine. It speaks three wire dialects:
//!
//! * **OpenAI Chat Completions** (`chat/completions` endpoint, SSE stream of `data: ...` chunks).
//! * **OpenAI Responses** (`responses` endpoint, the newer streaming shape with reasoning +
//!   tool-use events).
//! * **Anthropic Messages** (`v1/messages` endpoint, SSE event namespace with `message_start`,
//!   `content_block_delta`, etc.).
//!
//! Each dialect lives in its own subtree under [`requests`] (request
//! builder + body serde shape) and [`sse`] (event-stream parser). The
//! dialects share an [`auth`] surface, an [`endpoint`] resolver, and an
//! [`error`] type so callers can write provider-agnostic code.
//!
//! # Streaming
//!
//! All endpoints return their results through `futures::Stream` of typed
//! events; the upstream `reqwest` connection is held open until the model
//! signals end-of-stream. Callers must drain or drop the stream promptly
//! so connection slots are not exhausted.

#![allow(missing_debug_implementations)]
#![allow(
    clippy::filter_map_next,
    clippy::needless_continue,
    clippy::ref_option,
    clippy::result_large_err,
    clippy::return_self_not_must_use,
    clippy::unused_self
)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

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
