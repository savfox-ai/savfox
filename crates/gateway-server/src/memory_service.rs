use std::path::PathBuf;
use std::sync::Arc;

use savfox_memory::MemoryManager;
use savfox_memory::embedding::EmbeddingApiKeys;
use savfox_memory::types::{MemoryConfig, MemorySearchResult, MemorySyncProgress};
use tokio::sync::RwLock;
use tracing::info;

pub struct MemoryService {
    manager: Arc<RwLock<Option<MemoryManager>>>,
    savfox_home: PathBuf,
}

impl MemoryService {
    #[must_use]
    pub fn new(savfox_home: PathBuf) -> Self {
        Self {
            manager: Arc::new(RwLock::new(None)),
            savfox_home,
        }
    }

    pub async fn initialize(
        &self,
        config: MemoryConfig,
        api_keys: EmbeddingApiKeys,
    ) -> Result<(), MemoryServiceError> {
        let manager = MemoryManager::new(&self.savfox_home, config, api_keys)
            .await
            .map_err(|e| MemoryServiceError::InitError(e.to_string()))?;

        let mut guard = self.manager.write().await;
        *guard = Some(manager);

        info!("Memory service initialized");
        Ok(())
    }

    pub async fn is_initialized(&self) -> bool {
        self.manager.read().await.is_some()
    }

    pub async fn sync(&self) -> Result<SyncStats, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        let stats = manager
            .sync_all()
            .await
            .map_err(|e| MemoryServiceError::SyncError(e.to_string()))?;

        Ok(SyncStats {
            files_updated: stats.files_updated,
            files_unchanged: stats.files_unchanged,
            chunks_added: stats.chunks_added,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<MemorySearchResult>, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        let results = manager
            .search(query)
            .await
            .map_err(|e| MemoryServiceError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn search_vector(
        &self,
        query: &str,
    ) -> Result<Vec<MemorySearchResult>, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        let results = manager
            .search_vector(query)
            .await
            .map_err(|e| MemoryServiceError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn search_keyword(
        &self,
        query: &str,
    ) -> Result<Vec<MemorySearchResult>, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        let results = manager
            .search_keyword(query)
            .await
            .map_err(|e| MemoryServiceError::SearchError(e.to_string()))?;

        Ok(results)
    }

    pub async fn add_memory(&self, content: &str) -> Result<String, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        let chunk = manager
            .add_memory(content, None)
            .await
            .map_err(|e| MemoryServiceError::StorageError(e.to_string()))?;

        Ok(chunk.id)
    }

    pub async fn delete_memory(&self, id: &str) -> Result<bool, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        manager
            .delete_memory(id)
            .await
            .map_err(|e| MemoryServiceError::StorageError(e.to_string()))
    }

    pub async fn get_chunk_count(&self) -> Result<usize, MemoryServiceError> {
        let guard = self.manager.read().await;
        let manager = guard.as_ref().ok_or(MemoryServiceError::NotInitialized)?;

        manager
            .get_chunk_count()
            .await
            .map_err(|e| MemoryServiceError::StorageError(e.to_string()))
    }

    #[must_use]
    pub fn subscribe_progress(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<MemorySyncProgress>> {
        None
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SyncStats {
    pub files_updated: usize,
    pub files_unchanged: usize,
    pub chunks_added: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryServiceError {
    #[error("Memory service not initialized")]
    NotInitialized,

    #[error("Initialization error: {0}")]
    InitError(String),

    #[error("Sync error: {0}")]
    SyncError(String),

    #[error("Search error: {0}")]
    SearchError(String),

    #[error("Storage error: {0}")]
    StorageError(String),
}
