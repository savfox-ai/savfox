use std::collections::VecDeque;
use std::sync::OnceLock;

use serde::Serialize;
use tokio::sync::Mutex;

const MAX_LOGS: usize = 2000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GatewayLogEntry {
    pub(crate) ts_ms: u64,
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
        ts_ms: crate::json_store::now_ms(),
        level: level.to_owned(),
        source: source.to_owned(),
        message: safe_message,
    });
}

pub(crate) async fn list_logs(limit: usize) -> Vec<GatewayLogEntry> {
    let lock = store().lock().await;
    let n = limit.max(1).min(MAX_LOGS);
    let len = lock.len();
    let start = len.saturating_sub(n);
    lock.iter().skip(start).cloned().collect()
}
