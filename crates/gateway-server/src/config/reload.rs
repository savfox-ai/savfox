//! Live config reload -- watches config file for changes and triggers reload.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};
use tracing::{info, warn};

/// Config change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeEvent {
    pub changed_keys: Vec<String>,
    pub timestamp: u64,
}

/// Config reload service
#[derive(Debug)]
pub struct ConfigReloadService {
    config_path: PathBuf,
    current: RwLock<Value>,
    change_tx: broadcast::Sender<ConfigChangeEvent>,
}

impl ConfigReloadService {
    #[must_use] 
    pub fn new(config_path: PathBuf) -> Self {
        let (change_tx, _) = broadcast::channel(16);
        Self {
            config_path,
            current: RwLock::new(Value::Null),
            change_tx,
        }
    }

    /// Subscribe to config change events
    pub fn subscribe(&self) -> broadcast::Receiver<ConfigChangeEvent> {
        self.change_tx.subscribe()
    }

    /// Load current config from file
    pub async fn load(&self) -> Result<Value, String> {
        let content = tokio::fs::read_to_string(&self.config_path)
            .await
            .map_err(|e| format!("Failed to read config: {e}"))?;

        let value: Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse config JSON: {e}"))?;

        let mut current = self.current.write().await;
        *current = value.clone();
        Ok(value)
    }

    /// Reload config and detect changes
    pub async fn reload(&self) -> Result<ConfigChangeEvent, String> {
        let old = self.current.read().await.clone();
        let new = self.load().await?;

        let changed_keys = diff_keys("", &old, &new);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let event = ConfigChangeEvent {
            changed_keys: changed_keys.clone(),
            timestamp: now,
        };

        if !changed_keys.is_empty() {
            info!(changes = ?changed_keys, "config reloaded with changes");
            let _ = self.change_tx.send(event.clone());
        }

        Ok(event)
    }

    /// Get current config value
    pub async fn get(&self) -> Value {
        self.current.read().await.clone()
    }

    /// Start watching for config file changes (polling-based for cross-platform compatibility)
    pub fn start_watching(self: Arc<Self>, interval_secs: u64) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut last_modified = get_mtime(&self.config_path).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

            loop {
                interval.tick().await;
                let current_mtime = get_mtime(&self.config_path).await;
                if current_mtime != last_modified {
                    last_modified = current_mtime;
                    match self.reload().await {
                        Ok(event) => {
                            if !event.changed_keys.is_empty() {
                                info!(
                                    "config file changed, reloaded {} keys",
                                    event.changed_keys.len()
                                );
                            }
                        }
                        Err(e) => warn!("failed to reload config: {e}"),
                    }
                }
            }
        })
    }
}

/// Get file modification time as epoch secs
async fn get_mtime(path: &PathBuf) -> u64 {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Find keys that differ between two JSON values
fn diff_keys(prefix: &str, old: &Value, new: &Value) -> Vec<String> {
    let mut changed = Vec::new();

    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let all_keys: HashSet<_> = a.keys().chain(b.keys()).collect();
            for key in all_keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                let old_val = a.get(key).unwrap_or(&Value::Null);
                let new_val = b.get(key).unwrap_or(&Value::Null);
                if old_val != new_val {
                    if old_val.is_object() && new_val.is_object() {
                        changed.extend(diff_keys(&path, old_val, new_val));
                    } else {
                        changed.push(path);
                    }
                }
            }
        }
        _ => {
            if old != new && !prefix.is_empty() {
                changed.push(prefix.to_owned());
            }
        }
    }

    changed
}
