use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};
use tracing::info;
use walkdir::WalkDir;

use crate::types::{MemoryChunk, MemoryMetadata, MemorySource};

pub struct MemoryFileSync {
    memory_dir: PathBuf,
    chunk_tokens: usize,
    chunk_overlap: usize,
}

impl MemoryFileSync {
    pub fn new(memory_dir: PathBuf, chunk_tokens: usize, chunk_overlap: usize) -> Self {
        Self {
            memory_dir,
            chunk_tokens,
            chunk_overlap,
        }
    }

    pub fn scan_memory_files(&self) -> Result<Vec<MemoryFileEntry>, std::io::Error> {
        let mut entries = Vec::new();

        if !self.memory_dir.exists() {
            return Ok(entries);
        }

        for entry in WalkDir::new(&self.memory_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "md" || ext == "qmd" || ext == "txt" {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            let hash = Self::hash_content(&content);
                            entries.push(MemoryFileEntry {
                                path: path.to_path_buf(),
                                content,
                                hash,
                            });
                        }
                    }
                }
            }
        }

        info!("Scanned {} memory files", entries.len());
        Ok(entries)
    }

    pub fn chunk_file(&self, entry: &MemoryFileEntry) -> Vec<MemoryChunk> {
        let chunks = chunk_markdown(&entry.content, self.chunk_tokens, self.chunk_overlap);

        chunks
            .into_iter()
            .map(|chunk| MemoryChunk {
                id: uuid::Uuid::new_v4().to_string(),
                source: MemorySource::File,
                content: chunk.content,
                embedding: None,
                metadata: MemoryMetadata {
                    file_path: Some(entry.path.to_string_lossy().to_string()),
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    token_count: chunk.token_count,
                    hash: entry.hash.clone(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect()
    }

    pub fn compute_file_hash(path: &Path) -> Result<String, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::hash_content(&content))
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug, Clone)]
pub struct MemoryFileEntry {
    pub path: PathBuf,
    pub content: String,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct ChunkResult {
    pub content: String,
    pub line_start: usize,
    pub line_end: usize,
    pub token_count: usize,
}

pub fn chunk_markdown(content: &str, max_tokens: usize, overlap: usize) -> Vec<ChunkResult> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut current_chunk = Vec::new();
    let mut current_tokens = 0;
    let mut chunk_start = 0;
    let mut current_line = 0;

    let heading_pattern = regex::Regex::new(r"^#{1,6}\s").ok();

    for (idx, line) in lines.iter().enumerate() {
        let line_tokens = estimate_tokens(line);
        let is_heading = heading_pattern
            .as_ref()
            .map(|p| p.is_match(line))
            .unwrap_or(false);

        if (current_tokens + line_tokens > max_tokens && !current_chunk.is_empty())
            || (is_heading && !current_chunk.is_empty() && current_tokens > max_tokens / 2)
        {
            chunks.push(ChunkResult {
                content: current_chunk.join("\n"),
                line_start: chunk_start + 1,
                line_end: current_line,
                token_count: current_tokens,
            });

            let overlap_lines: Vec<&str> =
                lines[idx.saturating_sub(overlap / 50).max(chunk_start)..idx].to_vec();
            current_chunk = overlap_lines;
            current_tokens = current_chunk.iter().map(|l| estimate_tokens(l)).sum();
            chunk_start = idx.saturating_sub(overlap / 50).max(chunk_start);
        }

        current_chunk.push(*line);
        current_tokens += line_tokens;
        current_line = idx + 1;
    }

    if !current_chunk.is_empty() {
        chunks.push(ChunkResult {
            content: current_chunk.join("\n"),
            line_start: chunk_start + 1,
            line_end: current_line,
            token_count: current_tokens,
        });
    }

    chunks
}

pub fn estimate_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    let word_count = text.split_whitespace().count();

    (char_count / 4).max(word_count / 2).max(1)
}

pub struct SessionFileSync {
    sessions_dir: PathBuf,
}

impl SessionFileSync {
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub fn scan_session_files(&self) -> Result<Vec<SessionFileEntry>, std::io::Error> {
        let mut entries = Vec::new();

        if !self.sessions_dir.exists() {
            return Ok(entries);
        }

        for entry in WalkDir::new(&self.sessions_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "json" || ext == "md" {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            let hash = MemoryFileSync::hash_content(&content);
                            entries.push(SessionFileEntry {
                                path: path.to_path_buf(),
                                content,
                                hash,
                                session_id: extract_session_id(path),
                            });
                        }
                    }
                }
            }
        }

        Ok(entries)
    }

    pub fn build_session_chunks(
        &self,
        entry: &SessionFileEntry,
        chunk_tokens: usize,
    ) -> Vec<MemoryChunk> {
        let chunks = chunk_markdown(&entry.content, chunk_tokens, 50);

        chunks
            .into_iter()
            .map(|chunk| MemoryChunk {
                id: uuid::Uuid::new_v4().to_string(),
                source: MemorySource::Session,
                content: chunk.content,
                embedding: None,
                metadata: MemoryMetadata {
                    file_path: Some(entry.path.to_string_lossy().to_string()),
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    token_count: chunk.token_count,
                    hash: entry.hash.clone(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct SessionFileEntry {
    pub path: PathBuf,
    pub content: String,
    pub hash: String,
    pub session_id: Option<String>,
}

fn extract_session_id(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}
