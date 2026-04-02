//! Send policy  - controls where and how agent replies are delivered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Delivery mode for replies
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DeliveryMode {
    /// Reply to the same channel/thread the message came from
    #[default]
    SameChannel,
    /// Reply via DM to the sender
    DirectMessage,
    /// Reply to a specific target channel
    TargetChannel { channel: String, target: String },
    /// Broadcast to all connected channels
    Broadcast,
}

/// Streaming behavior for responses
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StreamingMode {
    /// Stream response tokens as they arrive
    Streaming,
    /// Wait for complete response, then send
    #[default]
    Batch,
    /// Stream but edit the same message (for platforms that support it)
    EditInPlace,
}

/// Full send policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPolicy {
    /// How to deliver the reply
    #[serde(default)]
    pub delivery: DeliveryMode,
    /// How to stream the response
    #[serde(default)]
    pub streaming: StreamingMode,
    /// Whether to chunk long messages
    #[serde(default = "default_true")]
    pub auto_chunk: bool,
    /// Whether to include typing indicators
    #[serde(default = "default_true")]
    pub typing_indicators: bool,
    /// Maximum response length (0 = unlimited)
    #[serde(default)]
    pub max_length: usize,
}

fn default_true() -> bool {
    true
}

impl Default for SendPolicy {
    fn default() -> Self {
        Self {
            delivery: DeliveryMode::default(),
            streaming: StreamingMode::default(),
            auto_chunk: true,
            typing_indicators: true,
            max_length: 0,
        }
    }
}

impl SendPolicy {
    /// Create a policy for DM replies
    #[must_use]
    pub fn dm() -> Self {
        Self {
            delivery: DeliveryMode::DirectMessage,
            ..Default::default()
        }
    }

    /// Create a policy for streaming replies
    #[must_use]
    pub fn streaming() -> Self {
        Self {
            streaming: StreamingMode::Streaming,
            ..Default::default()
        }
    }

    /// Create a broadcast policy
    #[must_use]
    pub fn broadcast() -> Self {
        Self {
            delivery: DeliveryMode::Broadcast,
            ..Default::default()
        }
    }
}

/// Threading behavior for outbound sends.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadingPolicy {
    /// Never include thread/reply metadata.
    Off,
    /// Always include thread/reply metadata.
    #[default]
    All,
    /// Include threading metadata only for the first send in a thread.
    First,
}

/// Channel-level send retry and threading settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSendPolicy {
    /// Number of attempts before giving up.
    #[serde(default = "default_retry_count")]
    pub retry_count: usize,
    /// Base backoff in milliseconds (exponential per attempt).
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: u64,
    /// Per-attempt timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Threading behavior.
    #[serde(default)]
    pub threading: ThreadingPolicy,
    /// Whether to auto-chunk long messages for this channel.
    #[serde(default = "default_true")]
    pub auto_chunk: bool,
    /// Optional max chunk size override (0 = platform default).
    #[serde(default)]
    pub chunk_max_chars: usize,
    /// Number of trailing characters repeated at the start of the next chunk.
    #[serde(default)]
    pub chunk_overlap_chars: usize,
}

fn default_retry_count() -> usize {
    3
}

fn default_backoff_ms() -> u64 {
    200
}

fn default_timeout_ms() -> u64 {
    15_000
}

impl Default for ChannelSendPolicy {
    fn default() -> Self {
        Self {
            retry_count: default_retry_count(),
            backoff_ms: default_backoff_ms(),
            timeout_ms: default_timeout_ms(),
            threading: ThreadingPolicy::All,
            auto_chunk: true,
            chunk_max_chars: 0,
            chunk_overlap_chars: 0,
        }
    }
}

/// Runtime send policy config loaded from `{savfox_home}/send-policy.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendPolicyConfig {
    /// Default fallback policy.
    #[serde(default)]
    pub default: ChannelSendPolicy,
    /// Per-channel or per-platform policy overrides.
    /// Keys can be `platform:channel_id`, `platform`, or `*`.
    #[serde(default)]
    pub channels: HashMap<String, ChannelSendPolicy>,
    /// Optional dead-letter directory (relative to `savfox_home` by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_letter_dir: Option<String>,
}

impl SendPolicyConfig {
    #[must_use]
    pub fn resolve(&self, channel: &str) -> ChannelSendPolicy {
        if let Some(exact) = self.channels.get(channel) {
            return exact.clone();
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(platform_override) = self.channels.get(platform)
        {
            return platform_override.clone();
        }
        if let Some(wildcard) = self.channels.get("*") {
            return wildcard.clone();
        }
        self.default.clone()
    }

    #[must_use]
    pub fn dead_letter_path(&self, savfox_home: &Path) -> PathBuf {
        match self.dead_letter_dir.as_deref() {
            Some(path) => {
                let candidate = PathBuf::from(path);
                if candidate.is_absolute() {
                    candidate
                } else {
                    savfox_home.join(candidate)
                }
            }
            None => savfox_home.join("dead_letter"),
        }
    }

    pub async fn load(savfox_home: &Path) -> Self {
        let path = savfox_home.join("send-policy.json");
        crate::json_store::load_json(&path, "send policy")
            .await
            .unwrap_or_default()
    }
}

/// Aggregated send metrics for reliability reporting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMetrics {
    pub attempts: u64,
    pub success: u64,
    pub failed: u64,
    pub total_latency_ms: u64,
}

impl SendMetrics {
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            0.0
        } else {
            self.success as f64 / self.attempts as f64
        }
    }

    #[must_use]
    pub fn average_latency_ms(&self) -> f64 {
        if self.success == 0 {
            0.0
        } else {
            self.total_latency_ms as f64 / self.success as f64
        }
    }
}
