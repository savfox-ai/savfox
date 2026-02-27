//! Embedding and hybrid search module.
//!
//! Provides vector embedding providers, an in-memory vector index, and a hybrid
//! search engine that combines BM25 text scoring with vector similarity.

pub mod hybrid;
pub mod index;
pub mod provider;

pub use hybrid::HybridSearcher;
pub use index::VectorIndex;
pub use provider::{EmbeddingProvider, LocalEmbedding, OllamaEmbedding, OpenAiEmbedding};
