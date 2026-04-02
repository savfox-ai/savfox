use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::{info, warn};

/// Unique identifier for a subagent instance.
pub type SubagentId = String;

/// Lifecycle status of a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentStatus {
    /// Spawned but not yet started processing.
    Pending,
    /// Actively running.
    Running,
    /// Completed successfully.
    Completed { result_summary: String },
    /// Failed with an error.
    Failed { error: String },
    /// Cancelled by parent or system.
    Cancelled,
}

/// Metadata about a single subagent instance.
#[derive(Debug, Clone)]
pub struct SubagentEntry {
    /// Unique subagent ID.
    pub id: SubagentId,
    /// Parent agent session ID (if nested).
    pub parent_id: Option<String>,
    /// Model used by this subagent.
    pub model: String,
    /// Human-readable task description.
    pub task_description: String,
    /// Current status.
    pub status: SubagentStatus,
    /// Nesting depth (0 = top-level, 1 = child, 2 = grandchild, etc.).
    pub depth: u32,
    /// When the subagent was created.
    pub created_at: Instant,
    /// Approximate token budget allocated.
    pub token_budget: usize,
    /// Approximate tokens consumed so far.
    pub tokens_used: usize,
}

/// Registry that tracks all active and recent subagent instances.
///
/// Session-safe: uses an internal `RwLock` so it can be shared across tasks.
#[derive(Debug)]
pub struct SubagentRegistry {
    entries: Arc<RwLock<HashMap<SubagentId, SubagentEntry>>>,
    /// Maximum nesting depth allowed. Prevents runaway recursive spawns.
    max_depth: u32,
    /// Maximum concurrent subagents.
    max_concurrent: usize,
}

impl SubagentRegistry {
    /// Create a new registry with default limits.
    #[must_use] 
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_depth: 3,
            max_concurrent: 8,
        }
    }

    /// Create with custom limits.
    #[must_use] 
    pub fn with_limits(max_depth: u32, max_concurrent: usize) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_depth,
            max_concurrent,
        }
    }

    /// Register a new subagent. Returns `Err` if limits are exceeded.
    pub async fn register(
        &self,
        id: SubagentId,
        parent_id: Option<String>,
        model: String,
        task_description: String,
        depth: u32,
        token_budget: usize,
    ) -> Result<(), SubagentRegistryError> {
        // Check depth limit.
        if depth > self.max_depth {
            return Err(SubagentRegistryError::MaxDepthExceeded {
                max: self.max_depth,
                requested: depth,
            });
        }

        // Check concurrency limit.
        let entries = self.entries.read().await;
        let active_count = entries
            .values()
            .filter(|e| matches!(e.status, SubagentStatus::Pending | SubagentStatus::Running))
            .count();
        drop(entries);

        if active_count >= self.max_concurrent {
            return Err(SubagentRegistryError::MaxConcurrentExceeded {
                max: self.max_concurrent,
            });
        }

        let entry = SubagentEntry {
            id: id.clone(),
            parent_id,
            model,
            task_description,
            status: SubagentStatus::Pending,
            depth,
            created_at: Instant::now(),
            token_budget,
            tokens_used: 0,
        };

        self.entries.write().await.insert(id.clone(), entry);
        info!(subagent_id = %id, "subagent registered");
        Ok(())
    }

    /// Update the status of a subagent.
    pub async fn update_status(&self, id: &str, status: SubagentStatus) {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(id) {
            info!(
                subagent_id = %id,
                old_status = ?entry.status,
                new_status = ?status,
                "subagent status changed"
            );
            entry.status = status;
        } else {
            warn!(subagent_id = %id, "attempted to update unknown subagent");
        }
    }

    /// Record token usage for a subagent.
    pub async fn record_tokens(&self, id: &str, tokens: usize) {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(id) {
            entry.tokens_used += tokens;
        }
    }

    /// Mark a subagent as completed with a result summary.
    pub async fn complete(&self, id: &str, result_summary: String) {
        self.update_status(id, SubagentStatus::Completed { result_summary })
            .await;
    }

    /// Mark a subagent as failed.
    pub async fn fail(&self, id: &str, error: String) {
        self.update_status(id, SubagentStatus::Failed { error })
            .await;
    }

    /// Cancel a subagent.
    pub async fn cancel(&self, id: &str) {
        self.update_status(id, SubagentStatus::Cancelled).await;
    }

    /// Get a snapshot of a subagent entry.
    pub async fn get(&self, id: &str) -> Option<SubagentEntry> {
        self.entries.read().await.get(id).cloned()
    }

    /// List all active (pending or running) subagents.
    pub async fn list_active(&self) -> Vec<SubagentEntry> {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| matches!(e.status, SubagentStatus::Pending | SubagentStatus::Running))
            .cloned()
            .collect()
    }

    /// List all subagents (including completed/failed).
    pub async fn list_all(&self) -> Vec<SubagentEntry> {
        self.entries.read().await.values().cloned().collect()
    }

    /// List children of a specific parent.
    pub async fn children_of(&self, parent_id: &str) -> Vec<SubagentEntry> {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| e.parent_id.as_deref() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// Total tokens used across all active subagents.
    pub async fn total_tokens_used(&self) -> usize {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| matches!(e.status, SubagentStatus::Pending | SubagentStatus::Running))
            .map(|e| e.tokens_used)
            .sum()
    }

    /// Prune completed/failed/cancelled entries older than the given duration.
    pub async fn prune_older_than(&self, age: std::time::Duration) {
        let cutoff = Instant::now() - age;
        let mut entries = self.entries.write().await;
        entries.retain(|_, e| {
            // Keep active entries and recent ones.
            matches!(e.status, SubagentStatus::Pending | SubagentStatus::Running)
                || e.created_at > cutoff
        });
    }

    /// Number of currently active subagents.
    pub async fn active_count(&self) -> usize {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| matches!(e.status, SubagentStatus::Pending | SubagentStatus::Running))
            .count()
    }
}

impl Default for SubagentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors returned by the subagent registry.
#[derive(Debug, thiserror::Error)]
pub enum SubagentRegistryError {
    #[error("maximum nesting depth exceeded (max: {max}, requested: {requested})")]
    MaxDepthExceeded { max: u32, requested: u32 },
    #[error("maximum concurrent subagents exceeded (max: {max})")]
    MaxConcurrentExceeded { max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_complete() {
        let registry = SubagentRegistry::new();

        registry
            .register(
                "sa-1".into(),
                None,
                "gpt-5".into(),
                "summarize doc".into(),
                0,
                4096,
            )
            .await
            .unwrap();

        assert_eq!(registry.active_count().await, 1);

        registry
            .update_status("sa-1", SubagentStatus::Running)
            .await;
        registry.record_tokens("sa-1", 500).await;
        registry
            .complete("sa-1", "Document summarized.".into())
            .await;

        assert_eq!(registry.active_count().await, 0);

        let entry = registry.get("sa-1").await.unwrap();
        assert!(matches!(entry.status, SubagentStatus::Completed { .. }));
        assert_eq!(entry.tokens_used, 500);
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let registry = SubagentRegistry::with_limits(2, 10);

        let result = registry
            .register(
                "sa-deep".into(),
                None,
                "gpt-5".into(),
                "task".into(),
                3,
                1000,
            )
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn concurrency_limit_enforced() {
        let registry = SubagentRegistry::with_limits(10, 2);

        registry
            .register("sa-1".into(), None, "m".into(), "t".into(), 0, 100)
            .await
            .unwrap();
        registry
            .register("sa-2".into(), None, "m".into(), "t".into(), 0, 100)
            .await
            .unwrap();

        let result = registry
            .register("sa-3".into(), None, "m".into(), "t".into(), 0, 100)
            .await;
        assert!(result.is_err());
    }
}
