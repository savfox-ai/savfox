use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider, EmbeddingResult};

pub struct OpenAIEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAIEmbeddingProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }

    fn vector_dims_for_model(model: &str) -> usize {
        match model {
            "text-embedding-3-small" => 1536,
            "text-embedding-3-large" => 3072,
            "text-embedding-ada-002" => 1536,
            _ => 1536,
        }
    }

    fn max_tokens_for_model(model: &str) -> usize {
        match model {
            "text-embedding-3-small" => 8191,
            "text-embedding-3-large" => 8191,
            "text-embedding-ada-002" => 8191,
            _ => 8191,
        }
    }
}

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    input: Vec<String>,
    encoding_format: String,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    data: Vec<OpenAIEmbeddingData>,
    model: String,
    usage: OpenAIUsage,
}

#[derive(Deserialize)]
struct OpenAIEmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct OpenAIUsage {
    total_tokens: usize,
}

#[async_trait]
impl EmbeddingProvider for OpenAIEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
        let request = OpenAIRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
            encoding_format: "float".to_string(),
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            let error_text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(EmbeddingError::RateLimited {
                    retry_after_ms: 60000,
                });
            }
            return Err(EmbeddingError::ApiError {
                message: error_text,
            });
        }

        let result: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::InvalidResponse(e.to_string()))?;

        let mut embeddings = vec![Vec::new(); texts.len()];
        for item in result.data {
            if item.index < texts.len() {
                embeddings[item.index] = item.embedding;
            }
        }

        Ok(EmbeddingResult {
            embeddings,
            model: result.model,
            total_tokens: result.usage.total_tokens,
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn vector_dims(&self) -> usize {
        Self::vector_dims_for_model(&self.model)
    }

    fn max_input_tokens(&self) -> usize {
        Self::max_tokens_for_model(&self.model)
    }
}
