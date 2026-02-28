mod custom_http;
mod gemini;
mod ollama;
mod openai;
mod voyage;

use async_trait::async_trait;
pub use custom_http::CustomHttpEmbeddingProvider;
pub use gemini::GeminiEmbeddingProvider;
pub use ollama::OllamaEmbeddingProvider;
pub use openai::OpenAIEmbeddingProvider;
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use voyage::VoyageEmbeddingProvider;

use crate::types::EmbeddingProviderConfig;

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
}

#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    pub embeddings: Vec<Vec<f32>>,
    pub model: String,
    pub total_tokens: usize,
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError>;

    fn model_name(&self) -> &str;

    fn vector_dims(&self) -> usize;

    fn max_input_tokens(&self) -> usize;
}

pub fn create_embedding_provider(
    config: &EmbeddingProviderConfig,
    api_keys: &EmbeddingApiKeys,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    match config {
        EmbeddingProviderConfig::OpenAI { model } => {
            let key = api_keys
                .openai
                .as_ref()
                .ok_or_else(|| EmbeddingError::MissingApiKey("openai".to_string()))?;
            Ok(Box::new(OpenAIEmbeddingProvider::new(
                key.clone(),
                model.clone(),
            )))
        }
        EmbeddingProviderConfig::Voyage { model } => {
            let key = api_keys
                .voyage
                .as_ref()
                .ok_or_else(|| EmbeddingError::MissingApiKey("voyage".to_string()))?;
            Ok(Box::new(VoyageEmbeddingProvider::new(
                key.clone(),
                model.clone(),
            )))
        }
        EmbeddingProviderConfig::Gemini { model } => {
            let key = api_keys
                .gemini
                .as_ref()
                .ok_or_else(|| EmbeddingError::MissingApiKey("gemini".to_string()))?;
            Ok(Box::new(GeminiEmbeddingProvider::new(
                key.clone(),
                model.clone(),
            )))
        }
        EmbeddingProviderConfig::Ollama { model, base_url } => Ok(Box::new(
            OllamaEmbeddingProvider::new(base_url.clone(), model.clone()),
        )),
        EmbeddingProviderConfig::CustomHttp {
            model,
            endpoint,
            api_key,
            api_key_header,
        } => Ok(Box::new(CustomHttpEmbeddingProvider::new(
            endpoint.clone(),
            model.clone(),
            api_key.clone(),
            api_key_header.clone(),
        ))),
    }
}

#[derive(Debug, Clone, Default)]
pub struct EmbeddingApiKeys {
    pub openai: Option<String>,
    pub voyage: Option<String>,
    pub gemini: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatch {
    pub texts: Vec<String>,
    pub indices: Vec<usize>,
}

pub fn estimate_tokens(text: &str) -> usize {
    let bytes = text.len();
    bytes / 4 + 1
}

pub fn split_into_batches(texts: &[String], max_tokens_per_batch: usize) -> Vec<EmbeddingBatch> {
    let mut batches = Vec::new();
    let mut current_texts = Vec::new();
    let mut current_indices = Vec::new();
    let mut current_tokens = 0;

    for (idx, text) in texts.iter().enumerate() {
        let tokens = estimate_tokens(text);

        if current_tokens + tokens > max_tokens_per_batch && !current_texts.is_empty() {
            batches.push(EmbeddingBatch {
                texts: std::mem::take(&mut current_texts),
                indices: std::mem::take(&mut current_indices),
            });
            current_tokens = 0;
        }

        current_texts.push(text.clone());
        current_indices.push(idx);
        current_tokens += tokens;
    }

    if !current_texts.is_empty() {
        batches.push(EmbeddingBatch {
            texts: current_texts,
            indices: current_indices,
        });
    }

    batches
}
