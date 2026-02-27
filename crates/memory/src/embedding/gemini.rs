use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider, EmbeddingResult};

pub struct GeminiEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl GeminiEmbeddingProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }

    fn vector_dims_for_model(model: &str) -> usize {
        match model {
            "text-embedding-004" => 768,
            "embedding-001" => 768,
            _ => 768,
        }
    }

    fn max_tokens_for_model(_model: &str) -> usize {
        2048
    }
}

#[derive(Serialize)]
struct GeminiRequest {
    requests: Vec<GeminiRequestItem>,
}

#[derive(Serialize)]
struct GeminiRequestItem {
    model: String,
    content: GeminiContent,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiResponse {
    embeddings: Vec<GeminiEmbeddingData>,
}

#[derive(Deserialize)]
struct GeminiEmbeddingData {
    values: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
        let model_path = format!("models/{}", self.model);
        let requests: Vec<GeminiRequestItem> = texts
            .iter()
            .map(|text| GeminiRequestItem {
                model: model_path.clone(),
                content: GeminiContent {
                    parts: vec![GeminiPart { text: text.clone() }],
                },
            })
            .collect();

        let request = GeminiRequest { requests };

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/embeddings?key={}",
            self.api_key
        );

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
            if status.as_u16() == 429 {
                return Err(EmbeddingError::RateLimited {
                    retry_after_ms: 60000,
                });
            }
            return Err(EmbeddingError::ApiError {
                message: error_text,
            });
        }

        let result: GeminiResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::InvalidResponse(e.to_string()))?;

        let embeddings: Vec<Vec<f32>> = result.embeddings.into_iter().map(|e| e.values).collect();

        let total_tokens: usize = texts.iter().map(|t| t.len() / 4).sum();

        Ok(EmbeddingResult {
            embeddings,
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
        Self::max_tokens_for_model(&self.model)
    }
}
