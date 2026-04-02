//! Hybrid search combining BM25 text scoring with vector similarity.
//!
//! Uses Reciprocal Rank Fusion (RRF) to merge results from both ranking
//! signals into a single scored list.

use std::collections::HashMap;

use super::index::cosine_similarity;

/// Hybrid search engine that combines BM25 text scoring with vector cosine
/// similarity via Reciprocal Rank Fusion (RRF).
#[derive(Debug, Clone)]
pub struct HybridSearcher {
    bm25_weight: f32,
    vector_weight: f32,
    /// RRF constant `k` (default 60). Higher values smooth out rank differences.
    rrf_k: f32,
}

impl HybridSearcher {
    /// Create a new hybrid searcher.
    ///
    /// * `bm25_weight` -- Weight for the BM25 text search signal (e.g. 0.3).
    /// * `vector_weight` -- Weight for the vector similarity signal (e.g. 0.7).
    #[must_use] 
    pub fn new(bm25_weight: f32, vector_weight: f32) -> Self {
        Self {
            bm25_weight,
            vector_weight,
            rrf_k: 60.0,
        }
    }

    /// Run a hybrid search over the provided documents.
    ///
    /// * `query_text` -- Free-text query for BM25 scoring.
    /// * `query_embedding` -- Dense vector embedding of the query.
    /// * `documents` -- Slice of `(doc_id, doc_text)` pairs. The doc_text is used for BM25 scoring;
    ///   vector similarity uses the vectors that have been previously indexed (looked up by doc_id
    ///   via `doc_embeddings`).
    /// * `doc_embeddings` -- Map from doc_id to embedding vector for each document. Documents
    ///   without an embedding are scored by BM25 alone.
    /// * `top_k` -- Maximum number of results to return.
    ///
    /// Returns `(doc_id, combined_score)` pairs sorted by descending score.
    #[must_use] 
    pub fn search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        documents: &[(String, String)],
        doc_embeddings: &HashMap<String, Vec<f32>>,
        top_k: usize,
    ) -> Vec<(String, f32)> {
        if documents.is_empty() {
            return Vec::new();
        }

        // --- BM25 scoring ---
        let bm25_scores = bm25_score(query_text, documents);

        // --- Vector similarity scoring ---
        let mut vector_scores: Vec<(String, f32)> = documents
            .iter()
            .filter_map(|(id, _)| {
                doc_embeddings.get(id).map(|emb| {
                    let sim = cosine_similarity(query_embedding, emb);
                    (id.clone(), sim)
                })
            })
            .collect();

        // Sort each list descending by score to establish ranks.
        let mut bm25_ranked: Vec<(String, f32)> = bm25_scores;
        bm25_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        vector_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // --- Reciprocal Rank Fusion ---
        let mut combined: HashMap<String, f32> = HashMap::new();

        for (rank, (id, _score)) in bm25_ranked.iter().enumerate() {
            let rrf = self.bm25_weight / (self.rrf_k + (rank + 1) as f32);
            *combined.entry(id.clone()).or_insert(0.0) += rrf;
        }

        for (rank, (id, _score)) in vector_scores.iter().enumerate() {
            let rrf = self.vector_weight / (self.rrf_k + (rank + 1) as f32);
            *combined.entry(id.clone()).or_insert(0.0) += rrf;
        }

        let mut results: Vec<(String, f32)> = combined.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        results.truncate(top_k);
        results
    }
}

impl Default for HybridSearcher {
    fn default() -> Self {
        Self::new(0.3, 0.7)
    }
}

// ---------------------------------------------------------------------------
// BM25 scoring
// ---------------------------------------------------------------------------

/// BM25 parameters.
const BM25_K1: f32 = 1.2;
const BM25_B: f32 = 0.75;

/// Compute BM25 scores for each document against the given query.
///
/// Returns `(doc_id, score)` pairs (unordered).
fn bm25_score(query: &str, documents: &[(String, String)]) -> Vec<(String, f32)> {
    let query_terms: Vec<String> = tokenize(query);
    if query_terms.is_empty() {
        return documents.iter().map(|(id, _)| (id.clone(), 0.0)).collect();
    }

    let n = documents.len() as f32;

    // Pre-tokenize all documents.
    let doc_tokens: Vec<Vec<String>> = documents.iter().map(|(_, text)| tokenize(text)).collect();

    // Average document length.
    let avg_dl: f32 = if documents.is_empty() {
        1.0
    } else {
        doc_tokens.iter().map(|t| t.len() as f32).sum::<f32>() / n
    };

    // Document frequency for each query term.
    let mut df: HashMap<&str, f32> = HashMap::new();
    for term in &query_terms {
        let count = doc_tokens
            .iter()
            .filter(|tokens| tokens.iter().any(|t| t == term))
            .count() as f32;
        df.insert(term.as_str(), count);
    }

    documents
        .iter()
        .zip(doc_tokens.iter())
        .map(|((id, _), tokens)| {
            let dl = tokens.len() as f32;
            let score: f32 = query_terms
                .iter()
                .map(|term| {
                    let tf = tokens.iter().filter(|t| *t == term).count() as f32;
                    let doc_freq = df.get(term.as_str()).copied().unwrap_or(0.0);

                    // IDF: log((N - df + 0.5) / (df + 0.5) + 1)
                    let idf = ((n - doc_freq + 0.5) / (doc_freq + 0.5)).ln_1p();

                    // TF saturation: (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * dl / avgdl))
                    let tf_norm = (tf * (BM25_K1 + 1.0))
                        / BM25_K1.mul_add(1.0 - BM25_B + BM25_B * dl / avg_dl, tf);

                    idf * tf_norm
                })
                .sum();

            (id.clone(), score)
        })
        .collect()
}

/// Lowercase and split text into whitespace-delimited tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|s| {
            // Strip common punctuation from the edges of tokens.
            s.trim_matches(|c: char| c.is_ascii_punctuation()).to_owned()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_docs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect()
    }

    fn make_embeddings(pairs: &[(&str, &[f32])]) -> HashMap<String, Vec<f32>> {
        pairs
            .iter()
            .map(|(id, emb)| (id.to_string(), emb.to_vec()))
            .collect()
    }

    #[test]
    fn bm25_scores_relevant_doc_higher() {
        let docs = make_docs(&[
            (
                "rust",
                "Rust is a systems programming language focused on safety",
            ),
            (
                "python",
                "Python is a dynamic scripting language for data science",
            ),
        ]);
        let scores = bm25_score("rust safety", &docs);
        let rust_score = scores.iter().find(|(id, _)| id == "rust").unwrap().1;
        let python_score = scores.iter().find(|(id, _)| id == "python").unwrap().1;
        assert!(
            rust_score > python_score,
            "rust doc ({rust_score}) should score higher than python doc ({python_score})"
        );
    }

    #[test]
    fn bm25_empty_query_returns_zero_scores() {
        let docs = make_docs(&[("a", "hello world")]);
        let scores = bm25_score("", &docs);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].1, 0.0);
    }

    #[test]
    fn bm25_no_match_returns_zero() {
        let docs = make_docs(&[("a", "cats and dogs")]);
        let scores = bm25_score("quantum physics", &docs);
        assert_eq!(scores[0].1, 0.0);
    }

    #[test]
    fn bm25_term_frequency_matters() {
        let docs = make_docs(&[
            ("repeat", "rust rust rust rust rust"),
            ("once", "rust is great"),
        ]);
        let scores = bm25_score("rust", &docs);
        let repeat_score = scores.iter().find(|(id, _)| id == "repeat").unwrap().1;
        let once_score = scores.iter().find(|(id, _)| id == "once").unwrap().1;
        assert!(
            repeat_score > once_score,
            "document with more term occurrences ({repeat_score}) should score higher ({once_score})"
        );
    }

    #[test]
    fn hybrid_search_combines_both_signals() {
        let docs = make_docs(&[
            ("d1", "rust programming language"),
            ("d2", "python scripting language"),
            ("d3", "java enterprise language"),
        ]);

        // d2 has the best embedding match but worst text match for "rust".
        let embeddings = make_embeddings(&[
            ("d1", &[0.5, 0.5, 0.0]),
            ("d2", &[1.0, 0.0, 0.0]),
            ("d3", &[0.0, 0.0, 1.0]),
        ]);

        let searcher = HybridSearcher::new(0.5, 0.5);
        let query_emb = vec![1.0, 0.0, 0.0];
        let results = searcher.search("rust programming", &query_emb, &docs, &embeddings, 3);

        assert_eq!(results.len(), 3);
        // d1 should rank high due to strong text match, d2 due to strong vector match.
        let d1_score = results.iter().find(|(id, _)| id == "d1").unwrap().1;
        let d3_score = results.iter().find(|(id, _)| id == "d3").unwrap().1;
        assert!(
            d1_score > d3_score,
            "d1 ({d1_score}) should rank above d3 ({d3_score}) due to combined signals"
        );
    }

    #[test]
    fn hybrid_search_empty_documents() {
        let searcher = HybridSearcher::default();
        let results = searcher.search("query", &[1.0, 0.0], &[], &HashMap::new(), 10);
        assert!(results.is_empty());
    }

    #[test]
    fn hybrid_search_respects_top_k() {
        let docs = make_docs(&[
            ("a", "alpha beta gamma"),
            ("b", "alpha delta"),
            ("c", "epsilon zeta"),
            ("d", "alpha alpha alpha"),
        ]);
        let embeddings = make_embeddings(&[
            ("a", &[1.0, 0.0]),
            ("b", &[0.5, 0.5]),
            ("c", &[0.0, 1.0]),
            ("d", &[0.8, 0.2]),
        ]);

        let searcher = HybridSearcher::default();
        let results = searcher.search("alpha", &[1.0, 0.0], &docs, &embeddings, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn hybrid_search_vector_only_weight() {
        let docs = make_docs(&[
            ("a", "completely unrelated text"),
            ("b", "also unrelated words"),
        ]);
        let embeddings = make_embeddings(&[("a", &[1.0, 0.0]), ("b", &[0.0, 1.0])]);

        // With bm25_weight=0.0, only vector similarity matters.
        let searcher = HybridSearcher::new(0.0, 1.0);
        let results = searcher.search("anything", &[1.0, 0.0], &docs, &embeddings, 2);
        assert_eq!(results[0].0, "a");
    }

    #[test]
    fn hybrid_search_bm25_only_weight() {
        let docs = make_docs(&[("a", "rust rust rust"), ("b", "python python")]);
        let embeddings = make_embeddings(&[("a", &[0.0, 1.0]), ("b", &[1.0, 0.0])]);

        // With vector_weight=0.0, only BM25 matters.
        let searcher = HybridSearcher::new(1.0, 0.0);
        let results = searcher.search("rust", &[1.0, 0.0], &docs, &embeddings, 2);
        assert_eq!(results[0].0, "a");
    }

    #[test]
    fn default_weights() {
        let searcher = HybridSearcher::default();
        assert!((searcher.bm25_weight - 0.3).abs() < 1e-6);
        assert!((searcher.vector_weight - 0.7).abs() < 1e-6);
    }

    #[test]
    fn tokenize_handles_punctuation() {
        let tokens = tokenize("Hello, world! Rust's great.");
        assert_eq!(tokens, vec!["hello", "world", "rust's", "great"]);
    }

    #[test]
    fn tokenize_empty_string() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn bm25_idf_penalizes_common_terms() {
        // "language" appears in all docs, "rust" in only one.
        let docs = make_docs(&[
            ("d1", "rust language"),
            ("d2", "python language"),
            ("d3", "java language"),
        ]);
        let scores = bm25_score("rust language", &docs);
        let d1_score = scores.iter().find(|(id, _)| id == "d1").unwrap().1;
        let d2_score = scores.iter().find(|(id, _)| id == "d2").unwrap().1;
        // d1 has both terms, d2 only has the common "language" term.
        assert!(
            d1_score > d2_score,
            "d1 ({d1_score}) should score higher than d2 ({d2_score})"
        );
    }
}
