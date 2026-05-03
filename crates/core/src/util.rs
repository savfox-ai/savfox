use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::RngExt;
use savfox_protocol::SessionId;
use tracing::{debug, error};

use crate::parse_command::shlex_join;

const INITIAL_DELAY_MS: u64 = 200;
const BACKOFF_FACTOR: f64 = 2.0;

/// Emit structured feedback metadata as key/value pairs.
///
/// This logs a tracing event with `target: "feedback_tags"`. If
/// `savfox_feedback::SavfoxFeedback::metadata_layer()` is installed, these fields are captured and
/// later attached as tags when feedback is uploaded.
///
/// Values are wrapped with [`tracing::field::DebugValue`], so the expression only needs to
/// implement [`std::fmt::Debug`].
///
/// Example:
///
/// ```rust
/// savfox_core::feedback_tags!(model = "gpt-5", cached = true);
/// savfox_core::feedback_tags!(provider = provider_id, request_id = request_id);
/// ```
#[macro_export]
macro_rules! feedback_tags {
    ($( $key:ident = $value:expr ),+ $(,)?) => {
        ::tracing::info!(
            target: "feedback_tags",
            $( $key = ::tracing::field::debug(&$value) ),+
        );
    };
}

pub(crate) fn backoff(attempt: u64) -> Duration {
    let exp = BACKOFF_FACTOR.powi(attempt.saturating_sub(1) as i32);
    let base = (INITIAL_DELAY_MS as f64 * exp) as u64;
    let jitter = rand::rng().random_range(0.9..1.1);
    Duration::from_millis((base as f64 * jitter) as u64)
}

pub(crate) fn error_or_panic(message: impl std::string::ToString) {
    if cfg!(debug_assertions) {
        panic!("{}", message.to_string());
    } else {
        error!("{}", message.to_string());
    }
}

pub(crate) fn try_parse_error_message(text: &str) -> String {
    debug!("Parsing server error response: {}", text);
    let json = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
    if let Some(error) = json.get("error")
        && let Some(message) = error.get("message")
        && let Some(message_str) = message.as_str()
    {
        return message_str.to_owned();
    }
    if text.is_empty() {
        return "Unknown error".to_owned();
    }
    text.to_owned()
}

#[must_use]
pub fn resolve_path(base: &Path, path: &PathBuf) -> PathBuf {
    if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    }
}

/// Trim a session name and return `None` if it is empty after trimming.
#[must_use]
pub fn normalize_session_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub fn resume_command(session_name: Option<&str>, session_id: Option<SessionId>) -> Option<String> {
    let resume_target = session_name
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| session_id.map(|session_id| session_id.to_string()));
    resume_target.map(|target| {
        let needs_double_dash = target.starts_with('-');
        let escaped = shlex_join(&[target]);
        if needs_double_dash {
            format!("savfox resume -- {escaped}")
        } else {
            format!("savfox resume {escaped}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_parse_error_message() {
        let text = r#"{
  "error": {
    "message": "Your refresh token has already been used to generate a new access token. Please try signing in again.",
    "type": "invalid_request_error",
    "param": null,
    "code": "refresh_token_reused"
  }
}"#;
        let message = try_parse_error_message(text);
        assert_eq!(
            message,
            "Your refresh token has already been used to generate a new access token. Please try signing in again."
        );
    }

    #[test]
    fn test_try_parse_error_message_no_error() {
        let text = r#"{"message": "test"}"#;
        let message = try_parse_error_message(text);
        assert_eq!(message, r#"{"message": "test"}"#);
    }

    #[test]
    fn feedback_tags_macro_compiles() {
        #[derive(Debug)]
        struct OnlyDebug;

        feedback_tags!(model = "gpt-5", cached = true, debug_only = OnlyDebug);
    }

    #[test]
    fn normalize_session_name_trims_and_rejects_empty() {
        assert_eq!(normalize_session_name("   "), None);
        assert_eq!(
            normalize_session_name("  my session  "),
            Some("my session".to_owned())
        );
    }

    #[test]
    fn resume_command_prefers_name_over_id() {
        let session_id = SessionId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let command = resume_command(Some("my-session"), Some(session_id));
        assert_eq!(command, Some("savfox resume my-session".to_owned()));
    }

    #[test]
    fn resume_command_with_only_id() {
        let session_id = SessionId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let command = resume_command(None, Some(session_id));
        assert_eq!(
            command,
            Some("savfox resume 123e4567-e89b-12d3-a456-426614174000".to_owned())
        );
    }

    #[test]
    fn resume_command_with_no_name_or_id() {
        let command = resume_command(None, None);
        assert_eq!(command, None);
    }

    #[test]
    fn resume_command_quotes_session_name_when_needed() {
        let command = resume_command(Some("-starts-with-dash"), None);
        assert_eq!(
            command,
            Some("savfox resume -- -starts-with-dash".to_owned())
        );

        let command = resume_command(Some("two words"), None);
        assert_eq!(command, Some("savfox resume 'two words'".to_owned()));

        let command = resume_command(Some("quote'case"), None);
        assert_eq!(command, Some("savfox resume \"quote'case\"".to_owned()));
    }
}
