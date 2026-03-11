use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{EmbeddingError, EmbeddingProvider, EmbeddingResult};

pub struct CustomHttpEmbeddingProvider {
    client: Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    api_key_header: Option<String>,
    vector_dims: usize,
    max_input_tokens: usize,
}

impl CustomHttpEmbeddingProvider {
    pub fn new(
        endpoint: String,
        model: String,
        api_key: Option<String>,
        api_key_header: Option<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            endpoint,
            model,
            api_key,
            api_key_header,
            vector_dims: 1536,
            max_input_tokens: 8191,
        }
    }
}

#[derive(Serialize)]
struct CustomEmbeddingRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OpenAILikeEmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct OpenAILikeEmbeddingResponse {
    data: Vec<OpenAILikeEmbeddingData>,
    model: Option<String>,
    usage: Option<OpenAILikeUsage>,
}

#[derive(Deserialize)]
struct OpenAILikeUsage {
    total_tokens: Option<usize>,
}

#[async_trait]
impl EmbeddingProvider for CustomHttpEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
        let req = CustomEmbeddingRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
        };

        let mut builder = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&req);

        if let Some(key) = &self.api_key {
            let header_name = self
                .api_key_header
                .as_deref()
                .unwrap_or("Authorization")
                .to_string();
            let header_value = if header_name.eq_ignore_ascii_case("Authorization") {
                format!("Bearer {key}")
            } else {
                key.clone()
            };
            builder = builder.header(header_name, header_value);
        }

        let response = builder.send().await?;
        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(EmbeddingError::RateLimited {
                    retry_after_ms: 60000,
                });
            }
            return Err(EmbeddingError::ApiError {
                message: format!("HTTP {}: {}", status, body),
            });
        }

        let body: Value = response
            .json()
            .await
            .map_err(|e| EmbeddingError::InvalidResponse(e.to_string()))?;

        let parsed: OpenAILikeEmbeddingResponse = serde_json::from_value(body)
            .map_err(|e| EmbeddingError::InvalidResponse(e.to_string()))?;

        let mut embeddings = vec![Vec::new(); texts.len()];
        for item in parsed.data {
            if item.index < texts.len() {
                embeddings[item.index] = item.embedding;
            }
        }

        Ok(EmbeddingResult {
            embeddings,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
            total_tokens: parsed.usage.and_then(|u| u.total_tokens).unwrap_or(0),
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn vector_dims(&self) -> usize {
        self.vector_dims
    }

    fn max_input_tokens(&self) -> usize {
        self.max_input_tokens
    }

    fn provider_name(&self) -> &str {
        "custom_http"
    }
}
