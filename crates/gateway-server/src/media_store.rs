//! Media storage for inbound/remote media with SSRF-safe downloads.
//!
//! Features:
//! - file-based storage under `{savfox_home}/media/`
//! - inbound files land in `{savfox_home}/media/staging/`
//! - UUID file naming
//! - MIME detection (magic bytes + extension/header hints)
//! - per-media-type size caps
//! - TTL cleanup
//! - SSRF-protected URL downloads
//! - filename sanitization
//! - expected MIME validation
//! - path traversal protection for reads/imports

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use crate::ssrf::{SsrfConfig, guarded_fetch};

const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaStage {
    Staging,
    Imported,
}

fn default_media_stage() -> MediaStage {
    MediaStage::Staging
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStoreConfig {
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,
    #[serde(default = "default_max_bytes")]
    pub max_image_bytes: usize,
    #[serde(default = "default_max_bytes")]
    pub max_audio_bytes: usize,
    #[serde(default = "default_max_bytes")]
    pub max_video_bytes: usize,
    #[serde(default = "default_max_bytes")]
    pub max_other_bytes: usize,
}

fn default_ttl_secs() -> u64 {
    DEFAULT_TTL_SECS
}

fn default_max_bytes() -> usize {
    DEFAULT_MAX_BYTES
}

impl Default for MediaStoreConfig {
    fn default() -> Self {
        Self {
            ttl_secs: default_ttl_secs(),
            max_image_bytes: default_max_bytes(),
            max_audio_bytes: default_max_bytes(),
            max_video_bytes: default_max_bytes(),
            max_other_bytes: default_max_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaEntry {
    pub id: String,
    pub filename: String,
    pub path: String,
    pub mime_type: String,
    #[serde(default = "default_media_stage")]
    pub stage: MediaStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_path: Option<String>,
    pub size_bytes: usize,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Debug)]
pub struct MediaStore {
    base_dir: PathBuf,
    staging_dir: PathBuf,
    imported_dir: PathBuf,
    index_path: PathBuf,
    index: RwLock<HashMap<String, MediaEntry>>,
    cfg: MediaStoreConfig,
    ssrf: SsrfConfig,
}

impl MediaStore {
    #[must_use] 
    pub fn new(base_dir: PathBuf, cfg: MediaStoreConfig, ssrf: SsrfConfig) -> Self {
        let _ = std::fs::create_dir_all(&base_dir);
        let staging_dir = base_dir.join("staging");
        let imported_dir = base_dir.join("imports");
        let _ = std::fs::create_dir_all(&staging_dir);
        let _ = std::fs::create_dir_all(&imported_dir);
        let index_path = base_dir.join("index.json");
        let index = std::fs::read_to_string(&index_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<HashMap<String, MediaEntry>>(&raw).ok())
            .unwrap_or_default();
        Self {
            base_dir,
            staging_dir,
            imported_dir,
            index_path,
            index: RwLock::new(index),
            cfg,
            ssrf,
        }
    }

    #[must_use] 
    pub fn from_home(savfox_home: &Path) -> Self {
        Self::new(
            savfox_home.join("media"),
            MediaStoreConfig::default(),
            SsrfConfig::from_env(),
        )
    }

    #[must_use] 
    pub fn from_env_or_default() -> Self {
        let savfox_home = savfox_core::config::find_savfox_home().unwrap_or_else(|_| {
            std::env::var("SAVFOX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".savfox")
                })
        });
        Self::from_home(&savfox_home)
    }

    pub async fn fetch_and_store(
        &self,
        url: &str,
        filename_hint: Option<&str>,
        expected_mime: Option<&str>,
    ) -> Result<MediaEntry, String> {
        self.fetch_to_staging(url, filename_hint, expected_mime, None)
            .await
    }

    pub async fn fetch_to_staging(
        &self,
        url: &str,
        filename_hint: Option<&str>,
        expected_mime: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<MediaEntry, String> {
        let _ = self.prune_expired().await;

        let response = guarded_fetch(url, &self.ssrf)
            .await
            .map_err(|err| format!("blocked download URL: {err}"))?;
        if !response.status.is_success() {
            return Err(format!(
                "download failed: HTTP {} from {}",
                response.status.as_u16(),
                response.final_url
            ));
        }

        let url_name = filename_from_url(&response.final_url);
        let filename = filename_hint
            .map(ToOwned::to_owned)
            .or(url_name)
            .unwrap_or_else(|| "download".to_owned());
        let detected_mime = detect_mime(
            &response.body,
            Some(&filename),
            response.content_type.as_deref(),
        );
        validate_expected_mime(expected_mime, &detected_mime)?;

        self.store_resolved(
            &filename,
            &response.body,
            &detected_mime,
            Some(response.final_url),
            MediaStage::Staging,
            session_id.map(ToOwned::to_owned),
        )
        .await
    }

    pub async fn store_bytes(
        &self,
        filename: &str,
        data: &[u8],
        mime_hint: Option<&str>,
    ) -> Result<MediaEntry, String> {
        self.store_bytes_to_staging(filename, data, mime_hint, None)
            .await
    }

    pub async fn store_bytes_to_staging(
        &self,
        filename: &str,
        data: &[u8],
        mime_hint: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<MediaEntry, String> {
        let _ = self.prune_expired().await;
        let detected = detect_mime(data, Some(filename), mime_hint);
        self.store_resolved(
            filename,
            data,
            &detected,
            None,
            MediaStage::Staging,
            session_id.map(ToOwned::to_owned),
        )
        .await
    }

    pub async fn read(&self, id: &str) -> Result<(MediaEntry, Vec<u8>), String> {
        let maybe_entry = self.index.read().await.get(id).cloned();
        let entry = maybe_entry.ok_or_else(|| format!("media not found: {id}"))?;
        if is_expired(&entry) {
            self.delete(id).await;
            return Err("media expired".to_owned());
        }

        let path = self.validated_managed_path(&entry.path).await?;
        let data = tokio::fs::read(&path)
            .await
            .map_err(|err| format!("failed to read media file: {err}"))?;
        Ok((entry, data))
    }

    pub async fn delete(&self, id: &str) {
        let removed = self.index.write().await.remove(id);
        if let Some(entry) = removed {
            if let Ok(path) = self.validated_managed_path(&entry.path).await {
                let _ = tokio::fs::remove_file(path).await;
            }
            let _ = self.persist_index().await;
        }
    }

    pub async fn list(&self) -> Vec<MediaEntry> {
        self.index.read().await.values().cloned().collect()
    }

    pub async fn list_staging(&self, session_id: Option<&str>) -> Vec<MediaEntry> {
        self.index
            .read()
            .await
            .values()
            .filter(|entry| entry.stage == MediaStage::Staging)
            .filter(|entry| {
                session_id
                    .map(|needle| entry.session_id.as_deref() == Some(needle))
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    pub async fn import_from_staging(
        &self,
        id: &str,
        workspace_dir: &Path,
    ) -> Result<PathBuf, String> {
        let entry = self
            .index
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| format!("media not found: {id}"))?;
        if entry.stage != MediaStage::Staging {
            return Err(format!("media {id} is not in staging"));
        }
        if is_expired(&entry) {
            self.delete(id).await;
            return Err("media expired".to_owned());
        }

        let source_path = self.validated_managed_path(&entry.path).await?;
        let workspace_root = if workspace_dir.is_absolute() {
            workspace_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|err| format!("failed to resolve current directory: {err}"))?
                .join(workspace_dir)
        };
        let media_dir = workspace_root.join("media");
        tokio::fs::create_dir_all(&media_dir)
            .await
            .map_err(|err| format!("failed to create workspace media dir: {err}"))?;

        let safe_filename = sanitize_filename(&entry.filename);
        let target_path = media_dir.join(format!("{id}-{safe_filename}"));
        tokio::fs::copy(&source_path, &target_path)
            .await
            .map_err(|err| format!("failed to import staged media: {err}"))?;
        let _ = set_read_only(&target_path, false).await;

        if let Some(current) = self.index.write().await.get_mut(id) {
            current.imported_path = Some(target_path.to_string_lossy().to_string());
        }
        self.persist_index().await?;
        Ok(target_path)
    }

    pub async fn cleanup_staging_for_session(&self, session_id: &str) -> usize {
        let needle = session_id.trim();
        if needle.is_empty() {
            return 0;
        }

        let matches: Vec<MediaEntry> = {
            self.index
                .read()
                .await
                .values()
                .filter(|entry| {
                    entry.stage == MediaStage::Staging
                        && entry.session_id.as_deref() == Some(needle)
                })
                .cloned()
                .collect()
        };
        if matches.is_empty() {
            return 0;
        }

        {
            let mut index = self.index.write().await;
            for entry in &matches {
                index.remove(&entry.id);
            }
        }
        for entry in &matches {
            if let Ok(path) = self.validated_managed_path(&entry.path).await {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        let _ = self.persist_index().await;
        matches.len()
    }

    pub async fn prune_expired(&self) -> usize {
        let now = crate::json_store::now_ms();
        let expired: Vec<MediaEntry> = {
            self.index
                .read()
                .await
                .values()
                .filter(|entry| entry.expires_at_ms <= now)
                .cloned()
                .collect()
        };
        if expired.is_empty() {
            return 0;
        }

        {
            let mut index = self.index.write().await;
            for entry in &expired {
                index.remove(&entry.id);
            }
        }
        for entry in &expired {
            if let Ok(path) = self.validated_managed_path(&entry.path).await {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        let _ = self.persist_index().await;
        info!(count = expired.len(), "pruned expired media entries");
        expired.len()
    }

    async fn store_resolved(
        &self,
        filename: &str,
        data: &[u8],
        mime_type: &str,
        source_url: Option<String>,
        stage: MediaStage,
        session_id: Option<String>,
    ) -> Result<MediaEntry, String> {
        let size_limit = self.size_limit_for_mime(mime_type);
        if data.len() > size_limit {
            return Err(format!(
                "media too large: {} bytes (limit {} for {})",
                data.len(),
                size_limit,
                mime_type
            ));
        }

        let id = Uuid::now_v7().to_string();
        let safe_name = with_mime_extension(&sanitize_filename(filename), mime_type);
        let ext = extension_for_mime(mime_type);
        let file_dir = match stage {
            MediaStage::Staging => &self.staging_dir,
            MediaStage::Imported => &self.imported_dir,
        };
        let file_path = file_dir.join(format!("{id}.{ext}"));
        tokio::fs::write(&file_path, data)
            .await
            .map_err(|err| format!("failed to write media file: {err}"))?;
        if stage == MediaStage::Staging {
            let _ = set_read_only(&file_path, true).await;
        }

        let created_at_ms = crate::json_store::now_ms();
        let expires_at_ms =
            created_at_ms.saturating_add(Duration::from_secs(self.cfg.ttl_secs).as_millis() as u64);
        let entry = MediaEntry {
            id: id.clone(),
            filename: safe_name,
            path: file_path.to_string_lossy().to_string(),
            mime_type: mime_type.to_owned(),
            stage,
            session_id,
            imported_path: None,
            size_bytes: data.len(),
            created_at_ms,
            expires_at_ms,
            source_url,
        };

        self.index.write().await.insert(id, entry.clone());
        self.persist_index().await?;
        Ok(entry)
    }

    async fn persist_index(&self) -> Result<(), String> {
        let snapshot = self.index.read().await.clone();
        let raw = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| format!("failed to serialize media index: {err}"))?;
        tokio::fs::write(&self.index_path, raw)
            .await
            .map_err(|err| format!("failed to persist media index: {err}"))
    }

    fn size_limit_for_mime(&self, mime: &str) -> usize {
        if mime.starts_with("image/") {
            self.cfg.max_image_bytes
        } else if mime.starts_with("audio/") {
            self.cfg.max_audio_bytes
        } else if mime.starts_with("video/") {
            self.cfg.max_video_bytes
        } else {
            self.cfg.max_other_bytes
        }
    }

    async fn validated_managed_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        let candidate = PathBuf::from(raw_path);
        let canonical = tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|err| format!("failed to resolve media path: {err}"))?;
        let base = tokio::fs::canonicalize(&self.base_dir)
            .await
            .map_err(|err| format!("failed to resolve media base dir: {err}"))?;
        if canonical.starts_with(&base) {
            Ok(canonical)
        } else {
            Err("blocked media path outside managed directory".to_owned())
        }
    }
}

async fn set_read_only(path: &Path, read_only: bool) -> Result<(), String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let metadata = std::fs::metadata(&path)
            .map_err(|err| format!("failed to read file metadata: {err}"))?;
        let mut permissions = metadata.permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(&path, permissions)
            .map_err(|err| format!("failed to update permissions: {err}"))?;
        Ok(())
    })
    .await
    .map_err(|err| format!("failed to join permission task: {err}"))?
}

fn is_expired(entry: &MediaEntry) -> bool {
    entry.expires_at_ms <= crate::json_store::now_ms()
}

fn filename_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn extension_for_mime(mime: &str) -> &'static str {
    mime_guess::get_mime_extensions_str(mime)
        .and_then(|exts| exts.first().copied())
        .unwrap_or("bin")
}

fn with_mime_extension(filename: &str, mime: &str) -> String {
    let ext = extension_for_mime(mime);
    if filename.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        filename.to_owned()
    } else {
        format!("{filename}.{ext}")
    }
}

#[must_use] 
pub fn sanitize_filename(name: &str) -> String {
    let leaf = Path::new(name)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("file");
    let mut out = String::with_capacity(leaf.len());
    for ch in leaf.chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
        out.push(if keep { ch } else { '_' });
    }
    let out = out.trim_matches('.').trim_matches('_').to_owned();
    if out.is_empty() {
        "file".to_owned()
    } else {
        out
    }
}

fn normalize_mime_hint(mime: &str) -> Option<String> {
    let value = mime.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    let essence = value.split(';').next().unwrap_or("").trim().to_owned();
    if essence.is_empty() {
        None
    } else {
        Some(essence)
    }
}

fn magic_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.len() >= 3 && data[0..3] == [0xFF, 0xD8, 0xFF] {
        return Some("image/jpeg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if data.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if data.starts_with(b"ID3") {
        return Some("audio/mpeg");
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        if data.windows(3).any(|w| w == b"M4A") || data.windows(4).any(|w| w == b"mp4a") {
            return Some("audio/mp4");
        }
        return Some("video/mp4");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    if data.starts_with(b"OggS") {
        return Some("audio/ogg");
    }
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some("video/webm");
    }
    None
}

pub fn detect_mime(
    data: &[u8],
    filename_hint: Option<&str>,
    content_type_hint: Option<&str>,
) -> String {
    if let Some(mime) = magic_mime(data) {
        return mime.to_owned();
    }
    if let Some(hint) = content_type_hint.and_then(normalize_mime_hint) {
        return hint;
    }
    if let Some(name) = filename_hint
        && let Some(mime) = mime_guess::from_path(name).first_raw() {
            return mime.to_owned();
        }
    "application/octet-stream".to_owned()
}

fn mime_matches(expected: &str, detected: &str) -> bool {
    if expected == detected {
        return true;
    }
    if let Some(prefix) = expected.strip_suffix("/*") {
        return detected.starts_with(&format!("{prefix}/"));
    }
    false
}

pub fn validate_expected_mime(expected: Option<&str>, detected: &str) -> Result<(), String> {
    let Some(expected) = expected.and_then(normalize_mime_hint) else {
        return Ok(());
    };
    if mime_matches(&expected, detected) {
        return Ok(());
    }
    warn!(
        expected = %expected,
        detected = %detected,
        "media store rejected MIME mismatch"
    );
    Err(format!(
        "unexpected content type: expected {expected}, detected {detected}"
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn sanitize_filename_blocks_traversal_and_bad_chars() {
        assert_eq!(sanitize_filename("../secret.txt"), "secret.txt");
        let sanitized = sanitize_filename("a:b*c?.png");
        assert!(sanitized.ends_with(".png"));
        assert!(!sanitized.contains(':'));
        assert!(!sanitized.contains('*'));
        assert!(!sanitized.contains('?'));
    }

    #[test]
    fn magic_detection_precedes_hint() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00";
        let mime = detect_mime(png, Some("x.bin"), Some("application/octet-stream"));
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn expected_mime_allows_wildcard() {
        assert!(validate_expected_mime(Some("image/*"), "image/png").is_ok());
        assert!(validate_expected_mime(Some("audio/*"), "video/mp4").is_err());
    }

    #[tokio::test]
    async fn store_enforces_size_caps() {
        let tmp = tempdir().expect("tempdir");
        let cfg = MediaStoreConfig {
            max_image_bytes: 8,
            ..MediaStoreConfig::default()
        };
        let store = MediaStore::new(tmp.path().join("media"), cfg, SsrfConfig::default());
        let data = b"\x89PNG\r\n\x1a\n123456789";
        let err = store
            .store_bytes("avatar.png", data, Some("image/png"))
            .await
            .expect_err("should fail size cap");
        assert!(err.contains("media too large"));
    }

    #[tokio::test]
    async fn prune_expired_deletes_files() {
        let tmp = tempdir().expect("tempdir");
        let cfg = MediaStoreConfig {
            ttl_secs: 0,
            ..MediaStoreConfig::default()
        };
        let store = MediaStore::new(tmp.path().join("media"), cfg, SsrfConfig::default());
        let entry = store
            .store_bytes("note.txt", b"hello", Some("text/plain"))
            .await
            .expect("store");
        let removed = store.prune_expired().await;
        assert_eq!(removed, 1);
        assert!(!std::path::Path::new(&entry.path).exists());
    }

    #[tokio::test]
    async fn inbound_media_lands_in_staging_and_can_be_imported() {
        let tmp = tempdir().expect("tempdir");
        let store = MediaStore::new(
            tmp.path().join("media"),
            MediaStoreConfig::default(),
            SsrfConfig::default(),
        );
        let entry = store
            .store_bytes_to_staging(
                "hello.txt",
                b"hello",
                Some("text/plain"),
                Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e171"),
            )
            .await
            .expect("store to staging");

        assert_eq!(entry.stage, MediaStage::Staging);
        assert!(
            std::path::Path::new(&entry.path)
                .components()
                .any(|c| c.as_os_str() == "staging")
        );

        let workspace = tmp.path().join("workspace");
        let imported = store
            .import_from_staging(&entry.id, &workspace)
            .await
            .expect("import from staging");
        assert!(imported.exists());
        let imported_data = tokio::fs::read(&imported).await.expect("imported read");
        assert_eq!(imported_data, b"hello");
    }

    #[tokio::test]
    async fn session_cleanup_removes_staged_entries() {
        let tmp = tempdir().expect("tempdir");
        let store = MediaStore::new(
            tmp.path().join("media"),
            MediaStoreConfig::default(),
            SsrfConfig::default(),
        );
        let a = store
            .store_bytes_to_staging(
                "a.txt",
                b"a",
                Some("text/plain"),
                Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e150"),
            )
            .await
            .expect("store A");
        let _b = store
            .store_bytes_to_staging(
                "b.txt",
                b"b",
                Some("text/plain"),
                Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e151"),
            )
            .await
            .expect("store B");

        let removed = store
            .cleanup_staging_for_session("0194f7b3-1d7b-7c40-ae3d-95b6ef93e150")
            .await;
        assert_eq!(removed, 1);
        assert!(!std::path::Path::new(&a.path).exists());
        let remaining = store.list_staging(None).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].session_id.as_deref(),
            Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e151")
        );
    }

    #[tokio::test]
    async fn read_blocks_path_traversal_outside_media_root() {
        let tmp = tempdir().expect("tempdir");
        let store = MediaStore::new(
            tmp.path().join("media"),
            MediaStoreConfig::default(),
            SsrfConfig::default(),
        );
        let entry = store
            .store_bytes_to_staging(
                "safe.txt",
                b"safe",
                Some("text/plain"),
                Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e152"),
            )
            .await
            .expect("store");

        {
            let mut index = store.index.write().await;
            if let Some(current) = index.get_mut(&entry.id) {
                current.path = tmp.path().join("outside.txt").to_string_lossy().to_string();
            }
        }
        tokio::fs::write(tmp.path().join("outside.txt"), b"outside")
            .await
            .expect("write outside");
        let err = store
            .read(&entry.id)
            .await
            .expect_err("must block traversal");
        assert!(err.contains("outside managed directory"));
    }
}
