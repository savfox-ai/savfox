use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider, EmbeddingResult};

pub struct OllamaEmbeddingProvider {
    client: Client,
    base_url: String,
    model: String,
}

impl OllamaEmbeddingProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
        }
    }

    fn vector_dims_for_model(model: &str) -> usize {
        match model {
            "nomic-embed-text" => 768,
            "mxbai-embed-large" => 1024,
            "all-minilm" => 384,
            _ => 768,
        }
    }
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OllamaResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
        let request = OllamaRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
        };

        let url = format!("{}/api/embed", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(EmbeddingError::ApiError {
                message: error_text,
            });
        }

        let result: OllamaResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::InvalidResponse(e.to_string()))?;

        let total_tokens: usize = texts.iter().map(|t| t.len() / 4).sum();

        Ok(EmbeddingResult {
            embeddings: result.embeddings,
            model: self.model.clone(),
            total_tokens,
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn vector_dims(&self) -> usize {
        Self::vector_dims_for_model(&self.model)
    }

    fn max_input_tokens(&self) -> usize {
        8192
    }
}
