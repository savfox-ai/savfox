use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider, EmbeddingResult};

pub struct VoyageEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl VoyageEmbeddingProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }

    fn vector_dims_for_model(model: &str) -> usize {
        match model {
            "voyage-3" | "voyage-3-lite" => 1024,
            "voyage-large-2-instruct" => 1024,
            "voyage-code-3" => 1024,
            "voyage-finance-2" => 1024,
            "voyage-law-2" => 1024,
            "voyage-code-2" => 1536,
            _ => 1024,
        }
    }

    fn max_tokens_for_model(model: &str) -> usize {
        match model {
            "voyage-3" => 32000,
            "voyage-3-lite" => 32000,
            "voyage-large-2-instruct" => 16000,
            "voyage-code-3" => 32000,
            "voyage-finance-2" => 32000,
            "voyage-law-2" => 16000,
            "voyage-code-2" => 16000,
            _ => 120000,
        }
    }
}

#[derive(Serialize)]
struct VoyageRequest {
    model: String,
    input: Vec<String>,
    truncation: bool,
    input_type: String,
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageEmbeddingData>,
    model: String,
    usage: VoyageUsage,
}

#[derive(Deserialize)]
struct VoyageEmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct VoyageUsage {
    total_tokens: usize,
}

#[async_trait]
impl EmbeddingProvider for VoyageEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
        let request = VoyageRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
            truncation: true,
            input_type: "document".to_string(),
        };

        let response = self
            .client
            .post("https://api.voyageai.com/v1/embeddings")
            .header("Authorization", format!("bearer {}", self.api_key))
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

        let result: VoyageResponse = response
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

    fn provider_name(&self) -> &str {
        "voyage"
    }
}
