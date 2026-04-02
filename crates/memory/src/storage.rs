use std::path::Path;

use sqlx::SqlitePool;
use sqlx::migrate::MigrateDatabase;
use tracing::info;

use crate::types::{MemoryChunk, MemoryIndexMeta, MemoryMetadata, MemorySource};

pub struct VectorStorage {
    pool: SqlitePool,
    vector_dims: usize,
}

impl VectorStorage {
    pub async fn new(path: &Path, vector_dims: usize) -> Result<Self, sqlx::Error> {
        let db_url = if path.to_str() == Some(":memory:") {
            "sqlite::memory:".to_owned()
        } else {
            format!("sqlite:{}", path.to_string_lossy())
        };

        if !sqlx::Sqlite::database_exists(&db_url)
            .await
            .unwrap_or(false)
        {
            sqlx::Sqlite::create_database(&db_url).await?;
        }

        let pool = SqlitePool::connect(&db_url).await?;

        let storage = Self { pool, vector_dims };
        storage.initialize().await?;
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS chunks (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                content TEXT NOT NULL,
                file_path TEXT,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                token_count INTEGER NOT NULL,
                hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS embeddings (
                chunk_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS index_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(hash)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_file_path ON chunks(file_path)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_updated_at ON chunks(updated_at)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_chunks_created_at ON chunks(created_at)")
            .execute(&self.pool)
            .await?;

        info!("Vector storage initialized with {} dims", self.vector_dims);
        Ok(())
    }

    pub async fn insert_chunk(&self, chunk: &MemoryChunk) -> Result<(), sqlx::Error> {
        let source_str = match chunk.source {
            MemorySource::File => "file",
            MemorySource::Session => "session",
            MemorySource::Qmd => "qmd",
            MemorySource::Manual => "manual",
        };

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO chunks 
            (id, source, content, file_path, line_start, line_end, token_count, hash, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#
        )
        .bind(&chunk.id)
        .bind(source_str)
        .bind(&chunk.content)
        .bind(&chunk.metadata.file_path)
        .bind(chunk.metadata.line_start as i32)
        .bind(chunk.metadata.line_end as i32)
        .bind(chunk.metadata.token_count as i32)
        .bind(&chunk.metadata.hash)
        .bind(chunk.created_at.to_rfc3339())
        .bind(chunk.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        if let Some(ref embedding) = chunk.embedding {
            let embedding_bytes = Self::embedding_to_bytes(embedding);
            sqlx::query("INSERT OR REPLACE INTO embeddings (chunk_id, embedding) VALUES (?1, ?2)")
                .bind(&chunk.id)
                .bind(&embedding_bytes)
                .execute(&self.pool)
                .await?;
        }

        Ok(())
    }

    pub async fn delete_chunk(&self, id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM chunks WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_chunks_by_file(&self, file_path: &str) -> Result<usize, sqlx::Error> {
        let result = sqlx::query("DELETE FROM chunks WHERE file_path = ?1")
            .bind(file_path)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() as usize)
    }

    pub async fn get_chunk(&self, id: &str) -> Result<Option<MemoryChunk>, sqlx::Error> {
        let row = sqlx::query_as::<_, ChunkRow>(
            "SELECT id, source, content, file_path, line_start, line_end, token_count, hash, created_at, updated_at FROM chunks WHERE id = ?1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.into_chunk()))
    }

    pub async fn get_embedding(&self, chunk_id: &str) -> Result<Option<Vec<f32>>, sqlx::Error> {
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT embedding FROM embeddings WHERE chunk_id = ?1")
                .bind(chunk_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(bytes,)| Self::bytes_to_embedding(&bytes)))
    }

    pub async fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ChunkWithEmbeddingRow>(
            r#"
            SELECT c.id, c.source, c.content, c.file_path, c.line_start, c.line_end, 
                   c.token_count, c.hash, c.created_at, c.updated_at, e.embedding
            FROM chunks c
            JOIN embeddings e ON c.id = e.chunk_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut results: Vec<(MemoryChunk, f32)> = rows
            .into_iter()
            .map(|row| {
                let chunk = row.into_chunk();
                let score = cosine_similarity(
                    query_embedding,
                    chunk
                        .embedding
                        .as_ref()
                        .expect("sqlite row with embedding should deserialize with embedding"),
                );
                (chunk, score)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    pub async fn search_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>, sqlx::Error> {
        let pattern = format!("%{query}%");

        let rows = sqlx::query_as::<_, ChunkRow>(
            r#"
            SELECT id, source, content, file_path, line_start, line_end, 
                   token_count, hash, created_at, updated_at
            FROM chunks
            WHERE content LIKE ?1
            ORDER BY rowid DESC
            LIMIT ?2
            "#,
        )
        .bind(&pattern)
        .bind(limit as i32)
        .fetch_all(&self.pool)
        .await?;

        let results: Vec<(MemoryChunk, f32)> = rows
            .into_iter()
            .map(|row| {
                let chunk = row.into_chunk();
                let score = bm25_score(&chunk.content, query);
                (chunk, score)
            })
            .collect();

        Ok(results)
    }

    pub async fn get_index_meta(&self) -> Result<Option<MemoryIndexMeta>, sqlx::Error> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM index_meta WHERE key = 'index_meta_v1'")
                .fetch_optional(&self.pool)
                .await?;

        match row {
            Some((json,)) => {
                let meta: MemoryIndexMeta = serde_json::from_str(&json).map_err(|_| {
                    sqlx::Error::Decode(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Failed to parse index meta",
                    )))
                })?;
                Ok(Some(meta))
            }
            None => Ok(None),
        }
    }

    pub async fn set_index_meta(&self, meta: &MemoryIndexMeta) -> Result<(), sqlx::Error> {
        let json = serde_json::to_string(meta).map_err(|_| {
            sqlx::Error::Encode(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Failed to serialize index meta",
            )))
        })?;

        sqlx::query("INSERT OR REPLACE INTO index_meta (key, value) VALUES ('index_meta_v1', ?1)")
            .bind(&json)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn get_chunk_count(&self) -> Result<usize, sqlx::Error> {
        let row: (i32,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
            .fetch_one(&self.pool)
            .await?;

        Ok(row.0 as usize)
    }

    pub async fn get_file_hashes(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, sqlx::Error> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT file_path, hash FROM chunks WHERE file_path IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().collect())
    }

    fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for &f in embedding {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        bytes
    }

    fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }
}

#[derive(sqlx::FromRow)]
struct ChunkRow {
    id: String,
    source: String,
    content: String,
    file_path: Option<String>,
    line_start: i32,
    line_end: i32,
    token_count: i32,
    hash: String,
    created_at: String,
    updated_at: String,
}

impl ChunkRow {
    fn into_chunk(self) -> MemoryChunk {
        let source = match self.source.as_str() {
            "file" => MemorySource::File,
            "session" => MemorySource::Session,
            "qmd" => MemorySource::Qmd,
            _ => MemorySource::Manual,
        };

        MemoryChunk {
            id: self.id,
            source,
            content: self.content,
            embedding: None,
            metadata: MemoryMetadata {
                file_path: self.file_path,
                line_start: self.line_start as usize,
                line_end: self.line_end as usize,
                token_count: self.token_count as usize,
                hash: self.hash,
            },
            created_at: chrono::DateTime::parse_from_rfc3339(&self.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        }
    }
}

#[derive(sqlx::FromRow)]
struct ChunkWithEmbeddingRow {
    id: String,
    source: String,
    content: String,
    file_path: Option<String>,
    line_start: i32,
    line_end: i32,
    token_count: i32,
    hash: String,
    created_at: String,
    updated_at: String,
    embedding: Vec<u8>,
}

impl ChunkWithEmbeddingRow {
    fn into_chunk(self) -> MemoryChunk {
        let source = match self.source.as_str() {
            "file" => MemorySource::File,
            "session" => MemorySource::Session,
            "qmd" => MemorySource::Qmd,
            _ => MemorySource::Manual,
        };

        let embedding = VectorStorage::bytes_to_embedding(&self.embedding);

        MemoryChunk {
            id: self.id,
            source,
            content: self.content,
            embedding: Some(embedding),
            metadata: MemoryMetadata {
                file_path: self.file_path,
                line_start: self.line_start as usize,
                line_end: self.line_end as usize,
                token_count: self.token_count as usize,
                hash: self.hash,
            },
            created_at: chrono::DateTime::parse_from_rfc3339(&self.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    dot / (mag_a * mag_b)
}

fn bm25_score(content: &str, query: &str) -> f32 {
    let query_lower = query.to_lowercase();
    let content_lower = content.to_lowercase();

    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();
    let mut score = 0.0;

    for term in query_terms {
        if content_lower.contains(term) {
            score += 1.0;
        }
    }

    score
}
