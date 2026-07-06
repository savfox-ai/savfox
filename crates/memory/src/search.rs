use crate::embedding::EmbeddingProvider;
use crate::storage::VectorStorage;
use crate::types::{MemorySearchResult, SNIPPET_MAX_CHARS};

pub struct HybridSearch {
    vector_weight: f32,
    keyword_weight: f32,
}

impl HybridSearch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            vector_weight: 0.7,
            keyword_weight: 0.3,
        }
    }

    #[must_use]
    pub fn with_weights(vector_weight: f32, keyword_weight: f32) -> Self {
        Self {
            vector_weight,
            keyword_weight,
        }
    }

    pub async fn search(
        &self,
        storage: &VectorStorage,
        provider: &dyn EmbeddingProvider,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>, SearchError> {
        let embedding_result = provider
            .embed(&[query.to_owned()])
            .await
            .map_err(|e| SearchError::EmbeddingError(e.to_string()))?;

        let query_embedding = &embedding_result.embeddings[0];

        let vector_results = storage
            .search_similar(query_embedding, limit * 2)
            .await
            .map_err(|e| SearchError::StorageError(e.to_string()))?;

        let keyword_results = storage
            .search_fts(query, limit * 2)
            .await
            .map_err(|e| SearchError::StorageError(e.to_string()))?;

        let merged = self.merge_results(vector_results, keyword_results, limit);

        Ok(merged)
    }

    pub async fn search_vector_only(
        &self,
        storage: &VectorStorage,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>, SearchError> {
        let results = storage
            .search_similar(query_embedding, limit)
            .await
            .map_err(|e| SearchError::StorageError(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(chunk, score)| MemorySearchResult {
                snippet: create_snippet(&chunk.content),
                chunk,
                score,
            })
            .collect())
    }

    pub async fn search_keyword_only(
        &self,
        storage: &VectorStorage,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>, SearchError> {
        let results = storage
            .search_fts(query, limit)
            .await
            .map_err(|e| SearchError::StorageError(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|(chunk, score)| MemorySearchResult {
                snippet: create_snippet(&chunk.content),
                chunk,
                score,
            })
            .collect())
    }

    fn merge_results(
        &self,
        vector_results: Vec<(crate::types::MemoryChunk, f32)>,
        keyword_results: Vec<(crate::types::MemoryChunk, f32)>,
        limit: usize,
    ) -> Vec<MemorySearchResult> {
        use std::collections::HashMap;

        let mut scores: HashMap<String, f32> = HashMap::new();
        let mut chunks: HashMap<String, crate::types::MemoryChunk> = HashMap::new();

        let max_vector_score = vector_results
            .iter()
            .map(|(_, s)| *s)
            .fold(0.0_f32, f32::max)
            .max(0.001);

        let max_keyword_score = keyword_results
            .iter()
            .map(|(_, s)| *s)
            .fold(0.0_f32, f32::max)
            .max(0.001);

        for (chunk, score) in vector_results {
            let normalized = score / max_vector_score;
            *scores.entry(chunk.id.clone()).or_insert(0.0) += normalized * self.vector_weight;
            chunks.insert(chunk.id.clone(), chunk);
        }

        for (chunk, score) in keyword_results {
            let normalized = score / max_keyword_score;
            *scores.entry(chunk.id.clone()).or_insert(0.0) += normalized * self.keyword_weight;
            chunks.insert(chunk.id.clone(), chunk);
        }

        let mut results: Vec<_> = scores
            .into_iter()
            .filter_map(|(id, score)| {
                chunks.remove(&id).map(|chunk| MemorySearchResult {
                    snippet: create_snippet(&chunk.content),
                    chunk,
                    score,
                })
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        results
    }
}

impl Default for HybridSearch {
    fn default() -> Self {
        Self::new()
    }
}

fn create_snippet(content: &str) -> String {
    let content = content.trim();

    if content.len() <= SNIPPET_MAX_CHARS {
        return content.to_owned();
    }

    let snippet = &content[..SNIPPET_MAX_CHARS];

    if let Some(last_space) = snippet.rfind(' ') {
        format!("{}...", &snippet[..last_space])
    } else {
        format!("{snippet}...")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    #[error("Storage error: {0}")]
    StorageError(String),
}
