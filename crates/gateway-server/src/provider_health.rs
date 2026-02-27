//! Provider health checks -- periodic health monitoring for configured LLM providers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::warn;

/// Health status for a provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub provider: String,
    pub status: HealthStatus,
    pub latency_ms: Option<u64>,
    pub error_count: u64,
    pub success_count: u64,
    pub last_check: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

/// Health check service
#[derive(Debug)]
pub struct ProviderHealthService {
    health: RwLock<HashMap<String, ProviderHealth>>,
    #[allow(dead_code)]
    check_interval_secs: u64,
}

impl ProviderHealthService {
    pub fn new(check_interval_secs: u64) -> Self {
        Self {
            health: RwLock::new(HashMap::new()),
            check_interval_secs,
        }
    }

    /// Record a successful API call
    pub async fn record_success(&self, provider: &str, latency_ms: u64) {
        let mut health = self.health.write().await;
        let entry = health
            .entry(provider.to_string())
            .or_insert_with(|| ProviderHealth {
                provider: provider.to_string(),
                status: HealthStatus::Unknown,
                latency_ms: None,
                error_count: 0,
                success_count: 0,
                last_check: now_secs(),
                last_error: None,
            });
        entry.success_count += 1;
        entry.latency_ms = Some(latency_ms);
        entry.last_check = now_secs();

        // Reset to healthy after enough successes
        if entry.error_count == 0 || entry.success_count > entry.error_count * 2 {
            entry.status = HealthStatus::Healthy;
        } else {
            entry.status = HealthStatus::Degraded;
        }
    }

    /// Record a failed API call
    pub async fn record_failure(&self, provider: &str, error_msg: &str) {
        let mut health = self.health.write().await;
        let entry = health
            .entry(provider.to_string())
            .or_insert_with(|| ProviderHealth {
                provider: provider.to_string(),
                status: HealthStatus::Unknown,
                latency_ms: None,
                error_count: 0,
                success_count: 0,
                last_check: now_secs(),
                last_error: None,
            });
        entry.error_count += 1;
        entry.last_check = now_secs();
        entry.last_error = Some(error_msg.to_string());

        if entry.error_count >= 5 {
            entry.status = HealthStatus::Unhealthy;
        } else {
            entry.status = HealthStatus::Degraded;
        }

        warn!(provider = %provider, errors = entry.error_count, "provider health degraded");
    }

    /// Get health status for all providers
    pub async fn get_all(&self) -> HashMap<String, ProviderHealth> {
        self.health.read().await.clone()
    }

    /// Get health status for a specific provider
    pub async fn get(&self, provider: &str) -> Option<ProviderHealth> {
        self.health.read().await.get(provider).cloned()
    }

    /// Check if a provider is healthy enough to use
    pub async fn is_available(&self, provider: &str) -> bool {
        self.health
            .read()
            .await
            .get(provider)
            .map(|h| h.status != HealthStatus::Unhealthy)
            .unwrap_or(true) // Unknown = available
    }

    /// Reset health status for a provider
    pub async fn reset(&self, provider: &str) {
        self.health.write().await.remove(provider);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
