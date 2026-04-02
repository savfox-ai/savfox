//! In-memory vector index with cosine similarity search.

/// An in-memory vector index that stores (id, embedding) pairs and supports
/// cosine similarity search.
#[derive(Debug, Clone)]
pub struct VectorIndex {
    vectors: Vec<(String, Vec<f32>)>,
}

impl VectorIndex {
    /// Create a new, empty vector index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vectors: Vec::new(),
        }
    }

    /// Insert an embedding with the given id.
    ///
    /// If an entry with the same id already exists it is replaced.
    pub fn insert(&mut self, id: String, embedding: Vec<f32>) {
        // Replace existing entry with the same id.
        if let Some(pos) = self
            .vectors
            .iter()
            .position(|(existing, _)| existing == &id)
        {
            self.vectors[pos].1 = embedding;
        } else {
            self.vectors.push((id, embedding));
        }
    }

    /// Remove the entry with the given id, if present.
    pub fn remove(&mut self, id: &str) {
        self.vectors.retain(|(existing, _)| existing != id);
    }

    /// Search for the `top_k` most similar vectors to `query`, returning
    /// `(id, cosine_similarity)` pairs sorted by descending similarity.
    #[must_use]
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        let mut scored: Vec<(String, f32)> = self
            .vectors
            .iter()
            .map(|(id, vec)| {
                let sim = cosine_similarity(query, vec);
                (id.clone(), sim)
            })
            .collect();

        // Sort descending by similarity.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        scored.truncate(top_k);
        scored
    }

    /// Return the number of stored vectors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Returns `true` if the index contains no vectors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}

impl Default for VectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the cosine similarity between two vectors.
///
/// Returns 0.0 if either vector has zero magnitude.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors should have similarity ~1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-6,
            "orthogonal vectors should have similarity ~0.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "opposite vectors should have similarity ~-1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![0.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "zero vector should produce similarity 0.0");
    }

    #[test]
    fn cosine_similarity_known_value() {
        // a = [1, 0], b = [1, 1] => cos = 1 / (1 * sqrt(2)) = 0.7071...
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        let expected = 1.0_f32 / 2.0_f32.sqrt();
        assert!(
            (sim - expected).abs() < 1e-6,
            "expected {expected}, got {sim}"
        );
    }

    #[test]
    fn index_insert_and_search() {
        let mut idx = VectorIndex::new();
        idx.insert("a".to_string(), vec![1.0, 0.0, 0.0]);
        idx.insert("b".to_string(), vec![0.0, 1.0, 0.0]);
        idx.insert("c".to_string(), vec![1.0, 1.0, 0.0]);

        let results = idx.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        // "a" is identical to query, should be first.
        assert_eq!(results[0].0, "a");
        assert!((results[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn index_remove() {
        let mut idx = VectorIndex::new();
        idx.insert("a".to_string(), vec![1.0, 0.0]);
        idx.insert("b".to_string(), vec![0.0, 1.0]);
        assert_eq!(idx.len(), 2);

        idx.remove("a");
        assert_eq!(idx.len(), 1);

        let results = idx.search(&[1.0, 0.0], 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "b");
    }

    #[test]
    fn index_insert_replaces_duplicate_id() {
        let mut idx = VectorIndex::new();
        idx.insert("a".to_string(), vec![1.0, 0.0]);
        idx.insert("a".to_string(), vec![0.0, 1.0]);
        assert_eq!(idx.len(), 1);

        // After replacement, "a" should be [0, 1].
        let results = idx.search(&[0.0, 1.0], 1);
        assert_eq!(results[0].0, "a");
        assert!((results[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn index_search_top_k_limits_results() {
        let mut idx = VectorIndex::new();
        for i in 0..10 {
            idx.insert(format!("v{i}"), vec![i as f32, 0.0, 0.0]);
        }
        let results = idx.search(&[1.0, 0.0, 0.0], 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn index_is_empty() {
        let idx = VectorIndex::new();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn index_search_empty_returns_empty() {
        let idx = VectorIndex::new();
        let results = idx.search(&[1.0, 0.0], 10);
        assert!(results.is_empty());
    }
}
