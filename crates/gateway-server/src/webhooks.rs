//! Inbound webhook system  - accepts HTTP webhooks that trigger agent actions.
//!
//! Webhooks are configured with a URL path, secret, target agent, and payload template.
//! Incoming webhooks are verified via HMAC-SHA256 signature and rate-limited.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;
use tracing::info;

type HmacSha256 = Hmac<Sha256>;

/// Webhook configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub target_agent: String,
    pub enabled: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub payload_template: Option<String>,
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
    pub created_at: u64,
}

/// Webhook execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookExecution {
    pub webhook_id: String,
    pub timestamp: u64,
    pub success: bool,
    pub status_code: u16,
    pub error: Option<String>,
}

/// Webhook store
#[derive(Debug)]
pub struct WebhookStore {
    path: PathBuf,
    webhooks: RwLock<HashMap<String, WebhookConfig>>,
    executions: RwLock<Vec<WebhookExecution>>,
}

impl WebhookStore {
    pub fn new(savfox_home: &Path) -> Self {
        let path = savfox_home.join("gateway").join("webhooks.json");
        Self {
            path,
            webhooks: RwLock::new(HashMap::new()),
            executions: RwLock::new(Vec::new()),
        }
    }

    pub async fn load(&self) {
        if let Ok(content) = tokio::fs::read_to_string(&self.path).await {
            if let Ok(webhooks) = serde_json::from_str::<HashMap<String, WebhookConfig>>(&content) {
                *self.webhooks.write().await = webhooks;
            }
        }
    }

    async fn save(&self) -> Result<(), String> {
        let webhooks = self.webhooks.read().await;
        let json = serde_json::to_string_pretty(&*webhooks)
            .map_err(|e| format!("serialize error: {e}"))?;
        if let Some(parent) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(&self.path, json)
            .await
            .map_err(|e| format!("write error: {e}"))
    }

    pub async fn list(&self) -> Vec<WebhookConfig> {
        self.webhooks.read().await.values().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<WebhookConfig> {
        self.webhooks.read().await.get(id).cloned()
    }

    pub async fn create(&self, mut config: WebhookConfig) -> Result<WebhookConfig, String> {
        if config.id.is_empty() {
            config.id = uuid::Uuid::now_v7().to_string();
        }
        config.created_at = now_secs();

        let id = config.id.clone();
        self.webhooks
            .write()
            .await
            .insert(id.clone(), config.clone());
        self.save().await?;
        info!(id = %id, name = %config.name, "webhook created");
        Ok(config)
    }

    pub async fn update(
        &self,
        id: &str,
        updates: serde_json::Value,
    ) -> Result<WebhookConfig, String> {
        let mut webhooks = self.webhooks.write().await;
        let webhook = webhooks
            .get_mut(id)
            .ok_or_else(|| format!("Webhook not found: {id}"))?;

        if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
            webhook.name = name.to_string();
        }
        if let Some(secret) = updates.get("secret").and_then(|v| v.as_str()) {
            webhook.secret = secret.to_string();
        }
        if let Some(agent) = updates.get("target_agent").and_then(|v| v.as_str()) {
            webhook.target_agent = agent.to_string();
        }
        if let Some(enabled) = updates.get("enabled").and_then(|v| v.as_bool()) {
            webhook.enabled = enabled;
        }

        let result = webhook.clone();
        drop(webhooks);
        self.save().await?;
        Ok(result)
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let mut webhooks = self.webhooks.write().await;
        webhooks
            .remove(id)
            .ok_or_else(|| format!("Webhook not found: {id}"))?;
        drop(webhooks);
        self.save().await?;
        info!(id = %id, "webhook deleted");
        Ok(())
    }

    /// Verify webhook signature (HMAC-SHA256 with constant-time comparison)
    pub fn verify_signature(secret: &str, payload: &[u8], signature: &str) -> bool {
        let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(payload);
        let expected = hex::encode(mac.finalize().into_bytes());
        // Strip optional "sha256=" prefix (GitHub-style)
        let sig = signature.strip_prefix("sha256=").unwrap_or(signature);
        // Constant-time comparison
        if expected.len() != sig.len() {
            return false;
        }
        expected
            .as_bytes()
            .iter()
            .zip(sig.as_bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    /// Record a webhook execution
    pub async fn record_execution(&self, exec: WebhookExecution) {
        let mut execs = self.executions.write().await;
        execs.push(exec);
        // Keep only last 1000 executions
        if execs.len() > 1000 {
            let drain_count = execs.len() - 1000;
            execs.drain(..drain_count);
        }
    }

    pub async fn recent_executions(
        &self,
        webhook_id: Option<&str>,
        limit: usize,
    ) -> Vec<WebhookExecution> {
        let execs = self.executions.read().await;
        execs
            .iter()
            .rev()
            .filter(|e| webhook_id.is_none() || webhook_id == Some(e.webhook_id.as_str()))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
