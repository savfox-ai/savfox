use std::collections::VecDeque;
use std::fmt;
use std::sync::OnceLock;

use serde::Serialize;
use tokio::sync::Mutex;
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const MAX_LOGS: usize = 2000;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GatewayLogEntry {
    pub(crate) timestamp: String,
    pub(crate) level: String,
    pub(crate) source: String,
    pub(crate) message: String,
}

fn store() -> &'static Mutex<VecDeque<GatewayLogEntry>> {
    static STORE: OnceLock<Mutex<VecDeque<GatewayLogEntry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LOGS)))
}

pub(crate) async fn append_log(level: &str, source: &str, message: impl Into<String>) {
    let raw_message = message.into();
    let safe_message = crate::redaction::redact_text(&raw_message);

    let mut lock = store().lock().await;
    if lock.len() >= MAX_LOGS {
        lock.pop_front();
    }
    lock.push_back(GatewayLogEntry {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        level: level.to_owned(),
        source: source.to_owned(),
        message: safe_message,
    });
}

/// Synchronous version for use inside tracing layer (non-async context).
pub(crate) fn append_log_sync(level: &str, source: &str, message: String) {
    let safe_message = crate::redaction::redact_text(&message);
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // Use try_lock to avoid blocking; drop the entry if the lock is held.
    if let Ok(mut lock) = store().try_lock() {
        if lock.len() >= MAX_LOGS {
            lock.pop_front();
        }
        lock.push_back(GatewayLogEntry {
            timestamp,
            level: level.to_owned(),
            source: source.to_owned(),
            message: safe_message,
        });
    }
}

pub(crate) async fn list_logs(limit: usize) -> Vec<GatewayLogEntry> {
    let lock = store().lock().await;
    let n = limit.max(1).min(MAX_LOGS);
    let len = lock.len();
    let start = len.saturating_sub(n);
    lock.iter().skip(start).cloned().collect()
}

// ── Tracing capture layer ─────────────────────────────────────────────────

/// A tracing layer that captures events into the in-memory log store
/// so they are visible on the gateway logs page.
pub(crate) struct LogCaptureLayer;

struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }

    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else {
            let extra = self.fields.join(" ");
            if self.message.is_empty() {
                extra
            } else {
                format!("{} {}", self.message, extra)
            }
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}

impl<S: Subscriber> Layer<S> for LogCaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };

        let source = event.metadata().target().to_string();

        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);
        let message = visitor.finish();

        append_log_sync(level, &source, message);
    }
}
