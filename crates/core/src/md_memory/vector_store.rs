//! Simple in-memory vector store for memory entry embeddings.
//!
//! Stores (slug, layer, embedding_vector) tuples and supports
//! cosine-similarity nearest-neighbor search.

use std::collections::HashMap;

/// A single stored embedding.
#[derive(Debug, Clone)]
pub struct StoredEmbedding {
    pub slug: String,
    pub layer: String,
    pub vector: Vec<f32>,
}

/// In-memory vector store backed by a flat list of embeddings.
/// Uses brute-force cosine similarity for search (sufficient for
/// the typical memory corpus of <1000 entries).
#[derive(Debug, Default)]
pub struct VectorStore {
    entries: Vec<StoredEmbedding>,
    /// Index by composite key "layer/slug" for fast lookup/update.
    index: HashMap<String, usize>,
}

impl VectorStore {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update an embedding for the given (layer, slug) pair.
    pub fn upsert(&mut self, slug: &str, layer: &str, vector: Vec<f32>) {
        let key = format!("{layer}/{slug}");
        if let Some(&idx) = self.index.get(&key) {
            self.entries[idx].vector = vector;
        } else {
            let idx = self.entries.len();
            self.entries.push(StoredEmbedding {
                slug: slug.to_owned(),
                layer: layer.to_owned(),
                vector,
            });
            self.index.insert(key, idx);
        }
    }

    /// Remove an embedding by (layer, slug).
    pub fn remove(&mut self, slug: &str, layer: &str) -> bool {
        let key = format!("{layer}/{slug}");
        if let Some(idx) = self.index.remove(&key) {
            self.entries.swap_remove(idx);
            // Fix up the index for the swapped element.
            if idx < self.entries.len() {
                let swapped = &self.entries[idx];
                let swapped_key = format!("{}/{}", swapped.layer, swapped.slug);
                self.index.insert(swapped_key, idx);
            }
            true
        } else {
            false
        }
    }

    /// Return the number of stored embeddings.
    #[must_use] 
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the store is empty.
    #[must_use] 
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find the top-k most similar entries to the query vector.
    #[must_use] 
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<SimilarityResult> {
        let mut scored: Vec<SimilarityResult> = self
            .entries
            .iter()
            .map(|e| SimilarityResult {
                slug: e.slug.clone(),
                layer: e.layer.clone(),
                score: cosine_similarity(query, &e.vector),
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }

    /// Find entries above a similarity threshold.
    #[must_use] 
    pub fn search_with_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        max_results: usize,
    ) -> Vec<SimilarityResult> {
        let mut results = self.search(query, max_results);
        results.retain(|r| r.score >= threshold);
        results
    }
}

/// Result of a similarity search.
#[derive(Debug, Clone)]
pub struct SimilarityResult {
    pub slug: String,
    pub layer: String,
    pub score: f32,
}

/// Compute the cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upsert_and_search() {
        let mut store = VectorStore::new();
        store.upsert("note1", "global", vec![1.0, 0.0, 0.0]);
        store.upsert("note2", "project", vec![0.0, 1.0, 0.0]);
        store.upsert("note3", "global", vec![0.9, 0.1, 0.0]);

        let results = store.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].slug, "note1");
        assert!(results[0].score > 0.99);
    }

    #[test]
    fn test_remove() {
        let mut store = VectorStore::new();
        store.upsert("a", "global", vec![1.0, 0.0]);
        store.upsert("b", "global", vec![0.0, 1.0]);
        assert_eq!(store.len(), 2);

        assert!(store.remove("a", "global"));
        assert_eq!(store.len(), 1);
        assert!(!store.remove("a", "global"));
    }

    #[test]
    fn test_cosine_similarity() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert!(cosine_similarity(&[], &[]).abs() < 1e-6);
    }
}
