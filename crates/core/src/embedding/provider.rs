//! Embedding provider abstractions and concrete implementations.

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

/// Errors that can occur during embedding operations.
#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("provider returned an unexpected response: {0}")]
    InvalidResponse(String),

    #[error("embedding provider not implemented: {0}")]
    NotImplemented(String),
}

/// Trait for embedding providers.
///
/// Implementations generate dense vector embeddings from text inputs.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    /// Generate embeddings for a batch of texts.
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// Embedding dimension produced by this provider.
    fn dimensions(&self) -> usize;

    /// Human-readable provider name.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// OpenAI
// ---------------------------------------------------------------------------

/// Embedding provider that calls the OpenAI `/v1/embeddings` endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiEmbedding {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    dimensions: usize,
}

/// Response shape for the OpenAI embeddings API.
#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

impl OpenAiEmbedding {
    /// Create a new OpenAI embedding provider.
    ///
    /// * `api_key` -- Bearer token for the OpenAI API.
    /// * `model` -- Model identifier, e.g. `"text-embedding-3-small"`.
    /// * `dimensions` -- Expected embedding dimensions (e.g. 1536).
    /// * `base_url` -- Optional base URL override; defaults to `https://api.openai.com`.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        dimensions: usize,
        base_url: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_owned()),
            api_key: api_key.into(),
            model: model.into(),
            dimensions,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::InvalidResponse(format!(
                "OpenAI returned status {status}: {text}"
            )));
        }

        let parsed: OpenAiEmbeddingResponse = resp.json().await?;
        let embeddings: Vec<Vec<f32>> = parsed.data.into_iter().map(|d| d.embedding).collect();

        if embeddings.len() != texts.len() {
            return Err(EmbeddingError::InvalidResponse(format!(
                "expected {} embeddings, got {}",
                texts.len(),
                embeddings.len()
            )));
        }

        Ok(embeddings)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "openai"
    }
}

// ---------------------------------------------------------------------------
// Ollama
// ---------------------------------------------------------------------------

/// Embedding provider that calls the Ollama `/api/embed` endpoint.
#[derive(Debug, Clone)]
pub struct OllamaEmbedding {
    client: reqwest::Client,
    base_url: String,
    model: String,
    dimensions: usize,
}

/// Response shape for the Ollama embed API.
#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl OllamaEmbedding {
    /// Create a new Ollama embedding provider.
    ///
    /// * `model` -- Model name, e.g. `"nomic-embed-text"`.
    /// * `dimensions` -- Expected embedding dimensions.
    /// * `base_url` -- Optional base URL override; defaults to `http://127.0.0.1:11434`.
    pub fn new(model: impl Into<String>, dimensions: usize, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| "http://127.0.0.1:11434".to_owned()),
            model: model.into(),
            dimensions,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbedding {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let url = format!("{}/api/embed", self.base_url);
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let resp = self.client.post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::InvalidResponse(format!(
                "Ollama returned status {status}: {text}"
            )));
        }

        let parsed: OllamaEmbedResponse = resp.json().await?;

        if parsed.embeddings.len() != texts.len() {
            return Err(EmbeddingError::InvalidResponse(format!(
                "expected {} embeddings, got {}",
                texts.len(),
                parsed.embeddings.len()
            )));
        }

        Ok(parsed.embeddings)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "ollama"
    }
}

// ---------------------------------------------------------------------------
// Local (stub)
// ---------------------------------------------------------------------------

/// Stub embedding provider for future local model support.
#[derive(Debug, Clone)]
pub struct LocalEmbedding {
    dimensions: usize,
}

impl LocalEmbedding {
    /// Create a stub local embedding provider.
    #[must_use]
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

#[async_trait]
impl EmbeddingProvider for LocalEmbedding {
    async fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Err(EmbeddingError::NotImplemented(
            "local embedding model support is not yet available".to_owned(),
        ))
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "local"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_embedding_returns_not_implemented() {
        let provider = LocalEmbedding::new(384);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(provider.embed(&["hello"]));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, EmbeddingError::NotImplemented(_)),
            "expected NotImplemented, got: {err:?}"
        );
    }

    #[test]
    fn openai_provider_has_correct_metadata() {
        let provider = OpenAiEmbedding::new("test-key", "text-embedding-3-small", 1536, None);
        assert_eq!(provider.name(), "openai");
        assert_eq!(provider.dimensions(), 1536);
    }

    #[test]
    fn ollama_provider_has_correct_metadata() {
        let provider = OllamaEmbedding::new("nomic-embed-text", 768, None);
        assert_eq!(provider.name(), "ollama");
        assert_eq!(provider.dimensions(), 768);
    }

    #[test]
    fn openai_custom_base_url() {
        let provider = OpenAiEmbedding::new(
            "key",
            "model",
            1536,
            Some("https://custom.api.example.com".to_owned()),
        );
        assert_eq!(provider.base_url, "https://custom.api.example.com");
    }

    #[test]
    fn ollama_default_base_url() {
        let provider = OllamaEmbedding::new("model", 768, None);
        assert_eq!(provider.base_url, "http://127.0.0.1:11434");
    }
}
