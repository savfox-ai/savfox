use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

#[derive(Deserialize)]
struct MemoryArgs {
    /// One of "search", "get", "add", "delete", "list".
    action: String,
    /// Free-text query (used by "search").
    #[serde(default)]
    query: Option<String>,
    /// Entry key (used by "get", "add", "delete").
    #[serde(default)]
    key: Option<String>,
    /// Entry content (used by "add").
    #[serde(default)]
    content: Option<String>,
    /// Tags to associate with the entry (used by "add").
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Maximum number of results to return.
    #[serde(default = "defaults::limit")]
    limit: usize,
}

mod defaults {
    pub fn limit() -> usize {
        10
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryEntry {
    key: String,
    content: String,
    tags: Vec<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct MemoryStore {
    entries: Vec<MemoryEntry>,
}

impl MemoryStore {
    fn storage_path() -> PathBuf {
        let home = std::env::var("SAVFOX_HOME").unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".savfox")
                .to_string_lossy()
                .into_owned()
        });
        PathBuf::from(home).join("memory").join("entries.json")
    }

    fn load() -> Self {
        let path = Self::storage_path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self) -> Result<(), String> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// BM25-inspired term-frequency search across entry key, content, and tags.
    fn search(&self, query: &str, limit: usize) -> Vec<&MemoryEntry> {
        let query_terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();

        if query_terms.is_empty() {
            return self.entries.iter().take(limit).collect();
        }

        let k1 = 1.2_f64;
        let b = 0.75_f64;
        let avg_len = 100.0_f64; // approximate average document length

        let mut scored: Vec<(&MemoryEntry, f64)> = self
            .entries
            .iter()
            .map(|entry| {
                let text = format!("{} {} {}", entry.key, entry.content, entry.tags.join(" "))
                    .to_lowercase();
                let doc_len = text.split_whitespace().count() as f64;

                let score: f64 = query_terms
                    .iter()
                    .map(|term| {
                        let tf = text.matches(term.as_str()).count() as f64;
                        // Simplified BM25 without IDF (single-doc scoring)
                        (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * doc_len / avg_len))
                    })
                    .sum();

                (entry, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(limit)
            .map(|(entry, _)| entry)
            .collect()
    }
}

/// Persistent key-value memory store with BM25-inspired text search.
///
/// Supports actions: `search`, `get`, `add`, `delete`, `list`.
/// Entries are stored as JSON on disk under `$SAVFOX_HOME/memory/entries.json`.
pub struct MemoryHandler;

#[async_trait::async_trait]
impl ToolHandler for MemoryHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("MemoryHandler received unsupported payload"),
        };
        let args: MemoryArgs = parse_arguments(&arguments)?;
        match args.action.as_str() {
            "search" => {
                let query = args.query.as_deref().unwrap_or("");
                let store = MemoryStore::load();
                let results = store.search(query, args.limit);
                let json = serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string());
                Ok(ToolOutput::ok(json))
            }
            "get" => {
                let key = args.key.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "'key' is required for get action".to_string(),
                    )
                })?;
                let store = MemoryStore::load();
                let entry = store.entries.iter().find(|e| e.key == key);
                let json = serde_json::to_string(&entry).unwrap_or_else(|_| "null".to_string());
                Ok(ToolOutput::ok(json))
            }
            "add" => {
                let key = args.key.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "'key' is required for add action".to_string(),
                    )
                })?;
                let content = args.content.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "'content' is required for add action".to_string(),
                    )
                })?;
                let now = chrono::Utc::now().to_rfc3339();
                let mut store = MemoryStore::load();

                // Update if the key already exists, otherwise insert a new entry.
                if let Some(existing) = store.entries.iter_mut().find(|e| e.key == key) {
                    existing.content = content.to_string();
                    existing.tags = args.tags.unwrap_or_default();
                    existing.updated_at = now;
                } else {
                    store.entries.push(MemoryEntry {
                        key: key.to_string(),
                        content: content.to_string(),
                        tags: args.tags.unwrap_or_default(),
                        created_at: now.clone(),
                        updated_at: now,
                    });
                }

                store.save().map_err(|e| {
                    FunctionCallError::RespondToModel(format!("failed to save memory: {e}"))
                })?;

                Ok(ToolOutput::ok(format!("Memory entry '{key}' saved")))
            }
            "delete" => {
                let key = args.key.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "'key' is required for delete action".to_string(),
                    )
                })?;
                let mut store = MemoryStore::load();
                let before = store.entries.len();
                store.entries.retain(|e| e.key != key);
                let deleted = before - store.entries.len();
                store.save().map_err(|e| {
                    FunctionCallError::RespondToModel(format!("failed to save memory: {e}"))
                })?;

                Ok(ToolOutput::ok(format!(
                    "Deleted {deleted} entries with key '{key}'"
                )))
            }
            "list" => {
                let store = MemoryStore::load();
                let entries: Vec<_> = store.entries.iter().take(args.limit).collect();
                let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());
                Ok(ToolOutput::ok(json))
            }
            other => model_err(format!("unknown memory action: {other}")),
        }
    }
}
