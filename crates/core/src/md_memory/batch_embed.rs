//! Batch embedding generation for memory entries.
//!
//! Provides a helper to embed multiple memory entries in a single
//! batch request to an OpenAI-compatible embedding API.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Request body for the OpenAI embeddings API.
#[derive(Debug, Serialize)]
struct EmbeddingRequest {
    model: String,
    input: Vec<String>,
}

/// Response from the OpenAI embeddings API.
#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

/// Configuration for embedding generation.
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    /// Base URL for the embedding API (e.g., "https://api.openai.com/v1").
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Embedding model name (e.g., "text-embedding-3-small").
    pub model: String,
    /// Maximum number of texts per batch request.
    pub batch_size: usize,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "text-embedding-3-small".to_string(),
            batch_size: 100,
        }
    }
}

/// Result of a batch embedding operation.
#[derive(Debug)]
pub struct BatchEmbedResult {
    /// Map from input key to embedding vector.
    pub embeddings: HashMap<String, Vec<f32>>,
    /// Number of texts that failed to embed.
    pub errors: usize,
}

/// Generate embeddings for a batch of (key, text) pairs.
///
/// Texts are sent in chunks of `config.batch_size` to the embedding API.
/// Returns a map from key to embedding vector.
pub async fn batch_embed(
    texts: &[(String, String)],
    config: &EmbedConfig,
) -> Result<BatchEmbedResult, String> {
    let client = reqwest::Client::new();
    let mut result = BatchEmbedResult {
        embeddings: HashMap::new(),
        errors: 0,
    };

    for chunk in texts.chunks(config.batch_size) {
        let input: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
        let keys: Vec<&str> = chunk.iter().map(|(key, _)| key.as_str()).collect();

        let request = EmbeddingRequest {
            model: config.model.clone(),
            input,
        };

        let url = format!("{}/embeddings", config.base_url);
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("embedding request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            warn!(
                status = %status,
                body = %body,
                "embedding API returned error"
            );
            result.errors += chunk.len();
            continue;
        }

        let embed_response: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| format!("failed to parse embedding response: {e}"))?;

        for data in embed_response.data {
            if data.index < keys.len() {
                result
                    .embeddings
                    .insert(keys[data.index].to_string(), data.embedding);
            }
        }
    }

    Ok(result)
}

/// Prepare text for embedding by combining frontmatter metadata with body content.
/// Truncates to a reasonable length for the embedding model.
pub fn prepare_text_for_embedding(
    slug: &str,
    tags: &[String],
    body: &str,
    max_chars: usize,
) -> String {
    let tag_str = if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join(", "))
    };

    let header = format!("{slug}{tag_str}: ");
    let available = max_chars.saturating_sub(header.len());
    let body_truncated = if body.len() > available {
        &body[..available]
    } else {
        body
    };

    format!("{header}{body_truncated}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_text() {
        let text = prepare_text_for_embedding(
            "coding-style",
            &["rust".to_string(), "conventions".to_string()],
            "Use snake_case for variables",
            100,
        );
        assert!(text.contains("coding-style"));
        assert!(text.contains("rust, conventions"));
        assert!(text.contains("Use snake_case"));
    }

    #[test]
    fn test_prepare_text_truncation() {
        let long_body = "a".repeat(1000);
        let text = prepare_text_for_embedding("test", &[], &long_body, 50);
        assert!(text.len() <= 50);
    }
}
