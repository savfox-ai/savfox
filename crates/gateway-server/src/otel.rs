//! OpenTelemetry diagnostics for gateway observability.
//!
//! When enabled, exports traces and metrics to an OTLP endpoint.
//! Configuration via environment variables:
//! - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (e.g., `http://localhost:4317`)
//! - `OTEL_SERVICE_NAME`: Service name (default: `"savfox-gateway"`)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn default_endpoint() -> String {
    "http://localhost:4317".to_owned()
}

fn default_service_name() -> String {
    "savfox-gateway".to_owned()
}

fn default_sample_rate() -> f64 {
    1.0
}

fn default_export_interval() -> u64 {
    60
}

/// OpenTelemetry export configuration.
///
/// Typically embedded inside the gateway config file under `[otel]`.
/// All fields have sensible defaults so the section can be omitted entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OtelConfig {
    /// Enable OpenTelemetry export.
    #[serde(default)]
    pub enabled: bool,

    /// OTLP endpoint URL.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Service name for traces/metrics.
    #[serde(default = "default_service_name")]
    pub service_name: String,

    /// Sample rate (0.0 to 1.0).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,

    /// Export interval in seconds.
    #[serde(default = "default_export_interval")]
    pub export_interval_secs: u64,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_endpoint(),
            service_name: default_service_name(),
            sample_rate: default_sample_rate(),
            export_interval_secs: default_export_interval(),
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Gateway metrics collected for observability.
///
/// All counters use relaxed atomic ordering since perfect accuracy is not
/// required  - these are best-effort telemetry signals.
#[derive(Debug)]
pub(crate) struct GatewayMetrics {
    /// Total HTTP/WS requests received.
    pub requests_total: AtomicU64,
    /// Requests that resulted in an error response.
    pub requests_failed: AtomicU64,
    /// Currently active WebSocket connections.
    pub ws_connections_active: AtomicU64,
    /// Total WebSocket connections opened (lifetime counter).
    pub ws_connections_total: AtomicU64,
    /// Total agent invocations started.
    pub agent_invocations_total: AtomicU64,
    /// Agent invocations that ended in failure.
    pub agent_invocations_failed: AtomicU64,
    /// Approximate LLM tokens consumed across all agent runs.
    pub agent_tokens_consumed: AtomicU64,
    /// Messages received from chat-platform bridges.
    pub bridge_messages_in: AtomicU64,
    /// Messages sent out to chat-platform bridges.
    pub bridge_messages_out: AtomicU64,
    /// Number of tracked sessions.
    pub session_count: AtomicU64,
}

impl GatewayMetrics {
    /// Create a fresh metrics instance with all counters at zero.
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_failed: AtomicU64::new(0),
            ws_connections_active: AtomicU64::new(0),
            ws_connections_total: AtomicU64::new(0),
            agent_invocations_total: AtomicU64::new(0),
            agent_invocations_failed: AtomicU64::new(0),
            agent_tokens_consumed: AtomicU64::new(0),
            bridge_messages_in: AtomicU64::new(0),
            bridge_messages_out: AtomicU64::new(0),
            session_count: AtomicU64::new(0),
        }
    }

    // -- Increment helpers --------------------------------------------------

    /// Record an incoming request.
    pub fn inc_requests(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a failed request.
    pub fn inc_requests_failed(&self) {
        self.requests_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a new WebSocket connection.
    pub fn inc_ws_connected(&self) {
        self.ws_connections_total.fetch_add(1, Ordering::Relaxed);
        self.ws_connections_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a WebSocket disconnection.
    pub fn dec_ws_connected(&self) {
        // Saturating subtract via fetch_sub  - underflow is fine for relaxed counters
        // but we guard against it for correctness of the gauge.
        let prev = self.ws_connections_active.load(Ordering::Relaxed);
        if prev > 0 {
            self.ws_connections_active.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Record an agent invocation.
    pub fn inc_agent_invocations(&self) {
        self.agent_invocations_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a failed agent invocation.
    pub fn inc_agent_invocations_failed(&self) {
        self.agent_invocations_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record consumed LLM tokens.
    pub fn add_agent_tokens(&self, tokens: u64) {
        self.agent_tokens_consumed
            .fetch_add(tokens, Ordering::Relaxed);
    }

    /// Record an inbound bridge message.
    pub fn inc_bridge_messages_in(&self) {
        self.bridge_messages_in.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an outbound bridge message.
    pub fn inc_bridge_messages_out(&self) {
        self.bridge_messages_out.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the current session count gauge.
    pub fn set_session_count(&self, count: u64) {
        self.session_count.store(count, Ordering::Relaxed);
    }

    // -- Snapshot / export --------------------------------------------------

    /// Export current metrics as a JSON value (for the `/api/metrics` endpoint).
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "requests_total": self.requests_total.load(Ordering::Relaxed),
            "requests_failed": self.requests_failed.load(Ordering::Relaxed),
            "ws_connections_active": self.ws_connections_active.load(Ordering::Relaxed),
            "ws_connections_total": self.ws_connections_total.load(Ordering::Relaxed),
            "agent_invocations_total": self.agent_invocations_total.load(Ordering::Relaxed),
            "agent_invocations_failed": self.agent_invocations_failed.load(Ordering::Relaxed),
            "agent_tokens_consumed": self.agent_tokens_consumed.load(Ordering::Relaxed),
            "bridge_messages_in": self.bridge_messages_in.load(Ordering::Relaxed),
            "bridge_messages_out": self.bridge_messages_out.load(Ordering::Relaxed),
            "session_count": self.session_count.load(Ordering::Relaxed),
        })
    }
}

// ---------------------------------------------------------------------------
// Salvo handler
// ---------------------------------------------------------------------------

/// `GET /api/metrics`  - Return current gateway metrics as JSON.
#[handler]
pub(crate) async fn metrics_handler(depot: &mut Depot, res: &mut Response) {
    let metrics = match depot.obtain::<Arc<GatewayMetrics>>() {
        Ok(m) => m.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": "metrics not available"}).to_string(),
            ));
            return;
        }
    };

    res.render(Text::Json(metrics.to_json().to_string()));
}

// ---------------------------------------------------------------------------
// Tracing layer (stub)
// ---------------------------------------------------------------------------

/// Initialize OpenTelemetry tracing layer.
///
/// This is a stub that logs the configuration. Actual OTLP export requires
/// adding the `opentelemetry-otlp` crate as a dependency and wiring up
/// a `tracing_opentelemetry::OpenTelemetryLayer`.
pub(crate) fn init_otel_layer(config: &OtelConfig) -> anyhow::Result<()> {
    if !config.enabled {
        info!("OpenTelemetry export disabled");
        return Ok(());
    }

    info!(
        endpoint = %config.endpoint,
        service = %config.service_name,
        sample_rate = config.sample_rate,
        export_interval_secs = config.export_interval_secs,
        "OpenTelemetry: configured (stub  - add opentelemetry-otlp for full export)"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_start_at_zero() {
        let m = GatewayMetrics::new();
        assert_eq!(m.requests_total.load(Ordering::Relaxed), 0);
        assert_eq!(m.ws_connections_active.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn inc_dec_ws_connections() {
        let m = GatewayMetrics::new();
        m.inc_ws_connected();
        m.inc_ws_connected();
        assert_eq!(m.ws_connections_active.load(Ordering::Relaxed), 2);
        assert_eq!(m.ws_connections_total.load(Ordering::Relaxed), 2);

        m.dec_ws_connected();
        assert_eq!(m.ws_connections_active.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dec_ws_does_not_underflow() {
        let m = GatewayMetrics::new();
        m.dec_ws_connected(); // should not panic or wrap
        assert_eq!(m.ws_connections_active.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn to_json_snapshot() {
        let m = GatewayMetrics::new();
        m.inc_requests();
        m.inc_requests();
        m.inc_requests_failed();
        m.add_agent_tokens(1500);

        let j = m.to_json();
        assert_eq!(j["requests_total"], 2);
        assert_eq!(j["requests_failed"], 1);
        assert_eq!(j["agent_tokens_consumed"], 1500);
    }

    #[test]
    fn default_config_values() {
        let config = OtelConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.endpoint, "http://localhost:4317");
        assert_eq!(config.service_name, "savfox-gateway");
        assert!((config.sample_rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.export_interval_secs, 60);
    }

    #[test]
    fn init_otel_disabled_succeeds() {
        let config = OtelConfig::default();
        assert!(init_otel_layer(&config).is_ok());
    }
}
