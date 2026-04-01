use async_trait::async_trait;
use thiserror::Error;

/// Errors that can occur during embedding operations.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("API error: {message}")]
    ApiError { message: String },

    #[error("Rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Token limit exceeded: {tokens} > {limit}")]
    TokenLimitExceeded { tokens: usize, limit: usize },

    #[error("Missing API key for provider: {0}")]
    MissingApiKey(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result of a successful embedding request.
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    /// The generated embedding vectors, one per input text.
    pub embeddings: Vec<Vec<f32>>,
    /// The model identifier that produced the embeddings.
    pub model: String,
    /// Total token count consumed across all inputs.
    pub total_tokens: usize,
}

/// Common trait for all embedding providers.
///
/// Each provider (OpenAI, Voyage, Gemini, Ollama, custom HTTP) implements this
/// trait so that callers can work with any backend through a single interface.
/// Providers differ only in construction; the operations exposed here are
/// uniform.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate embeddings for the given texts.
    ///
    /// Returns one embedding vector per input string, in the same order.
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError>;

    /// The model identifier used for embeddings (e.g. `"text-embedding-3-small"`).
    fn model_name(&self) -> &str;

    /// The dimensionality of the embedding vectors produced by this provider.
    fn vector_dims(&self) -> usize;

    /// Maximum number of input tokens this provider accepts per request.
    fn max_input_tokens(&self) -> usize;

    /// Short identifier for the provider backend (e.g. `"openai"`, `"voyage"`).
    ///
    /// Used for metadata and logging rather than dispatch.
    fn provider_name(&self) -> &str;
}
