//! Resilience utilities  - auto-reconnect, exponential backoff, circuit breaker.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Exponential backoff configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackoffConfig {
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: f64,
    pub max_retries: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay_ms: 1000,
            max_delay_ms: 60_000,
            multiplier: 2.0,
            max_retries: 10,
        }
    }
}

impl BackoffConfig {
    /// Calculate delay for a given attempt number
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay = self.initial_delay_ms as f64 * self.multiplier.powi(attempt as i32);
        let capped = delay.min(self.max_delay_ms as f64) as u64;
        Duration::from_millis(capped)
    }
}

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Normal operation
    Closed,
    /// Failing, reject requests
    Open,
    /// Testing if recovery happened
    HalfOpen,
}

/// Circuit breaker for failing services
#[derive(Debug)]
pub struct CircuitBreaker {
    pub name: String,
    failure_count: AtomicU64,
    success_count: AtomicU64,
    is_open: AtomicBool,
    failure_threshold: u64,
    success_threshold: u64,
    reset_timeout_ms: u64,
    last_failure_ms: AtomicU64,
}

impl CircuitBreaker {
    pub fn new(
        name: String,
        failure_threshold: u64,
        success_threshold: u64,
        reset_timeout_ms: u64,
    ) -> Self {
        Self {
            name,
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            is_open: AtomicBool::new(false),
            failure_threshold,
            success_threshold,
            reset_timeout_ms,
            last_failure_ms: AtomicU64::new(0),
        }
    }

    /// Check if request should be allowed
    pub fn allow_request(&self) -> bool {
        if !self.is_open.load(Ordering::Relaxed) {
            return true;
        }
        // Check if reset timeout has passed (half-open)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last = self.last_failure_ms.load(Ordering::Relaxed);
        now.saturating_sub(last) >= self.reset_timeout_ms
    }

    /// Record a successful request
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        let prev = self.success_count.fetch_add(1, Ordering::Relaxed);
        if self.is_open.load(Ordering::Relaxed) && prev + 1 >= self.success_threshold {
            self.is_open.store(false, Ordering::Relaxed);
            self.success_count.store(0, Ordering::Relaxed);
            info!(name = %self.name, "circuit breaker closed  - service recovered");
        }
    }

    /// Record a failed request
    pub fn record_failure(&self) {
        self.success_count.store(0, Ordering::Relaxed);
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_failure_ms.store(now, Ordering::Relaxed);

        if count >= self.failure_threshold && !self.is_open.load(Ordering::Relaxed) {
            self.is_open.store(true, Ordering::Relaxed);
            warn!(name = %self.name, failures = count, "circuit breaker opened  - service failing");
        }
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        if !self.is_open.load(Ordering::Relaxed) {
            return CircuitState::Closed;
        }
        if self.allow_request() {
            CircuitState::HalfOpen
        } else {
            CircuitState::Open
        }
    }
}

/// Auto-reconnect wrapper for channel connections
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    pub backoff: BackoffConfig,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            backoff: BackoffConfig::default(),
        }
    }
}
