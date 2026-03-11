use std::time::Duration;

use http::StatusCode;
use savfox_http_client::TransportError;
use thiserror::Error;

use crate::rate_limits::RateLimitError;

/// Error type for the Codex API client.
///
/// Represents errors that can occur when interacting with the Codex/Responses API,
/// including transport errors, API errors, and rate limiting.
///
/// # Relationship with SavfoxError
///
/// Many of these error variants map to similar errors in [`savfox_core::error::SavfoxError`]:
/// - `ContextWindowExceeded` → `SavfoxError::ContextWindowExceeded`
/// - `QuotaExceeded` → `SavfoxError::QuotaExceeded`
/// - `UsageNotIncluded` → `SavfoxError::UsageNotIncluded`
/// - `InvalidRequest` → `SavfoxError::InvalidRequest`
///
/// These conversions are typically handled at the session/agent layer.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Transport-layer error (network, TLS, etc.)
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// API returned an error status code with a message
    #[error("api error {status}: {message}")]
    Api { status: StatusCode, message: String },

    /// Error occurred while streaming the response
    #[error("stream error: {0}")]
    Stream(String),

    /// Model's context window was exceeded
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// User quota was exceeded
    #[error("quota exceeded")]
    QuotaExceeded,

    /// Usage not included in current plan
    #[error("usage not included")]
    UsageNotIncluded,

    /// Retryable error with optional retry delay
    #[error("retryable error: {message}")]
    Retryable {
        message: String,
        delay: Option<Duration>,
    },

    /// Rate limit was exceeded
    #[error("rate limit: {0}")]
    RateLimit(String),

    /// Invalid request parameters
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
}

impl From<RateLimitError> for ApiError {
    fn from(err: RateLimitError) -> Self {
        Self::RateLimit(err.to_string())
    }
}
