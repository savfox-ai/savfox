use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::channel::GatewayChannel;
use crate::response_chunker::chunk_message_for_channel;
use crate::send_policy::{SendMetrics, SendPolicyConfig, ThreadingPolicy};

const DEDUPE_TTL_MS: u64 = 10 * 60 * 1000;

fn dedupe_cache() -> &'static Mutex<HashMap<String, u64>> {
    static CACHE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn thread_once_cache() -> &'static Mutex<HashSet<String>> {
    static CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn send_metrics_store() -> &'static Mutex<HashMap<String, SendMetrics>> {
    static STORE: OnceLock<Mutex<HashMap<String, SendMetrics>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) async fn send_metrics_snapshot() -> HashMap<String, SendMetrics> {
    send_metrics_store().lock().await.clone()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChannelHealthMetrics {
    pub last_message_time_ms: Option<u64>,
    pub last_event_time_ms: Option<u64>,
    pub reconnect_attempt_count: u64,
    pub connected_since_ms: Option<u64>,
    pub last_probe_time_ms: Option<u64>,
    pub probe_status: Option<String>,
}

impl ChannelHealthMetrics {
    fn mark_event(&mut self, timestamp_ms: u64) {
        self.last_event_time_ms = Some(timestamp_ms);
    }

    fn mark_send(&mut self, success: bool, timestamp_ms: u64) {
        if success {
            self.last_message_time_ms = Some(timestamp_ms);
            if self.connected_since_ms.is_none() {
                self.connected_since_ms = Some(timestamp_ms);
            }
        } else {
            self.reconnect_attempt_count = self.reconnect_attempt_count.saturating_add(1);
        }
    }

    fn mark_probe(&mut self, status: &str, timestamp_ms: u64) {
        self.last_probe_time_ms = Some(timestamp_ms);
        self.probe_status = Some(status.to_owned());
    }
}

fn channel_health_store() -> &'static Mutex<HashMap<String, ChannelHealthMetrics>> {
    static STORE: OnceLock<Mutex<HashMap<String, ChannelHealthMetrics>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) async fn channel_health_snapshot() -> HashMap<String, ChannelHealthMetrics> {
    channel_health_store().lock().await.clone()
}

pub(crate) async fn record_channel_probe(platform: &str, status: &str) {
    let platform = platform.trim().to_ascii_lowercase();
    if platform.is_empty() {
        return;
    }
    let now = crate::json_store::now_ms();
    let mut lock = channel_health_store().lock().await;
    lock.entry(platform).or_default().mark_probe(status, now);
}

pub(super) async fn record_channel_event(channel: &str) {
    let now = crate::json_store::now_ms();
    let platform = channel.split_once(':').map(|(p, _)| p).unwrap_or(channel);
    let mut lock = channel_health_store().lock().await;
    for key in [platform.to_owned(), "*".to_owned()] {
        lock.entry(key).or_default().mark_event(now);
    }
}

pub(crate) async fn should_drop_duplicate(event_key: Option<String>) -> bool {
    let Some(key) = event_key else {
        return false;
    };
    let now = crate::json_store::now_ms();
    let mut lock = dedupe_cache().lock().await;
    lock.retain(|_, ts| now.saturating_sub(*ts) <= DEDUPE_TTL_MS);
    if lock.contains_key(&key) {
        return true;
    }
    lock.insert(key, now);
    false
}

fn thread_tracking_key(
    channel: &str,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
) -> Option<String> {
    let thread = thread_id
        .and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .or_else(|| {
            reply_target.and_then(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
        })?;
    Some(format!("{channel}::{thread}"))
}

async fn record_send_metrics(channel: &str, success: bool, latency_ms: u64) {
    let mut lock = send_metrics_store().lock().await;
    let platform = channel.split_once(':').map(|(p, _)| p).unwrap_or(channel);
    for key in [platform.to_owned(), "*".to_owned()] {
        let metric = lock.entry(key).or_default();
        metric.attempts = metric.attempts.saturating_add(1);
        if success {
            metric.success = metric.success.saturating_add(1);
            metric.total_latency_ms = metric.total_latency_ms.saturating_add(latency_ms);
        } else {
            metric.failed = metric.failed.saturating_add(1);
        }
    }
    drop(lock);

    let now = crate::json_store::now_ms();
    let mut health = channel_health_store().lock().await;
    for key in [platform.to_owned(), "*".to_owned()] {
        health.entry(key).or_default().mark_send(success, now);
    }
}

async fn write_dead_letter(
    gateway_channel: &GatewayChannel,
    policy_cfg: &SendPolicyConfig,
    channel_id: &str,
    text: &str,
    error: &str,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
    attempts: usize,
) {
    let dir = policy_cfg.dead_letter_path(&gateway_channel.config().savfox_home);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let file_name = format!(
        "{}-{}.json",
        crate::json_store::now_ms(),
        uuid::Uuid::now_v7()
    );
    let payload = json!({
        "timestamp_ms": crate::json_store::now_ms(),
        "channel": channel_id,
        "thread_id": thread_id,
        "reply_target": reply_target,
        "attempts": attempts,
        "error": error,
        "text": text,
    });
    if let Ok(serialized) = serde_json::to_vec_pretty(&payload) {
        let _ = tokio::fs::write(dir.join(file_name), serialized).await;
    }
}

pub(super) async fn send_with_retry(
    gateway_channel: &GatewayChannel,
    channel_id: &str,
    text: &str,
    attempt_override: Option<usize>,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
    saved_channel_config_id: Option<&str>,
) -> anyhow::Result<()> {
    let policy_cfg = SendPolicyConfig::load(&gateway_channel.config().savfox_home).await;
    let policy = policy_cfg.resolve(channel_id);
    let max_attempts = attempt_override.unwrap_or(policy.retry_count).max(1);
    let timeout = Duration::from_millis(policy.timeout_ms.max(1));
    let base_backoff = policy.backoff_ms.max(1);
    let chunk_max_override = (policy.chunk_max_chars > 0).then_some(policy.chunk_max_chars);
    let chunk_texts: Vec<String> = if policy.auto_chunk {
        chunk_message_for_channel(
            text,
            channel_id,
            chunk_max_override,
            policy.chunk_overlap_chars,
        )
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
    } else {
        vec![text.to_owned()]
    };

    let thread_key = thread_tracking_key(channel_id, thread_id, reply_target);
    let include_first_threading = match policy.threading {
        ThreadingPolicy::Off => false,
        ThreadingPolicy::All => true,
        ThreadingPolicy::First => {
            if let Some(key) = thread_key.as_ref() {
                !thread_once_cache().lock().await.contains(key)
            } else {
                false
            }
        }
    };
    for (chunk_index, chunk_text) in chunk_texts.iter().enumerate() {
        let include_threading = match policy.threading {
            ThreadingPolicy::Off => false,
            ThreadingPolicy::All => true,
            ThreadingPolicy::First => include_first_threading && chunk_index == 0,
        };
        let scoped_thread_id = if include_threading { thread_id } else { None };
        let scoped_reply_target = if include_threading {
            reply_target
        } else {
            None
        };

        for attempt in 1..=max_attempts {
            let started = Instant::now();
            let send_result = tokio::time::timeout(
                timeout,
                gateway_channel.send_platform_message_with_context(
                    channel_id,
                    chunk_text,
                    None,
                    None,
                    None,
                    scoped_thread_id,
                    scoped_reply_target,
                    saved_channel_config_id,
                ),
            )
            .await;

            match send_result {
                Ok(Ok(())) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel_id, true, latency_ms).await;
                    if policy.threading == ThreadingPolicy::First
                        && include_threading
                        && let Some(key) = thread_key.as_ref()
                    {
                        thread_once_cache().lock().await.insert(key.clone());
                    }
                    break;
                }
                Ok(Err(err)) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel_id, false, latency_ms).await;
                    if attempt >= max_attempts {
                        let chunk_error = if chunk_texts.len() > 1 {
                            format!(
                                "chunk {}/{} failed: {err}",
                                chunk_index + 1,
                                chunk_texts.len()
                            )
                        } else {
                            err.to_string()
                        };
                        write_dead_letter(
                            gateway_channel,
                            &policy_cfg,
                            channel_id,
                            chunk_text,
                            &chunk_error,
                            scoped_thread_id,
                            scoped_reply_target,
                            attempt,
                        )
                        .await;
                        return Err(err);
                    }
                }
                Err(_) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel_id, false, latency_ms).await;
                    if attempt >= max_attempts {
                        let err = anyhow::anyhow!("send timeout after {}ms", policy.timeout_ms);
                        let chunk_error = if chunk_texts.len() > 1 {
                            format!(
                                "chunk {}/{} timeout: {err}",
                                chunk_index + 1,
                                chunk_texts.len()
                            )
                        } else {
                            err.to_string()
                        };
                        write_dead_letter(
                            gateway_channel,
                            &policy_cfg,
                            channel_id,
                            chunk_text,
                            &chunk_error,
                            scoped_thread_id,
                            scoped_reply_target,
                            attempt,
                        )
                        .await;
                        return Err(err);
                    }
                }
            }

            let backoff_ms =
                base_backoff.saturating_mul(1u64 << (attempt.saturating_sub(1) as u32));
            tokio::time::sleep(Duration::from_millis(backoff_ms.min(5_000))).await;
        }
    }

    Ok(())
}
