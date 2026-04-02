use serde::Deserialize;

pub const SESSION_LABEL_MAX_CHARS: usize = 48;
const USER_INSTRUCTIONS_PREFIX: &str = "# agents.md instructions for ";
const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
const TURN_ABORTED_OPEN_TAG: &str = "<turn_aborted>";
const SKILL_INSTRUCTIONS_PREFIX: &str = "<skill";

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

#[must_use] 
pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 16 {
        id.to_owned()
    } else {
        let truncated: String = id.chars().take(16).collect();
        format!("{truncated}...")
    }
}

/// Sender information for a session.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSender {
    pub user_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionEntry {
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub scope: Option<String>,
    pub label: Option<String>,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub sender: Option<SessionSender>,
    pub last_activity: Option<String>,
    pub message_count: Option<u32>,
    pub messages: Option<u32>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub thread_id: Option<String>,
}

impl SessionEntry {
    #[must_use] 
    pub fn display_id(&self) -> String {
        self.session_id
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("-").to_owned()
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
            .map(ToString::to_string)
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
            .unwrap_or("-").to_owned()
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
}
