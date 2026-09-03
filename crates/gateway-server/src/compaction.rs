//! Context window compaction for gateway sessions.
//!
//! Manages context window size by summarising older messages when the token
//! count approaches a configurable threshold. Pinned messages and (optionally)
//! tool results are preserved verbatim; everything else is condensed into a
//! compact summary block.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::info;

// ---- Configuration --------------------------------------------------------

/// How the compaction service decides when to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CompactionMode {
    /// Compact automatically when the threshold is reached.
    #[default]
    Auto,
    /// Only compact when explicitly requested via RPC.
    Manual,
    /// Never compact (context grows unbounded).
    Disabled,
}

/// Knobs that control *how* and *when* compaction happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Compaction trigger mode.
    #[serde(default)]
    pub mode: CompactionMode,

    /// Percentage of `max_tokens` at which compaction triggers (0-100).
    #[serde(default = "default_threshold_percent")]
    pub threshold_percent: u8,

    /// Keep messages whose `role` is `"tool"` (or that carry `tool_call_id`).
    #[serde(default = "default_true")]
    pub preserve_tool_results: bool,

    /// Keep messages that have `"pinned": true` in their metadata.
    #[serde(default = "default_true")]
    pub preserve_pinned: bool,

    /// Maximum number of tokens the generated summary may use.
    #[serde(default = "default_summary_max_tokens")]
    pub summary_max_tokens: u32,

    // ── Memory flush (#47) ──────────────────────────────────────────
    /// When true, flush a summary to session-layer memory on compaction.
    #[serde(default)]
    pub memory_flush_enabled: bool,

    /// Token threshold at which memory flush begins (soft threshold).
    #[serde(default = "default_memory_flush_threshold")]
    pub memory_flush_soft_threshold_tokens: u64,

    /// Custom prompt for generating the memory flush summary.
    #[serde(default)]
    pub memory_flush_prompt: Option<String>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            mode: CompactionMode::default(),
            threshold_percent: default_threshold_percent(),
            preserve_tool_results: true,
            preserve_pinned: true,
            summary_max_tokens: default_summary_max_tokens(),
            memory_flush_enabled: false,
            memory_flush_soft_threshold_tokens: default_memory_flush_threshold(),
            memory_flush_prompt: None,
        }
    }
}

fn default_threshold_percent() -> u8 {
    80
}

fn default_summary_max_tokens() -> u32 {
    2000
}

fn default_true() -> bool {
    true
}

fn default_memory_flush_threshold() -> u64 {
    50_000
}

// ---- Result ---------------------------------------------------------------

/// Outcome of a single compaction run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionResult {
    /// A prose summary of the messages that were removed.
    pub summary: String,
    /// Messages that survived compaction (pinned + tool + recent + summary).
    pub kept_messages: Vec<Value>,
    /// How many messages were removed and folded into the summary.
    pub removed_count: u32,
    /// Estimated token count *before* compaction.
    pub pre_tokens: u64,
    /// Estimated token count *after* compaction.
    pub post_tokens: u64,
}

/// Compaction is fail-closed because replacing history with a lossy local
/// truncation can permanently discard constraints, decisions and tool data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompactionError {
    #[error("a semantic summary is required before removing {removed_count} message(s)")]
    SemanticSummaryRequired { removed_count: u32 },
    #[error("the semantic summary must not be empty")]
    EmptySummary,
    #[error("the semantic summary exceeds the configured {max_tokens}-token budget")]
    SummaryExceedsBudget { max_tokens: u32 },
}

// ---- Service --------------------------------------------------------------

/// Stateless compaction service.
///
/// Holds a [`CompactionConfig`] and exposes helpers that decide *whether* to
/// compact and *how* to compact a message list.
#[derive(Debug, Clone)]
pub struct CompactionService {
    config: CompactionConfig,
}

impl CompactionService {
    /// Create a new service with the given configuration.
    #[must_use]
    pub fn new(config: CompactionConfig) -> Self {
        Self { config }
    }

    /// Return a reference to the active configuration.
    #[must_use]
    pub fn config(&self) -> &CompactionConfig {
        &self.config
    }

    /// Quick check: should we compact right now?
    ///
    /// Returns `true` when the current token usage exceeds the configured
    /// percentage of the context window and the mode is not `Disabled`.
    #[must_use]
    pub fn should_compact(&self, session_tokens: u64, max_tokens: u64) -> bool {
        if self.config.mode == CompactionMode::Disabled {
            return false;
        }
        if max_tokens == 0 {
            return false;
        }
        let threshold = max_tokens * u64::from(self.config.threshold_percent) / 100;
        session_tokens >= threshold
    }

    /// Check whether compaction can proceed without removing history.
    ///
    /// The algorithm:
    /// 1. Separate **pinned** messages (kept unconditionally).
    /// 2. Separate **tool-result** messages when `preserve_tool_results` is set.
    /// 3. Keep the most recent `tail_count` messages untouched.
    /// 4. If any other messages would be removed, fail until a semantic summary is supplied through
    ///    [`Self::compact_with_summary`].
    ///
    /// `tail_count` controls how many recent messages are preserved verbatim
    /// (in addition to pinned and tool messages). A value of 0 means only
    /// pinned / tool messages survive.
    pub fn compact(
        &self,
        session_id: &str,
        messages: &[Value],
        tail_count: usize,
    ) -> Result<CompactionResult, CompactionError> {
        self.compact_inner(session_id, messages, tail_count, None)
    }

    /// Build a compaction result using a semantic summary produced by an
    /// external model or structured extractor.
    ///
    /// The summary is validated before any compacted result is returned. An
    /// empty or over-budget summary fails closed, leaving the caller's original
    /// message slice untouched.
    pub fn compact_with_summary(
        &self,
        session_id: &str,
        messages: &[Value],
        tail_count: usize,
        semantic_summary: &str,
    ) -> Result<CompactionResult, CompactionError> {
        self.compact_inner(session_id, messages, tail_count, Some(semantic_summary))
    }

    fn compact_inner(
        &self,
        session_id: &str,
        messages: &[Value],
        tail_count: usize,
        semantic_summary: Option<&str>,
    ) -> Result<CompactionResult, CompactionError> {
        let pre_tokens = estimate_tokens(messages);

        if messages.is_empty() {
            return Ok(CompactionResult {
                summary: String::new(),
                kept_messages: Vec::new(),
                removed_count: 0,
                pre_tokens,
                post_tokens: 0,
            });
        }

        // --- partition messages ---

        // We walk from oldest to newest. The last `tail_count` messages are
        // always kept; of the remainder, pinned and (optionally) tool messages
        // are preserved and everything else is "old" and will be summarised.

        let total = messages.len();
        let split_point = total.saturating_sub(tail_count);
        let (candidates, recent) = messages.split_at(split_point);

        let mut pinned: Vec<Value> = Vec::new();
        let mut tool_results: Vec<Value> = Vec::new();
        let mut to_summarise: Vec<&Value> = Vec::new();

        for msg in candidates {
            if self.config.preserve_pinned && is_pinned(msg) {
                pinned.push(msg.clone());
            } else if self.config.preserve_tool_results && is_tool_result(msg) {
                tool_results.push(msg.clone());
            } else {
                to_summarise.push(msg);
            }
        }

        let removed_count = to_summarise.len() as u32;

        // --- build summary ---

        let summary = if to_summarise.is_empty() {
            String::new()
        } else {
            let summary = semantic_summary
                .map(str::trim)
                .ok_or(CompactionError::SemanticSummaryRequired { removed_count })?;
            if summary.is_empty() {
                return Err(CompactionError::EmptySummary);
            }
            if estimate_text_tokens(summary) > u64::from(self.config.summary_max_tokens) {
                return Err(CompactionError::SummaryExceedsBudget {
                    max_tokens: self.config.summary_max_tokens,
                });
            }
            summary.to_owned()
        };

        // --- assemble kept messages ---

        let mut kept: Vec<Value> =
            Vec::with_capacity(1 + pinned.len() + tool_results.len() + recent.len());

        // Insert the summary as a synthetic system message at the front.
        if !summary.is_empty() {
            kept.push(serde_json::json!({
                "role": "system",
                "content": summary,
                "metadata": { "compaction_summary": true },
            }));
        }

        kept.extend(pinned);
        kept.extend(tool_results);
        kept.extend_from_slice(recent);

        let post_tokens = estimate_tokens(&kept);

        info!(
            session_id,
            removed_count, pre_tokens, post_tokens, "session compacted"
        );

        Ok(CompactionResult {
            summary,
            kept_messages: kept,
            removed_count,
            pre_tokens,
            post_tokens,
        })
    }

    /// Generate a memory flush entry from a compaction summary.
    ///
    /// When `memory_flush_enabled` is true, this produces a JSON value
    /// suitable for storing as a session-layer memory entry.
    #[must_use]
    pub fn generate_memory_flush(
        &self,
        session_id: &str,
        result: &CompactionResult,
    ) -> Option<Value> {
        if !self.config.memory_flush_enabled || result.summary.is_empty() {
            return None;
        }

        let prompt = self
            .config
            .memory_flush_prompt
            .as_deref()
            .unwrap_or("Compaction summary for session context preservation.");

        Some(serde_json::json!({
            "layer": "session",
            "title": format!("Compaction summary  - {session_id}"),
            "content": result.summary,
            "tags": ["compaction", "auto-flush"],
            "metadata": {
                "source": "compaction_flush",
                "session_id": session_id,
                "removed_count": result.removed_count,
                "pre_tokens": result.pre_tokens,
                "post_tokens": result.post_tokens,
                "prompt": prompt,
            },
        }))
    }
}

// ---- Token estimation -----------------------------------------------------

/// Rough token estimate: ~4 characters per token.
///
/// This intentionally over-counts slightly so we trigger compaction a little
/// early rather than a little late.
#[must_use]
pub fn estimate_tokens(messages: &[Value]) -> u64 {
    let total_chars: u64 = messages
        .iter()
        .map(|m| {
            // Prefer the `content` field; fall back to the full JSON repr.
            match m.get("content").and_then(Value::as_str) {
                Some(text) => text.len() as u64,
                None => {
                    // serde_json::to_string is infallible for Value
                    m.to_string().len() as u64
                }
            }
        })
        .sum();

    total_chars / 4
}

fn estimate_text_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

// ---- Helpers (private) ----------------------------------------------------

/// Check whether a message is pinned.
fn is_pinned(msg: &Value) -> bool {
    msg.get("metadata")
        .and_then(|m| m.get("pinned"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || msg.get("pinned").and_then(Value::as_bool).unwrap_or(false)
}

/// Check whether a message is a tool result.
fn is_tool_result(msg: &Value) -> bool {
    // OpenAI style: role == "tool"
    if msg.get("role").and_then(Value::as_str) == Some("tool") {
        return true;
    }
    // Or has a `tool_call_id` field.
    if msg.get("tool_call_id").is_some() {
        return true;
    }
    false
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_messages() -> Vec<Value> {
        vec![
            json!({ "role": "system", "content": "You are a helpful assistant." }),
            json!({ "role": "user", "content": "Hello, tell me about Rust." }),
            json!({ "role": "assistant", "content": "Rust is a systems programming language focused on safety and performance." }),
            json!({ "role": "user", "content": "What about async?" }),
            json!({ "role": "assistant", "content": "Async in Rust uses futures and the tokio runtime for concurrency." }),
            json!({ "role": "user", "content": "Thanks!" }),
        ]
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = sample_messages();
        let tokens = estimate_tokens(&msgs);
        // Sanity: total chars / 4, should be > 0.
        assert!(tokens > 0, "token estimate should be positive");
    }

    #[test]
    fn should_compact_respects_threshold() {
        let svc = CompactionService::new(CompactionConfig {
            threshold_percent: 80,
            ..Default::default()
        });

        // Below threshold.
        assert!(!svc.should_compact(700, 1000));
        // At threshold.
        assert!(svc.should_compact(800, 1000));
        // Above threshold.
        assert!(svc.should_compact(900, 1000));
    }

    #[test]
    fn should_compact_disabled_mode() {
        let svc = CompactionService::new(CompactionConfig {
            mode: CompactionMode::Disabled,
            threshold_percent: 50,
            ..Default::default()
        });
        assert!(!svc.should_compact(900, 1000));
    }

    #[test]
    fn should_compact_zero_max() {
        let svc = CompactionService::new(CompactionConfig::default());
        assert!(!svc.should_compact(100, 0));
    }

    #[test]
    fn compact_empty_messages() {
        let svc = CompactionService::new(CompactionConfig::default());
        let result = svc.compact("test:session", &[], 2).unwrap();
        assert_eq!(result.removed_count, 0);
        assert!(result.kept_messages.is_empty());
        assert!(result.summary.is_empty());
    }

    #[test]
    fn compact_requires_semantic_summary_without_mutating_input() {
        let svc = CompactionService::new(CompactionConfig::default());
        let msgs = sample_messages();
        let original = msgs.clone();
        let error = svc.compact("test:session", &msgs, 2).unwrap_err();

        assert_eq!(
            error,
            CompactionError::SemanticSummaryRequired { removed_count: 4 }
        );
        assert_eq!(msgs, original);
    }

    #[test]
    fn compact_preserves_recent_tail_with_semantic_summary() {
        let svc = CompactionService::new(CompactionConfig::default());
        let msgs = sample_messages();
        let result = svc
            .compact_with_summary(
                "test:session",
                &msgs,
                2,
                "The user asked about Rust and async execution.",
            )
            .unwrap();

        // The last 2 messages should survive.
        assert!(result.kept_messages.len() >= 2);
        // The removed count should be 4 (6 total - 2 recent).
        assert_eq!(result.removed_count, 4);
        assert_eq!(
            result.summary,
            "The user asked about Rust and async execution."
        );
    }

    #[test]
    fn compact_preserves_pinned() {
        let svc = CompactionService::new(CompactionConfig {
            preserve_pinned: true,
            ..Default::default()
        });
        let msgs = vec![
            json!({ "role": "system", "content": "pinned msg", "metadata": { "pinned": true } }),
            json!({ "role": "user", "content": "first user msg" }),
            json!({ "role": "assistant", "content": "first assistant response" }),
            json!({ "role": "user", "content": "latest" }),
        ];

        let result = svc
            .compact_with_summary(
                "test:pinned",
                &msgs,
                1,
                "Two middle messages were discussed.",
            )
            .unwrap();

        // Pinned message + summary + recent tail = at least 3.
        // Only the 2 middle messages (user + assistant) are candidates for
        // summarisation; 1 is pinned and kept, so removed_count == 2.
        assert_eq!(result.removed_count, 2);

        // The pinned message must be in kept_messages.
        let has_pinned = result.kept_messages.iter().any(|m| {
            m.get("metadata")
                .and_then(|md| md.get("pinned"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        assert!(has_pinned, "pinned message should survive compaction");
    }

    #[test]
    fn compact_preserves_tool_results() {
        let svc = CompactionService::new(CompactionConfig {
            preserve_tool_results: true,
            ..Default::default()
        });
        let msgs = vec![
            json!({ "role": "user", "content": "run the tool" }),
            json!({ "role": "tool", "content": "tool output", "tool_call_id": "tc_1" }),
            json!({ "role": "assistant", "content": "here is the result" }),
            json!({ "role": "user", "content": "thanks" }),
        ];

        let result = svc
            .compact_with_summary("test:tool", &msgs, 1, "The user requested a tool run.")
            .unwrap();

        let has_tool = result
            .kept_messages
            .iter()
            .any(|m| m.get("role").and_then(Value::as_str) == Some("tool"));
        assert!(has_tool, "tool result should survive compaction");
    }

    #[test]
    fn compact_rejects_empty_or_over_budget_summary() {
        let svc = CompactionService::new(CompactionConfig::default());
        let msgs = sample_messages();
        assert_eq!(
            svc.compact_with_summary("test:empty", &msgs, 1, "   ")
                .unwrap_err(),
            CompactionError::EmptySummary
        );

        let tiny = CompactionService::new(CompactionConfig {
            summary_max_tokens: 1,
            ..Default::default()
        });
        assert_eq!(
            tiny.compact_with_summary("test:large", &msgs, 1, "This summary is too large")
                .unwrap_err(),
            CompactionError::SummaryExceedsBudget { max_tokens: 1 }
        );
    }

    #[test]
    fn compact_reports_token_counts() {
        let svc = CompactionService::new(CompactionConfig::default());
        let msgs = sample_messages();
        let result = svc
            .compact_with_summary("test:tokens", &msgs, 1, "Rust and async were discussed.")
            .unwrap();
        assert!(result.pre_tokens > 0);
        assert!(result.post_tokens > 0);
        assert!(result.removed_count > 0);
        assert!(!result.summary.is_empty());
    }

    #[test]
    fn memory_flush_disabled_returns_none() {
        let svc = CompactionService::new(CompactionConfig {
            memory_flush_enabled: false,
            ..Default::default()
        });
        let msgs = sample_messages();
        let compacted = svc
            .compact_with_summary("test:flush-off", &msgs, 1, "Rust was discussed.")
            .unwrap();
        assert!(
            svc.generate_memory_flush("test:flush-off", &compacted)
                .is_none()
        );
    }

    #[test]
    fn memory_flush_contains_session_layer_and_metrics() {
        let svc = CompactionService::new(CompactionConfig {
            memory_flush_enabled: true,
            memory_flush_prompt: Some("Keep short summary".to_owned()),
            ..Default::default()
        });
        let msgs = sample_messages();
        let compacted = svc
            .compact_with_summary(
                "0194f7b3-1d7b-7c40-ae3d-95b6ef93e170",
                &msgs,
                1,
                "Rust and async were discussed.",
            )
            .unwrap();
        let flush = svc
            .generate_memory_flush("0194f7b3-1d7b-7c40-ae3d-95b6ef93e170", &compacted)
            .expect("memory flush should be generated");

        assert_eq!(flush["layer"].as_str(), Some("session"));
        assert_eq!(
            flush["metadata"]["source"].as_str(),
            Some("compaction_flush")
        );
        assert_eq!(
            flush["metadata"]["session_id"].as_str(),
            Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e170")
        );
        assert_eq!(
            flush["metadata"]["prompt"].as_str(),
            Some("Keep short summary")
        );
    }
}
