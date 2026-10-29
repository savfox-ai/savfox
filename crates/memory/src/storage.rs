use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;

use futures::TryStreamExt;
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

        // Do not backfill from chunks: legacy indexes may contain multiple
        // revisions of the same file. The first sync replaces them completely.
        sqlx::query("CREATE TABLE IF NOT EXISTS indexed_files (source TEXT NOT NULL, file_path TEXT NOT NULL, hash TEXT NOT NULL, PRIMARY KEY (source, file_path))")
            .execute(&self.pool)
            .await?;

        info!("Vector storage initialized with {} dims", self.vector_dims);
        Ok(())
    }

    pub async fn insert_chunk(&self, chunk: &MemoryChunk) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        Self::insert_chunk_on(&mut tx, chunk).await?;
        tx.commit().await
    }

    async fn insert_chunk_on(
        connection: &mut sqlx::SqliteConnection,
        chunk: &MemoryChunk,
    ) -> Result<(), sqlx::Error> {
        let source_str = source_name(&chunk.source);

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
        .execute(&mut *connection)
        .await?;

        if let Some(ref embedding) = chunk.embedding {
            let embedding_bytes = Self::embedding_to_bytes(embedding);
            sqlx::query("INSERT OR REPLACE INTO embeddings (chunk_id, embedding) VALUES (?1, ?2)")
                .bind(&chunk.id)
                .bind(&embedding_bytes)
                .execute(&mut *connection)
                .await?;
        }

        Ok(())
    }

    pub(crate) async fn source_file_hashes(
        &self,
        source: &MemorySource,
    ) -> Result<HashMap<String, String>, sqlx::Error> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT file_path, hash FROM indexed_files WHERE source = ?1")
                .bind(source_name(source))
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// Publish a fully embedded revision atomically, including empty files.
    pub(crate) async fn replace_file_chunks(
        &self,
        source: &MemorySource,
        path: &str,
        hash: &str,
        chunks: &[MemoryChunk],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM chunks WHERE source = ?1 AND file_path = ?2")
            .bind(source_name(source))
            .bind(path)
            .execute(&mut *tx)
            .await?;
        for chunk in chunks {
            Self::insert_chunk_on(&mut tx, chunk).await?;
        }
        sqlx::query(
            "INSERT OR REPLACE INTO indexed_files (source, file_path, hash) VALUES (?1, ?2, ?3)",
        )
        .bind(source_name(source))
        .bind(path)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    /// Prune only after a complete successful source scan. Include legacy rows
    /// without a manifest so removed files from older indexes are cleaned up.
    pub(crate) async fn prune_source_files(
        &self,
        source: &MemorySource,
        live: &HashSet<String>,
    ) -> Result<(), sqlx::Error> {
        let source = source_name(source);
        let mut tx = self.pool.begin().await?;
        let paths: Vec<(String,)> = sqlx::query_as("SELECT file_path FROM indexed_files WHERE source = ?1 UNION SELECT file_path FROM chunks WHERE source = ?1 AND file_path IS NOT NULL")
            .bind(source).fetch_all(&mut *tx).await?;
        for (path,) in paths {
            if !live.contains(&path) {
                sqlx::query("DELETE FROM chunks WHERE source = ?1 AND file_path = ?2")
                    .bind(source)
                    .bind(&path)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM indexed_files WHERE source = ?1 AND file_path = ?2")
                    .bind(source)
                    .bind(&path)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await
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
        if limit == 0 {
            return Ok(Vec::new());
        }
        let pool = self.pool.clone();
        let query_embedding = query_embedding.to_vec();
        // Run cosine work on a blocking worker rather than Tokio's async workers.
        // SQLx's stream buffers only a bounded row window.
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut tx = pool.begin().await?;
                let mut rows = sqlx::query_as::<_, (String, Vec<u8>)>(
                    "SELECT c.id, e.embedding FROM chunks c JOIN embeddings e ON c.id = e.chunk_id ORDER BY c.rowid",
                ).fetch(&mut *tx);
                let mut best = BinaryHeap::new();
                let mut ordinal = 0;
                while let Some((id, bytes)) = rows.try_next().await? {
                    let embedding = Self::bytes_to_embedding(&bytes);
                    let candidate = RankedChunk { id, score: cosine_similarity(&query_embedding, &embedding), ordinal };
                    retain_top_k(&mut best, candidate, limit);
                    ordinal += 1;
                }
                drop(rows);

                let mut ranked = best.into_vec();
                ranked.sort_unstable();
                let mut results = Vec::with_capacity(ranked.len());
                // Fetch content and metadata only for winners in the same read
                // snapshot, so concurrent edits cannot invalidate selected IDs.
                for candidate in ranked {
                    let row = sqlx::query_as::<_, ChunkWithEmbeddingRow>(
                        "SELECT c.id, c.source, c.content, c.file_path, c.line_start, c.line_end, c.token_count, c.hash, c.created_at, c.updated_at, e.embedding FROM chunks c JOIN embeddings e ON c.id = e.chunk_id WHERE c.id = ?1",
                    ).bind(candidate.id).fetch_one(&mut *tx).await?;
                    results.push((row.into_chunk(), candidate.score));
                }
                tx.commit().await?;
                Ok::<_, sqlx::Error>(results)
            })
        }).await.map_err(|error| sqlx::Error::Io(std::io::Error::other(error)))?
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
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }
}

fn source_name(source: &MemorySource) -> &'static str {
    match source {
        MemorySource::File => "file",
        MemorySource::Session => "session",
        MemorySource::Qmd => "qmd",
        MemorySource::Manual => "manual",
    }
}

// Worst candidate sorts greatest, exposing the entry to evict. Insertion order
// breaks ties, matching a stable descending score sort.
struct RankedChunk {
    id: String,
    score: f32,
    ordinal: usize,
}

impl PartialEq for RankedChunk {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for RankedChunk {}
impl PartialOrd for RankedChunk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RankedChunk {
    fn cmp(&self, other: &Self) -> Ordering {
        // Undefined cosine scores rank last without breaking the heap's total
        // ordering. Finite scores retain the existing descending semantics.
        let rank = |score: f32| {
            if score.is_nan() {
                f32::NEG_INFINITY
            } else {
                score
            }
        };
        rank(other.score)
            .partial_cmp(&rank(self.score))
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

fn retain_top_k(best: &mut BinaryHeap<RankedChunk>, candidate: RankedChunk, limit: usize) {
    if limit == 0 {
        return;
    }
    if best.len() < limit {
        best.push(candidate);
    } else if best.peek().is_some_and(|worst| candidate < *worst) {
        best.pop();
        best.push(candidate);
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

#[cfg(test)]
mod sync_tests {
    use super::*;

    fn chunk(id: &str, content: &str) -> MemoryChunk {
        MemoryChunk {
            id: id.to_owned(),
            source: MemorySource::File,
            content: content.to_owned(),
            embedding: Some(vec![1.0, 0.0]),
            metadata: MemoryMetadata {
                file_path: Some("notes.md".to_owned()),
                line_start: 1,
                line_end: 1,
                token_count: 1,
                hash: content.to_owned(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn replacement_repairs_legacy_duplicates_and_rolls_back_failed_writes() {
        let storage = VectorStorage::new(Path::new(":memory:"), 2).await.unwrap();
        storage.insert_chunk(&chunk("old1", "old")).await.unwrap();
        storage.insert_chunk(&chunk("old2", "old")).await.unwrap();
        storage
            .replace_file_chunks(
                &MemorySource::File,
                "notes.md",
                "new",
                &[chunk("new", "new")],
            )
            .await
            .unwrap();
        assert_eq!(storage.get_chunk_count().await.unwrap(), 1);
        assert!(storage.get_embedding("old1").await.unwrap().is_none());
        sqlx::query("CREATE TRIGGER fail_insert BEFORE INSERT ON chunks WHEN NEW.content = 'fail' BEGIN SELECT RAISE(ABORT, 'injected failure'); END")
            .execute(&storage.pool).await.unwrap();
        assert!(
            storage
                .replace_file_chunks(
                    &MemorySource::File,
                    "notes.md",
                    "fail",
                    &[chunk("fail", "fail")]
                )
                .await
                .is_err()
        );
        assert_eq!(storage.get_chunk_count().await.unwrap(), 1);
        assert!(storage.get_chunk("new").await.unwrap().is_some());
        assert_eq!(
            storage
                .source_file_hashes(&MemorySource::File)
                .await
                .unwrap()
                .get("notes.md")
                .map(String::as_str),
            Some("new")
        );

        // Legacy entries lacking a manifest are also pruned after source deletion.
        storage
            .insert_chunk(&chunk("legacy", "legacy"))
            .await
            .unwrap();
        storage
            .prune_source_files(&MemorySource::File, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(storage.get_chunk_count().await.unwrap(), 0);
        assert!(
            storage
                .source_file_hashes(&MemorySource::File)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn top_k_is_bounded_and_matches_stable_full_sort() {
        let mut heap = BinaryHeap::new();
        let mut reference = Vec::new();
        for ordinal in 0..10_000 {
            let score = ((ordinal * 37) % 101) as f32 / 100.0;
            reference.push((ordinal.to_string(), score));
            retain_top_k(
                &mut heap,
                RankedChunk {
                    id: ordinal.to_string(),
                    score,
                    ordinal,
                },
                7,
            );
            assert!(heap.len() <= 7);
        }
        reference.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        reference.truncate(7);
        let mut retained = heap.into_vec();
        retained.sort_unstable();
        assert_eq!(
            retained
                .into_iter()
                .map(|candidate| (candidate.id, candidate.score))
                .collect::<Vec<_>>(),
            reference
        );
    }

    #[test]
    fn top_k_handles_zero_limit_and_undefined_scores() {
        let mut heap = BinaryHeap::new();
        retain_top_k(
            &mut heap,
            RankedChunk {
                id: "none".to_owned(),
                score: 1.0,
                ordinal: 0,
            },
            0,
        );
        assert!(heap.is_empty());
        retain_top_k(
            &mut heap,
            RankedChunk {
                id: "nan".to_owned(),
                score: f32::NAN,
                ordinal: 0,
            },
            1,
        );
        retain_top_k(
            &mut heap,
            RankedChunk {
                id: "valid".to_owned(),
                score: -1.0,
                ordinal: 1,
            },
            1,
        );
        assert_eq!(heap.pop().unwrap().id, "valid");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamed_search_matches_full_sort_and_returns_winner_metadata() {
        let storage = VectorStorage::new(Path::new(":memory:"), 2).await.unwrap();
        let mut reference = Vec::new();
        for ordinal in 0..64 {
            // Repeated scores deliberately exercise stable insertion-order ties.
            let embedding = vec![(ordinal % 5) as f32 - 2.0, 1.0];
            let chunk = MemoryChunk {
                id: ordinal.to_string(),
                source: MemorySource::Manual,
                content: format!("content {ordinal}"),
                embedding: Some(embedding.clone()),
                metadata: MemoryMetadata {
                    file_path: None,
                    line_start: 1,
                    line_end: 1,
                    token_count: 2,
                    hash: String::new(),
                },
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            storage.insert_chunk(&chunk).await.unwrap();
            reference.push((chunk.id, cosine_similarity(&[1.0, 0.0], &embedding)));
        }
        reference.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for limit in [0, 1, 7, 100] {
            let results = storage.search_similar(&[1.0, 0.0], limit).await.unwrap();
            let actual = results
                .iter()
                .map(|(chunk, score)| (chunk.id.clone(), *score))
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                reference.iter().take(limit).cloned().collect::<Vec<_>>()
            );
            for (chunk, _) in results {
                assert_eq!(chunk.content, format!("content {}", chunk.id));
                assert!(chunk.embedding.is_some());
            }
        }
        storage.pool.close().await;
        assert!(
            storage
                .search_similar(&[1.0, 0.0], 0)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
