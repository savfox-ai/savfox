//! Session-summary wire types and the helpers the frontend uses to derive
//! a human-readable label from a chat session's first message / topic /
//! sender.
//!
//! The helpers ([`is_internal_session_message`], [`normalize_session_label`],
//! [`derive_session_label`], [`short_id`]) are also used inside
//! gateway-server when generating session previews; keeping them here means
//! the frontend renders identical labels without re-implementing the rules.

use serde::Deserialize;

/// Maximum visible characters in a derived session label before
/// truncation. Picked to fit common chat-list row widths (~3 inches at
/// the default font size) without ellipsis.
pub const SESSION_LABEL_MAX_CHARS: usize = 48;
const USER_INSTRUCTIONS_PREFIX: &str = "# agents.md instructions for ";
const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
const TURN_ABORTED_OPEN_TAG: &str = "<turn_aborted>";
const SKILL_INSTRUCTIONS_PREFIX: &str = "<skill";

/// Returns `true` for session messages that the runtime injects on its
/// own behalf (system instructions, environment context, abort markers,
/// skill metadata) and that should never appear as a session label or
/// in the chat history surface. The check is prefix-based so ordering of
/// the discriminator at the very start of the message matters.
#[must_use]
pub fn is_internal_session_message(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    let lowered = trimmed.to_ascii_lowercase();
    lowered.starts_with(USER_INSTRUCTIONS_PREFIX)
        || lowered.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG)
        || lowered.starts_with(TURN_ABORTED_OPEN_TAG)
        || lowered.starts_with(SKILL_INSTRUCTIONS_PREFIX)
}

/// Compact whitespace, drop internal-marker messages, and clamp to
/// [`SESSION_LABEL_MAX_CHARS`].
///
/// Returns `None` for empty input or anything classified as internal by
/// [`is_internal_session_message`]. Otherwise returns a single-line
/// label suitable for display in a session list row, with a `…`-style
/// ellipsis appended only when the input was longer than the cap.
#[must_use]
pub fn normalize_session_label(raw: &str) -> Option<String> {
    if is_internal_session_message(raw) {
        return None;
    }

    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().count() <= SESSION_LABEL_MAX_CHARS {
        return Some(compact.to_owned());
    }
    let truncated: String = compact.chars().take(SESSION_LABEL_MAX_CHARS).collect();
    Some(format!("{truncated}..."))
}

/// Pick the best label candidate for a session in priority order:
/// 1. the first user message (filtered through [`normalize_session_label`]),
/// 2. the configured topic,
/// 3. the channel-supplied name.
///
/// Returns `None` if all three candidates resolve to internal messages /
/// empty strings.
pub fn derive_session_label(
    first_message: Option<&str>,
    topic: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    first_message
        .and_then(normalize_session_label)
        .or_else(|| topic.and_then(normalize_session_label))
        .or_else(|| name.and_then(normalize_session_label))
}

/// Truncate an opaque identifier (e.g. UUIDv7) to its first 16 characters
/// for display, appending `...` when truncated. Useful when no label is
/// available and we need to fall back to "show something".
#[must_use]
pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 16 {
        id.to_owned()
    } else {
        let truncated: String = id.chars().take(16).collect();
        format!("{truncated}...")
    }
}

/// Sender information for a session — populated by the channel adapter
/// from the platform's user identity (Slack user id, Discord author,
/// Matrix MXID, etc.) when available.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSender {
    pub user_id: Option<String>,
    pub name: Option<String>,
}

/// One row in the session list. The wire format carries both legacy and
/// current field names for the same datum (`session_id` / `id`,
/// `message_count` / `messages`); helper methods on this struct collapse
/// those down to a single display value.
#[derive(Clone, Debug, Deserialize)]
pub struct SessionEntry {
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub scope: Option<String>,
    pub label: Option<String>,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub sender: Option<SessionSender>,
    /// Most recent activity timestamp (RFC3339 string).
    pub last_activity: Option<String>,
    pub message_count: Option<u32>,
    /// Legacy alias for `message_count`. Either may be present.
    pub messages: Option<u32>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub thread_id: Option<String>,
    /// `"manual" | "mention" | "dm"` etc. — channel-specific.
    pub group_activation: Option<String>,
}

impl SessionEntry {
    #[must_use]
    pub fn display_id(&self) -> String {
        self.session_id
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("-")
            .to_owned()
    }

    #[must_use]
    pub fn display_count(&self) -> u32 {
        self.message_count.or(self.messages).unwrap_or(0)
    }

    pub fn display_label(&self) -> String {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| self.title.clone())
            .or_else(|| self.subject.clone())
            .or_else(|| self.sender.as_ref().and_then(|s| s.name.clone()))
            .unwrap_or_else(|| short_id(&self.display_id()))
    }
}

#[derive(Debug, Deserialize)]
pub struct SessionsResponse {
    pub entries: Vec<SessionEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionAmbientMessage {
    pub timestamp_ms: u64,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub sender_kind: String,
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionAmbientResponse {
    pub session_id: String,
    pub messages: Vec<SessionAmbientMessage>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionIdleReplyPending {
    pub session_id: String,
    pub agent_id: String,
    pub outbound_channel: String,
    pub delay_secs: u64,
    pub scheduled_at_ms: u64,
    pub deadline_at_ms: u64,
    pub message_preview: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionIdleReplyResponse {
    pub session_id: String,
    pub generation: u64,
    pub pending: Option<SessionIdleReplyPending>,
    pub recent_sent_count: usize,
    pub recent_sent_at_ms: Vec<u64>,
    pub last_suppressed_at_ms: Option<u64>,
    pub suppressed_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionDetail {
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub label: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub channel: Option<String>,
    pub scope: Option<String>,
    pub last_activity: Option<String>,
    pub message_count: Option<u32>,
    pub messages: Option<u32>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub thinking_level: Option<String>,
    pub verbose_level: Option<String>,
    pub reasoning_level: Option<String>,
}

impl SessionDetail {
    #[must_use]
    pub fn display_id(&self) -> String {
        self.session_id
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("-")
            .to_owned()
    }

    #[must_use]
    pub fn display_count(&self) -> u32 {
        self.message_count.or(self.messages).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        derive_session_label, is_internal_session_message, normalize_session_label, short_id,
    };

    #[test]
    fn normalize_session_label_filters_internal_prefix_messages() {
        assert!(normalize_session_label("# AGENTS.md instructions for /tmp").is_none());
        assert!(
            normalize_session_label("<environment_context>\nctx\n</environment_context>").is_none()
        );
    }

    #[test]
    fn derive_session_label_ignores_internal_message_and_falls_back() {
        let label = derive_session_label(
            Some("# AGENTS.md instructions for /tmp"),
            Some("Topic Label"),
            None,
        );
        assert_eq!(label.as_deref(), Some("Topic Label"));
    }

    #[test]
    fn internal_session_message_detection_is_case_insensitive() {
        assert!(is_internal_session_message(
            "<EnViRoNmEnT_CoNtExT>ctx</EnViRoNmEnT_CoNtExT>"
        ));
        assert!(is_internal_session_message(
            "# agents.md instructions for /repo"
        ));
        assert!(!is_internal_session_message("Regular user message"));
        assert_eq!(short_id("1234567890abcdefxyz"), "1234567890abcdef...");
    }

    // ── T4 follow-up: helper coverage ─────────────────────────────────

    #[test]
    fn normalize_collapses_whitespace_runs() {
        let label = normalize_session_label("  hello\t \n   world  ").unwrap();
        assert_eq!(label, "hello world");
    }

    #[test]
    fn normalize_clamps_at_max_chars_with_ellipsis() {
        use super::SESSION_LABEL_MAX_CHARS;
        let s = "a".repeat(SESSION_LABEL_MAX_CHARS + 5);
        let label = normalize_session_label(&s).unwrap();
        // Truncated body + literal "..." appended.
        assert_eq!(label, format!("{}...", "a".repeat(SESSION_LABEL_MAX_CHARS)));
    }

    #[test]
    fn normalize_keeps_label_at_exactly_max_chars_unchanged() {
        use super::SESSION_LABEL_MAX_CHARS;
        let s = "a".repeat(SESSION_LABEL_MAX_CHARS);
        assert_eq!(normalize_session_label(&s).unwrap(), s);
    }

    #[test]
    fn normalize_returns_none_for_whitespace_only_input() {
        assert!(normalize_session_label("   \n\t  ").is_none());
        assert!(normalize_session_label("").is_none());
    }

    #[test]
    fn derive_label_falls_through_to_name_when_message_and_topic_internal() {
        let label = derive_session_label(
            Some("# AGENTS.md instructions for /tmp"),
            Some("<environment_context>x</environment_context>"),
            Some("Sender Name"),
        );
        assert_eq!(label.as_deref(), Some("Sender Name"));
    }

    #[test]
    fn derive_label_returns_none_when_all_candidates_filter_out() {
        assert!(derive_session_label(None, None, None).is_none());
        assert!(
            derive_session_label(Some("<turn_aborted>x</turn_aborted>"), Some(""), None).is_none()
        );
    }

    #[test]
    fn short_id_handles_unicode_correctly() {
        // 16-char limit is by *char count* (not bytes) — confirm by
        // feeding multi-byte chars.
        let mb = "🦀".repeat(20); // 20 chars, 80 bytes
        let result = short_id(&mb);
        assert_eq!(result.chars().count(), 16 + 3); // 16 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn short_id_keeps_short_input_unchanged() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
        assert_eq!(short_id("1234567890123456"), "1234567890123456");
    }

    #[test]
    fn skill_instructions_marker_detected() {
        // The internal-message detection now also matches skill blocks
        // (added when the runtime injects a skill description).
        assert!(is_internal_session_message(
            "<skill name=\"git\">use git</skill>"
        ));
    }
}
