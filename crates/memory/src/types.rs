use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    pub id: String,
    pub source: MemorySource,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub metadata: MemoryMetadata,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub file_path: Option<String>,
    pub line_start: usize,
    pub line_end: usize,
    pub token_count: usize,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    File,
    Session,
    Qmd,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub chunk: MemoryChunk,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySyncProgress {
    pub total: usize,
    pub completed: usize,
    pub current_file: Option<String>,
    pub status: MemorySyncStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySyncStatus {
    Idle,
    Scanning,
    Indexing,
    Embedding,
    Complete,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub embedding_provider: EmbeddingProviderConfig,
    pub chunk_tokens: usize,
    pub chunk_overlap: usize,
    pub max_results: usize,
    pub index_session_transcripts: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            embedding_provider: EmbeddingProviderConfig::OpenAI {
                model: "text-embedding-3-small".to_owned(),
            },
            chunk_tokens: 500,
            chunk_overlap: 50,
            max_results: 10,
            index_session_transcripts: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum EmbeddingProviderConfig {
    OpenAI {
        model: String,
    },
    Voyage {
        model: String,
    },
    Gemini {
        model: String,
    },
    Ollama {
        model: String,
        base_url: String,
    },
    CustomHttp {
        model: String,
        endpoint: String,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        api_key_header: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexMeta {
    pub model: String,
    pub provider: String,
    pub chunk_tokens: usize,
    pub chunk_overlap: usize,
    pub vector_dims: usize,
    pub indexed_at: DateTime<Utc>,
}

pub const SNIPPET_MAX_CHARS: usize = 700;
pub const EMBEDDING_BATCH_MAX_TOKENS: usize = 8000;
pub const EMBEDDING_INDEX_CONCURRENCY: usize = 4;
pub const EMBEDDING_RETRY_MAX_ATTEMPTS: usize = 3;
pub const EMBEDDING_RETRY_BASE_DELAY_MS: u64 = 500;
pub const EMBEDDING_RETRY_MAX_DELAY_MS: u64 = 8000;
