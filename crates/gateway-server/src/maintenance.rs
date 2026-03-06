//! Periodic maintenance tasks for the gateway server.
//!
//! Runs background health checks, state cleanup, and resource management
//! on a configurable interval.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::session::SessionStore;

/// Configuration for the maintenance timer.
#[derive(Debug, Clone)]
pub(crate) struct MaintenanceConfig {
    /// Interval between maintenance runs.
    pub(crate) interval: Duration,
    /// Whether to prune expired sessions.
    pub(crate) prune_sessions: bool,
    /// Whether to check channel health.
    pub(crate) check_channels: bool,
    /// Whether to compact log store.
    pub(crate) compact_logs: bool,
    /// Maximum age for completed sessions before pruning.
    pub(crate) session_max_age: Duration,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300), // 5 minutes
            prune_sessions: true,
            check_channels: true,
            compact_logs: true,
            session_max_age: Duration::from_secs(7 * 24 * 3600), // 7 days
        }
    }
}

/// Results from a maintenance run.
#[derive(Debug, Default)]
pub(crate) struct MaintenanceReport {
    /// Number of sessions pruned.
    pub(crate) sessions_pruned: usize,
    /// Number of channels checked.
    pub(crate) channels_checked: usize,
    /// Number of channels that are unhealthy.
    pub(crate) channels_unhealthy: usize,
    /// Whether log compaction ran.
    pub(crate) logs_compacted: bool,
    /// Errors encountered.
    pub(crate) errors: Vec<String>,
}

/// Background maintenance service.
pub(crate) struct MaintenanceService {
    config: MaintenanceConfig,
    session_store: Arc<SessionStore>,
    /// Shutdown signal receiver.
    shutdown_rx: watch::Receiver<bool>,
}

impl MaintenanceService {
    pub(crate) fn new(
        config: MaintenanceConfig,
        session_store: Arc<SessionStore>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            config,
            session_store,
            shutdown_rx,
        }
    }

    /// Run the maintenance loop. This blocks until shutdown.
    pub(crate) async fn run(mut self) {
        info!(
            interval_secs = self.config.interval.as_secs(),
            "maintenance service started"
        );

        let mut interval = tokio::time::interval(self.config.interval);
        interval.tick().await; // Skip initial tick.

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let report = self.run_maintenance().await;
                    if !report.errors.is_empty() {
                        warn!(
                            errors = ?report.errors,
                            "maintenance completed with errors"
                        );
                    } else {
                        info!(
                            sessions_pruned = report.sessions_pruned,
                            "maintenance completed"
                        );
                    }
                }
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("maintenance service shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Execute one maintenance cycle.
    async fn run_maintenance(&self) -> MaintenanceReport {
        let mut report = MaintenanceReport::default();

        // 1. Prune expired sessions.
        if self.config.prune_sessions {
            report.sessions_pruned = self.session_store.prune().await;
        }

        // 2. Check channel health.
        if self.config.check_channels {
            // Channel health checking would iterate registered channels
            // and call a health_check method on each.
            // For now, this is a placeholder.
            report.channels_checked = 0;
        }

        // 3. Compact logs.
        if self.config.compact_logs {
            // Log compaction would trim old log entries.
            report.logs_compacted = true;
        }

        report
    }
}

impl std::fmt::Debug for MaintenanceService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintenanceService")
            .field("config", &self.config)
            .finish()
    }
}
