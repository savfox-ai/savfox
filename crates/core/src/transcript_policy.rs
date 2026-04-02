//! Transcript policy for controlling what gets recorded in session history.
//!
//! Provides fine-grained control over:
//! - What messages are persisted
//! - Redaction of sensitive content
//! - Retention periods
//! - Export restrictions

use serde::{Deserialize, Serialize};

/// Transcript policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptPolicy {
    /// Whether to record transcripts at all.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// What to include in the transcript.
    #[serde(default)]
    pub include: IncludePolicy,
    /// Redaction rules for sensitive content.
    #[serde(default)]
    pub redaction: RedactionPolicy,
    /// Retention policy.
    #[serde(default)]
    pub retention: RetentionPolicy,
    /// Export restrictions.
    #[serde(default)]
    pub export: ExportPolicy,
}

fn default_true() -> bool {
    true
}

impl Default for TranscriptPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            include: IncludePolicy::default(),
            redaction: RedactionPolicy::default(),
            retention: RetentionPolicy::default(),
            export: ExportPolicy::default(),
        }
    }
}

/// What to include in the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludePolicy {
    /// Include user messages.
    #[serde(default = "default_true")]
    pub user_messages: bool,
    /// Include assistant messages.
    #[serde(default = "default_true")]
    pub assistant_messages: bool,
    /// Include system messages.
    #[serde(default)]
    pub system_messages: bool,
    /// Include tool calls and results.
    #[serde(default = "default_true")]
    pub tool_calls: bool,
    /// Include thinking/reasoning blocks.
    #[serde(default)]
    pub thinking: bool,
    /// Include metadata (timestamps, token counts).
    #[serde(default = "default_true")]
    pub metadata: bool,
}

impl Default for IncludePolicy {
    fn default() -> Self {
        Self {
            user_messages: true,
            assistant_messages: true,
            system_messages: false,
            tool_calls: true,
            thinking: false,
            metadata: true,
        }
    }
}

/// Redaction rules for sensitive content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RedactionPolicy {
    /// Redact patterns matching these regex patterns.
    #[serde(default)]
    pub patterns: Vec<RedactionPattern>,
    /// Redact API keys and tokens.
    #[serde(default = "default_true")]
    pub api_keys: bool,
    /// Redact email addresses.
    #[serde(default)]
    pub emails: bool,
    /// Redact IP addresses.
    #[serde(default)]
    pub ip_addresses: bool,
    /// Replacement string for redacted content.
    #[serde(default = "default_redaction_replacement")]
    pub replacement: String,
}

fn default_redaction_replacement() -> String {
    "[REDACTED]".to_owned()
}

/// A custom redaction pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionPattern {
    /// Name of this pattern (for logging).
    pub name: String,
    /// Regex pattern to match.
    pub pattern: String,
    /// Optional custom replacement (overrides global).
    pub replacement: Option<String>,
}

/// Retention policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Maximum age of transcripts in days (0 = forever).
    #[serde(default)]
    pub max_age_days: u32,
    /// Maximum number of transcripts to keep (0 = unlimited).
    #[serde(default)]
    pub max_count: u32,
    /// Maximum total size in MB (0 = unlimited).
    #[serde(default)]
    pub max_size_mb: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_days: 90,
            max_count: 0,
            max_size_mb: 0,
        }
    }
}

/// Export restrictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPolicy {
    /// Allow exporting as JSON.
    #[serde(default = "default_true")]
    pub json: bool,
    /// Allow exporting as Markdown.
    #[serde(default = "default_true")]
    pub markdown: bool,
    /// Allow exporting as CSV.
    #[serde(default = "default_true")]
    pub csv: bool,
    /// Apply redaction to exports.
    #[serde(default = "default_true")]
    pub apply_redaction: bool,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            json: true,
            markdown: true,
            csv: true,
            apply_redaction: true,
        }
    }
}

/// Apply redaction policy to a text string.
#[must_use]
pub fn redact(text: &str, policy: &RedactionPolicy) -> String {
    let mut result = text.to_owned();

    // Built-in API key pattern
    if policy.api_keys {
        let api_key_re = regex::Regex::new(
            r"(?i)(sk-[a-zA-Z0-9]{20,}|ghp_[a-zA-Z0-9]{36}|xoxb-[a-zA-Z0-9-]+|AIza[a-zA-Z0-9_-]{35})"
        )
        .expect("built-in API key regex should compile");
        result = api_key_re
            .replace_all(&result, &policy.replacement)
            .to_string();
    }

    // Email addresses
    if policy.emails {
        let email_re = regex::Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}")
            .expect("built-in email regex should compile");
        result = email_re
            .replace_all(&result, &policy.replacement)
            .to_string();
    }

    // IP addresses
    if policy.ip_addresses {
        let ip_re = regex::Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b")
            .expect("built-in IP regex should compile");
        result = ip_re.replace_all(&result, &policy.replacement).to_string();
    }

    // Custom patterns
    for pattern in &policy.patterns {
        if let Ok(re) = regex::Regex::new(&pattern.pattern) {
            let replacement = pattern
                .replacement
                .as_deref()
                .unwrap_or(&policy.replacement);
            result = re.replace_all(&result, replacement).to_string();
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_api_keys() {
        let policy = RedactionPolicy {
            api_keys: true,
            replacement: "[REDACTED]".to_owned(),
            ..Default::default()
        };

        let text = "My key is sk-proj1234567890abcdefghij";
        let result = redact(text, &policy);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("sk-proj"));
    }

    #[test]
    fn test_redact_emails() {
        let policy = RedactionPolicy {
            emails: true,
            replacement: "[REDACTED]".to_owned(),
            ..Default::default()
        };

        let text = "Contact user@example.com for help";
        let result = redact(text, &policy);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("user@example.com"));
    }

    #[test]
    fn test_redact_ip_addresses() {
        let policy = RedactionPolicy {
            ip_addresses: true,
            replacement: "[REDACTED]".to_owned(),
            ..Default::default()
        };

        let text = "Server at 192.168.1.100";
        let result = redact(text, &policy);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("192.168.1.100"));
    }

    #[test]
    fn test_custom_pattern() {
        let policy = RedactionPolicy {
            patterns: vec![RedactionPattern {
                name: "SSN".to_owned(),
                pattern: r"\d{3}-\d{2}-\d{4}".to_owned(),
                replacement: Some("[SSN]".to_owned()),
            }],
            ..Default::default()
        };

        let text = "SSN is 123-45-6789";
        let result = redact(text, &policy);
        assert_eq!(result, "SSN is [SSN]");
    }

    #[test]
    fn test_default_policy() {
        let policy = TranscriptPolicy::default();
        assert!(policy.enabled);
        assert!(policy.include.user_messages);
        assert!(policy.include.assistant_messages);
        assert!(!policy.include.system_messages);
        assert!(!policy.include.thinking);
        assert_eq!(policy.retention.max_age_days, 90);
    }
}
