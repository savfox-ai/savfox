use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::info;

use crate::embedding::{
    EmbeddingApiKeys, EmbeddingProvider, create_embedding_provider, split_into_batches,
};
use crate::qmd::{QmdFile, QmdManager};
use crate::search::HybridSearch;
use crate::storage::VectorStorage;
use crate::sync::{MemoryFileSync, SessionFileSync};
use crate::types::{
    EMBEDDING_BATCH_MAX_TOKENS, MemoryChunk, MemoryConfig, MemoryIndexMeta, MemoryMetadata,
    MemorySearchResult, MemorySource, MemorySyncProgress, MemorySyncStatus,
};

pub struct MemoryManager {
    config: MemoryConfig,
    storage: Arc<RwLock<VectorStorage>>,
    provider: Box<dyn EmbeddingProvider>,
    file_sync: MemoryFileSync,
    session_sync: Option<SessionFileSync>,
    qmd_manager: QmdManager,
    search: HybridSearch,
    progress_tx: broadcast::Sender<MemorySyncProgress>,
    syncing: Arc<Mutex<bool>>,
    embedding_cache: Arc<RwLock<HashMap<String, Vec<f32>>>>,
}

const EMBEDDING_CACHE_MAX_ENTRIES: usize = 2048;

impl MemoryManager {
    pub async fn new(
        savfox_home: &Path,
        config: MemoryConfig,
        api_keys: EmbeddingApiKeys,
    ) -> Result<Self, MemoryError> {
        let memory_dir = savfox_home.join("memory");
        let sessions_dir = savfox_home.join("sessions");
        let db_path = savfox_home.join("memory.db");

        let provider = create_embedding_provider(&config.embedding_provider, &api_keys)
            .map_err(|e| MemoryError::InitError(e.to_string()))?;

        let vector_dims = provider.vector_dims();
        let storage = VectorStorage::new(&db_path, vector_dims)
            .await
            .map_err(|e| MemoryError::InitError(e.to_string()))?;

        let session_sync = if config.index_session_transcripts {
            Some(SessionFileSync::new(sessions_dir))
        } else {
            None
        };

        let (progress_tx, _) = broadcast::channel(16);

        Ok(Self {
            config,
            storage: Arc::new(RwLock::new(storage)),
            provider,
            file_sync: MemoryFileSync::new(memory_dir.clone(), 500, 50),
            session_sync,
            qmd_manager: QmdManager::new(memory_dir.join("qmd")),
            search: HybridSearch::new(),
            progress_tx,
            syncing: Arc::new(Mutex::new(false)),
            embedding_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    #[must_use]
    pub fn subscribe_progress(&self) -> broadcast::Receiver<MemorySyncProgress> {
        self.progress_tx.subscribe()
    }

    pub async fn sync_all(&self) -> Result<SyncStats, MemoryError> {
        let mut syncing = self.syncing.lock().await;
        if *syncing {
            return Err(MemoryError::AlreadySyncing);
        }
        *syncing = true;
        drop(syncing);

        let _guard = SyncGuard {
            syncing: Arc::clone(&self.syncing),
        };

        self.emit_progress(MemorySyncProgress {
            total: 0,
            completed: 0,
            current_file: None,
            status: MemorySyncStatus::Scanning,
        });

        let files = self
            .file_sync
            .scan_memory_files()
            .map_err(|e| MemoryError::SyncError(e.to_string()))?;

        let storage = self.storage.read().await;
        let existing_hashes = storage
            .source_file_hashes(&MemorySource::File)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))?;
        drop(storage);

        let mut stats = SyncStats::default();

        self.emit_progress(MemorySyncProgress {
            total: files.len(),
            completed: 0,
            current_file: None,
            status: MemorySyncStatus::Indexing,
        });

        for (idx, file) in files.iter().enumerate() {
            let needs_update = existing_hashes
                .get(file.path.to_string_lossy().as_ref())
                .map(|h| *h != file.hash)
                .unwrap_or(true);

            if needs_update {
                let chunks = self.file_sync.chunk_file(file);

                self.emit_progress(MemorySyncProgress {
                    total: files.len(),
                    completed: idx,
                    current_file: Some(file.path.to_string_lossy().to_string()),
                    status: MemorySyncStatus::Embedding,
                });

                self.replace_file(&MemorySource::File, &file.path, &file.hash, &chunks)
                    .await?;

                stats.files_updated += 1;
                stats.chunks_added += chunks.len();
            } else {
                stats.files_unchanged += 1;
            }

            self.emit_progress(MemorySyncProgress {
                total: files.len(),
                completed: idx + 1,
                current_file: None,
                status: MemorySyncStatus::Indexing,
            });
        }

        self.prune_files(&MemorySource::File, files.iter().map(|file| &file.path))
            .await?;

        if let Some(ref session_sync) = self.session_sync {
            self.sync_sessions(session_sync).await?;
        }

        self.update_index_meta().await?;

        self.emit_progress(MemorySyncProgress {
            total: files.len(),
            completed: files.len(),
            current_file: None,
            status: MemorySyncStatus::Complete,
        });

        info!("Memory sync complete: {:?}", stats);
        Ok(stats)
    }

    async fn sync_sessions(&self, session_sync: &SessionFileSync) -> Result<(), MemoryError> {
        let files = session_sync
            .scan_session_files()
            .map_err(|e| MemoryError::SyncError(e.to_string()))?;

        let existing_hashes = self
            .storage
            .read()
            .await
            .source_file_hashes(&MemorySource::Session)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))?;
        for file in &files {
            if existing_hashes.get(file.path.to_string_lossy().as_ref()) == Some(&file.hash) {
                continue;
            }
            let chunks = session_sync.build_session_chunks(file, self.config.chunk_tokens);
            self.replace_file(&MemorySource::Session, &file.path, &file.hash, &chunks)
                .await?;
        }

        self.prune_files(&MemorySource::Session, files.iter().map(|file| &file.path))
            .await?;

        Ok(())
    }

    async fn index_chunks(&self, chunks: &[MemoryChunk]) -> Result<(), MemoryError> {
        let chunks = self.embed_chunks(chunks).await?;
        let storage = self.storage.write().await;
        for chunk in &chunks {
            storage
                .insert_chunk(chunk)
                .await
                .map_err(|e| MemoryError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    async fn replace_file(
        &self,
        source: &MemorySource,
        path: &Path,
        hash: &str,
        chunks: &[MemoryChunk],
    ) -> Result<(), MemoryError> {
        // Finish remote work before starting the transaction. A failed embedding
        // leaves the previously published revision and its manifest intact.
        let chunks = self.embed_chunks(chunks).await?;
        self.storage
            .write()
            .await
            .replace_file_chunks(source, &path.to_string_lossy(), hash, &chunks)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    async fn prune_files<'a>(
        &self,
        source: &MemorySource,
        paths: impl Iterator<Item = &'a std::path::PathBuf>,
    ) -> Result<(), MemoryError> {
        let live: HashSet<String> = paths
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        self.storage
            .write()
            .await
            .prune_source_files(source, &live)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    async fn embed_chunks(&self, chunks: &[MemoryChunk]) -> Result<Vec<MemoryChunk>, MemoryError> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let batches = split_into_batches(&texts, EMBEDDING_BATCH_MAX_TOKENS);
        let mut chunks = chunks.to_vec();

        for batch in batches {
            let embeddings = self.embed_texts_cached(&batch.texts).await?;

            for (batch_idx, &chunk_idx) in batch.indices.iter().enumerate() {
                if chunk_idx < chunks.len() && batch_idx < embeddings.len() {
                    chunks[chunk_idx].embedding = Some(embeddings[batch_idx].clone());
                }
            }
        }

        Ok(chunks)
    }

    pub async fn search(&self, query: &str) -> Result<Vec<MemorySearchResult>, MemoryError> {
        let storage = self.storage.read().await;

        let results = self
            .search
            .search(
                &storage,
                self.provider.as_ref(),
                query,
                self.config.max_results,
            )
            .await
            .map_err(|e| MemoryError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn search_vector(&self, query: &str) -> Result<Vec<MemorySearchResult>, MemoryError> {
        let query_embedding = self
            .embed_texts_cached(&[query.to_owned()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| MemoryError::EmbeddingError("empty embedding response".to_owned()))?;

        let storage = self.storage.read().await;
        let results = self
            .search
            .search_vector_only(&storage, &query_embedding, self.config.max_results)
            .await
            .map_err(|e| MemoryError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn search_keyword(
        &self,
        query: &str,
    ) -> Result<Vec<MemorySearchResult>, MemoryError> {
        let storage = self.storage.read().await;
        let results = self
            .search
            .search_keyword_only(&storage, query, self.config.max_results)
            .await
            .map_err(|e| MemoryError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn add_memory(
        &self,
        content: &str,
        metadata: Option<MemoryMetadata>,
    ) -> Result<MemoryChunk, MemoryError> {
        let chunk = MemoryChunk {
            id: uuid::Uuid::new_v4().to_string(),
            source: crate::types::MemorySource::Manual,
            content: content.to_owned(),
            embedding: None,
            metadata: metadata.unwrap_or_else(|| MemoryMetadata {
                file_path: None,
                line_start: 0,
                line_end: 0,
                token_count: crate::sync::estimate_tokens(content),
                hash: String::new(),
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.index_chunks(std::slice::from_ref(&chunk)).await?;

        Ok(chunk)
    }

    pub async fn delete_memory(&self, id: &str) -> Result<bool, MemoryError> {
        let storage = self.storage.write().await;
        storage
            .delete_chunk(id)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    pub async fn get_chunk_count(&self) -> Result<usize, MemoryError> {
        let storage = self.storage.read().await;
        storage
            .get_chunk_count()
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    pub fn list_qmd_files(&self) -> Result<Vec<QmdFile>, MemoryError> {
        self.qmd_manager
            .list_qmd_files()
            .map_err(|e| MemoryError::QmdError(e.to_string()))
    }

    pub async fn update_index_meta(&self) -> Result<(), MemoryError> {
        let meta = MemoryIndexMeta {
            model: self.provider.model_name().to_owned(),
            provider: self.provider.provider_name().to_owned(),
            chunk_tokens: self.config.chunk_tokens,
            chunk_overlap: self.config.chunk_overlap,
            vector_dims: self.provider.vector_dims(),
            indexed_at: chrono::Utc::now(),
        };

        let storage = self.storage.write().await;
        storage
            .set_index_meta(&meta)
            .await
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    fn emit_progress(&self, progress: MemorySyncProgress) {
        let _ = self.progress_tx.send(progress);
    }

    async fn embed_texts_cached(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut embeddings = vec![Vec::new(); texts.len()];
        let mut missing_texts = Vec::new();
        let mut missing_indices = Vec::new();

        {
            let cache = self.embedding_cache.read().await;
            for (idx, text) in texts.iter().enumerate() {
                if let Some(cached) = cache.get(text) {
                    embeddings[idx] = cached.clone();
                } else {
                    missing_texts.push(text.clone());
                    missing_indices.push(idx);
                }
            }
        }

        if missing_texts.is_empty() {
            return Ok(embeddings);
        }

        let result = self
            .provider
            .embed(&missing_texts)
            .await
            .map_err(|e| MemoryError::EmbeddingError(e.to_string()))?;

        if result.embeddings.len() != missing_texts.len() {
            return Err(MemoryError::EmbeddingError(format!(
                "embedding count mismatch: expected {}, got {}",
                missing_texts.len(),
                result.embeddings.len()
            )));
        }

        let mut cache = self.embedding_cache.write().await;
        for (miss_pos, text) in missing_texts.into_iter().enumerate() {
            let emb = result.embeddings[miss_pos].clone();
            let out_idx = missing_indices[miss_pos];
            embeddings[out_idx] = emb.clone();

            if !cache.contains_key(&text)
                && cache.len() >= EMBEDDING_CACHE_MAX_ENTRIES
                && let Some(evict_key) = cache.keys().next().cloned()
            {
                cache.remove(&evict_key);
            }
            cache.insert(text, emb);
        }

        Ok(embeddings)
    }
}

struct SyncGuard {
    syncing: Arc<Mutex<bool>>,
}

impl Drop for SyncGuard {
    fn drop(&mut self) {
        if let Ok(mut syncing) = self.syncing.try_lock() {
            *syncing = false;
        }
    }
}

#[derive(Debug, Default)]
pub struct SyncStats {
    pub files_updated: usize,
    pub files_unchanged: usize,
    pub chunks_added: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Initialization error: {0}")]
    InitError(String),

    #[error("Sync already in progress")]
    AlreadySyncing,

    #[error("Sync error: {0}")]
    SyncError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    #[error("Search error: {0}")]
    SearchError(String),

    #[error("QMD error: {0}")]
    QmdError(String),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::embedding::{EmbeddingError, EmbeddingResult};
    use crate::types::EmbeddingProviderConfig;

    struct TestProvider {
        fail: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl EmbeddingProvider for TestProvider {
        async fn embed(&self, texts: &[String]) -> Result<EmbeddingResult, EmbeddingError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(EmbeddingError::Config("injected failure".to_owned()));
            }
            Ok(EmbeddingResult {
                embeddings: vec![vec![1.0, 0.0]; texts.len()],
                model: "test".to_owned(),
                total_tokens: 0,
            })
        }
        fn model_name(&self) -> &str {
            "test"
        }
        fn provider_name(&self) -> &str {
            "test"
        }
        fn vector_dims(&self) -> usize {
            2
        }
        fn max_input_tokens(&self) -> usize {
            8192
        }
    }

    async fn manager(home: &Path) -> (MemoryManager, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let config = MemoryConfig {
            embedding_provider: EmbeddingProviderConfig::Ollama {
                model: "test".to_owned(),
                base_url: "http://127.0.0.1:1".to_owned(),
            },
            ..Default::default()
        };
        let mut manager = MemoryManager::new(home, config, EmbeddingApiKeys::default())
            .await
            .unwrap();
        let fail = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        manager.provider = Box::new(TestProvider {
            fail: Arc::clone(&fail),
            calls: Arc::clone(&calls),
        });
        (manager, fail, calls)
    }

    #[tokio::test]
    async fn sync_replaces_revisions_skips_unchanged_and_prunes_removed_sources() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("memory")).unwrap();
        std::fs::create_dir(home.path().join("sessions")).unwrap();
        let file = home.path().join("memory/notes.md");
        let session = home.path().join("sessions/session.md");
        std::fs::write(&file, "old memory").unwrap();
        std::fs::write(&session, "old session").unwrap();
        let (manager, _, calls) = manager(home.path()).await;
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 2);
        let count = calls.load(Ordering::SeqCst);
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), count);

        std::fs::write(&file, "new memory").unwrap();
        std::fs::write(&session, "new session").unwrap();
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 2);
        assert!(manager.search_keyword("old").await.unwrap().is_empty());

        std::fs::write(&file, "").unwrap();
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 1);
        assert_eq!(manager.sync_all().await.unwrap().files_unchanged, 1);

        let manual = manager.add_memory("keep manual", None).await.unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_file(session).unwrap();
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 1);
        assert!(
            manager
                .storage
                .read()
                .await
                .get_chunk(&manual.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn embedding_or_scan_failure_preserves_published_revision() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("memory")).unwrap();
        let file = home.path().join("memory/notes.md");
        std::fs::write(&file, "old revision").unwrap();
        let (manager, fail, _) = manager(home.path()).await;
        manager.sync_all().await.unwrap();
        std::fs::write(&file, "new revision").unwrap();
        fail.store(true, Ordering::SeqCst);
        assert!(manager.sync_all().await.is_err());
        assert_eq!(manager.search_keyword("old").await.unwrap().len(), 1);
        assert!(manager.search_keyword("new").await.unwrap().is_empty());

        fail.store(false, Ordering::SeqCst);
        std::fs::write(&file, [0xff]).unwrap();
        assert!(manager.sync_all().await.is_err());
        assert_eq!(manager.get_chunk_count().await.unwrap(), 1);
        std::fs::write(&file, "new revision").unwrap();
        manager.sync_all().await.unwrap();
        assert_eq!(manager.get_chunk_count().await.unwrap(), 1);
        assert!(manager.search_keyword("old").await.unwrap().is_empty());
    }
}
