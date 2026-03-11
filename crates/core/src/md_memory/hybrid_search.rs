//! Hybrid search combining keyword/text matching with vector similarity.
//!
//! Uses a weighted combination of BM25-style text scores and cosine
//! similarity from the vector store.

use super::entry::{MdMemoryEntry, MemoryLayer};
use super::search::search_memories;
use super::vector_store::VectorStore;

/// Configuration for hybrid search blending.
#[derive(Debug, Clone)]
pub struct HybridSearchConfig {
    /// Weight for text-based search score (0.0 to 1.0).
    pub text_weight: f32,
    /// Weight for vector similarity score (0.0 to 1.0).
    pub vector_weight: f32,
    /// Minimum combined score threshold.
    pub min_score: f32,
    /// Maximum results to return.
    pub max_results: usize,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            text_weight: 0.4,
            vector_weight: 0.6,
            min_score: 0.1,
            max_results: 20,
        }
    }
}

/// A hybrid search result combining text and vector scores.
#[derive(Debug, Clone)]
pub struct HybridResult {
    pub slug: String,
    pub layer: MemoryLayer,
    pub text_score: f32,
    pub vector_score: f32,
    pub combined_score: f32,
}

/// Perform hybrid search over memories using both text matching and vector similarity.
///
/// Steps:
/// 1. Text search using `search_memories()` to find keyword matches
/// 2. Vector search using the `VectorStore` for semantic similarity
/// 3. Merge and rank by weighted combination
pub fn hybrid_search(
    entries: &[MdMemoryEntry],
    vector_store: &VectorStore,
    query: &str,
    query_embedding: Option<&[f32]>,
    config: &HybridSearchConfig,
) -> Vec<HybridResult> {
    use std::collections::HashMap;

    let mut scores: HashMap<String, HybridResult> = HashMap::new();

    // Phase 1: Text-based search
    let text_results = search_memories(entries, query, config.max_results * 2);
    let text_count = text_results.len().max(1) as f32;

    for (rank, entry) in text_results.iter().enumerate() {
        let key = format!("{:?}/{}", entry.layer, entry.slug);
        // Simple rank-based score: higher rank = higher score
        let text_score = 1.0 - (rank as f32 / text_count);
        scores
            .entry(key)
            .and_modify(|r| r.text_score = text_score)
            .or_insert(HybridResult {
                slug: entry.slug.clone(),
                layer: entry.layer,
                text_score,
                vector_score: 0.0,
                combined_score: 0.0,
            });
    }

    // Phase 2: Vector similarity search
    if let Some(embedding) = query_embedding {
        let vector_results = vector_store.search(embedding, config.max_results * 2);
        for sim in vector_results {
            let key = format!("{}/{}", sim.layer, sim.slug);
            let layer = match sim.layer.as_str() {
                "global" => MemoryLayer::Global,
                "project" => MemoryLayer::Project,
                "agent" => MemoryLayer::Agent,
                _ => MemoryLayer::Session,
            };
            scores
                .entry(key)
                .and_modify(|r| r.vector_score = sim.score)
                .or_insert(HybridResult {
                    slug: sim.slug,
                    layer,
                    text_score: 0.0,
                    vector_score: sim.score,
                    combined_score: 0.0,
                });
        }
    }

    // Phase 3: Compute combined scores and filter
    let mut results: Vec<HybridResult> = scores
        .into_values()
        .map(|mut r| {
            r.combined_score =
                r.text_score * config.text_weight + r.vector_score * config.vector_weight;
            r
        })
        .filter(|r| r.combined_score >= config.min_score)
        .collect();

    results.sort_by(|a, b| {
        b.combined_score
            .partial_cmp(&a.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(config.max_results);
    results
}
