mod client;
mod config;
mod error;
pub(crate) mod names;
pub(crate) mod runtime_metrics;
pub(crate) mod timer;
pub(crate) mod validation;

use std::sync::OnceLock;

pub use crate::metrics::client::MetricsClient;
pub use crate::metrics::config::{MetricsConfig, MetricsExporter};
pub use crate::metrics::error::{MetricsError, Result};

static GLOBAL_METRICS: OnceLock<MetricsClient> = OnceLock::new();

pub(crate) fn install_global(metrics: MetricsClient) {
    let _ = GLOBAL_METRICS.set(metrics);
}

pub fn global() -> Option<MetricsClient> {
    GLOBAL_METRICS.get().cloned()
}
