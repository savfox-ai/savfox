//! Chat attachment handling  - upload, download, temporary file storage with lifecycle management.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

/// Attachment metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

/// Maximum attachment size: 50MB
const MAX_ATTACHMENT_SIZE: u64 = 50 * 1024 * 1024;

/// Default time-to-live: 1 hour
const DEFAULT_TTL_SECS: u64 = 3600;

/// Allowed MIME type prefixes
const ALLOWED_MIME_PREFIXES: &[&str] = &[
    "image/",
    "audio/",
    "video/",
    "text/",
    "application/pdf",
    "application/json",
];

/// File-backed attachment store with in-memory index
#[derive(Debug)]
pub struct AttachmentStore {
    base_dir: PathBuf,
    index: RwLock<HashMap<String, AttachmentMeta>>,
}

impl AttachmentStore {
    pub fn new(base_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&base_dir);
        Self {
            base_dir,
            index: RwLock::new(HashMap::new()),
        }
    }

    pub fn from_home(savfox_home: &Path) -> Self {
        Self::new(savfox_home.join("gateway").join("attachments"))
    }

    /// Validate MIME type against the allow-list
    fn validate_mime(mime: &str) -> bool {
        ALLOWED_MIME_PREFIXES.iter().any(|p| mime.starts_with(p))
    }

    /// Store an attachment from raw bytes, returns metadata
    pub async fn store(
        &self,
        filename: String,
        mime_type: String,
        data: &[u8],
    ) -> Result<AttachmentMeta, String> {
        if data.len() as u64 > MAX_ATTACHMENT_SIZE {
            return Err(format!(
                "Attachment too large: {} bytes (max {})",
                data.len(),
                MAX_ATTACHMENT_SIZE
            ));
        }
        if !Self::validate_mime(&mime_type) {
            return Err(format!("MIME type not allowed: {mime_type}"));
        }

        let id = uuid::Uuid::now_v7().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let meta = AttachmentMeta {
            id: id.clone(),
            filename: filename.clone(),
            mime_type,
            size_bytes: data.len() as u64,
            created_at: now,
            expires_at: Some(now + DEFAULT_TTL_SECS),
        };

        let file_path = self.base_dir.join(&id);
        tokio::fs::write(&file_path, data)
            .await
            .map_err(|e| format!("Failed to write attachment: {e}"))?;

        self.index.write().await.insert(id.clone(), meta.clone());
        info!(id = %id, filename = %filename, "stored attachment");
        Ok(meta)
    }

    /// Retrieve attachment data by id
    pub async fn get(&self, id: &str) -> Result<(AttachmentMeta, Vec<u8>), String> {
        let index = self.index.read().await;
        let meta = index
            .get(id)
            .ok_or_else(|| format!("Attachment not found: {id}"))?
            .clone();
        drop(index);

        // Check expiry
        if let Some(expires) = meta.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now > expires {
                self.delete(id).await;
                return Err("Attachment expired".to_owned());
            }
        }

        let file_path = self.base_dir.join(id);
        let data = tokio::fs::read(&file_path)
            .await
            .map_err(|e| format!("Failed to read attachment: {e}"))?;
        Ok((meta, data))
    }

    /// Delete an attachment by id
    pub async fn delete(&self, id: &str) {
        self.index.write().await.remove(id);
        let file_path = self.base_dir.join(id);
        let _ = tokio::fs::remove_file(&file_path).await;
    }

    /// List all attachments
    pub async fn list(&self) -> Vec<AttachmentMeta> {
        self.index.read().await.values().cloned().collect()
    }

    /// Prune expired attachments, returns count of removed entries
    pub async fn prune_expired(&self) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let expired: Vec<String> = {
            let index = self.index.read().await;
            index
                .iter()
                .filter(|(_, m)| m.expires_at.is_some_and(|e| now > e))
                .map(|(id, _)| id.clone())
                .collect()
        };

        let count = expired.len();
        for id in &expired {
            self.delete(id).await;
        }
        if count > 0 {
            info!(count, "pruned expired attachments");
        }
        count
    }
}
