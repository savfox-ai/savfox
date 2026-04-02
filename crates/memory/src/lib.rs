#![allow(missing_debug_implementations)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod embedding;
pub mod manager;
pub mod qmd;
pub mod search;
pub mod storage;
pub mod sync;
pub mod types;

pub use embedding::{EmbeddingError, EmbeddingProvider, EmbeddingResult};
pub use manager::MemoryManager;
pub use types::{MemoryChunk, MemorySearchResult, MemorySource};
