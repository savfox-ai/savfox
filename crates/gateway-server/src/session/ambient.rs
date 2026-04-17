use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const MAX_AMBIENT_MESSAGES: usize = 16;
const MAX_AMBIENT_CHARS: usize = 4_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmbientMessage {
    pub timestamp_ms: u64,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub sender_kind: String,
    pub text: String,
    pub reason: String,
}

fn ambient_store() -> &'static Mutex<HashMap<String, Vec<AmbientMessage>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Vec<AmbientMessage>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn push_ambient_message(session_id: &str, message: AmbientMessage) {
    let session_id = session_id.trim();
    if session_id.is_empty() || message.text.trim().is_empty() {
        return;
    }

    let mut lock = ambient_store().lock().await;
    let entry = lock.entry(session_id.to_owned()).or_default();
    entry.push(message);
    if entry.len() > MAX_AMBIENT_MESSAGES {
        let overflow = entry.len() - MAX_AMBIENT_MESSAGES;
        entry.drain(0..overflow);
    }
}

pub async fn take_ambient_messages(session_id: &str) -> Vec<AmbientMessage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }
    ambient_store()
        .lock()
        .await
        .remove(session_id)
        .unwrap_or_default()
}

pub async fn peek_ambient_messages(session_id: &str) -> Vec<AmbientMessage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }
    ambient_store()
        .lock()
        .await
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

pub fn format_ambient_context(messages: &[AmbientMessage]) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let mut remaining = MAX_AMBIENT_CHARS;
    for message in messages.iter().rev() {
        let speaker = message
            .sender_name
            .as_deref()
            .or(message.sender_id.as_deref())
            .unwrap_or("unknown");
        let line = format!("- {speaker}: {}", message.text.trim());
        let line_len = line.chars().count() + 1;
        if line_len > remaining {
            break;
        }
        remaining = remaining.saturating_sub(line_len);
        lines.push(line);
    }

    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(format!(
        "[ambient context since last reply]\n{}",
        lines.join("\n")
    ))
}

pub fn prepend_ambient_context(prompt: &str, ambient_context: Option<&str>) -> String {
    let Some(ambient_context) = ambient_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return prompt.to_owned();
    };

    let trimmed_prompt = prompt.trim();
    if trimmed_prompt.is_empty() {
        ambient_context.to_owned()
    } else {
        format!("{ambient_context}\n\n[current message]\n{trimmed_prompt}")
    }
}

#[cfg(test)]
mod tests {
    use super::{AmbientMessage, format_ambient_context, prepend_ambient_context};

    #[test]
    fn format_ambient_context_renders_lines() {
        let rendered = format_ambient_context(&[
            AmbientMessage {
                timestamp_ms: 1,
                sender_id: Some("user-1".to_owned()),
                sender_name: Some("Alice".to_owned()),
                sender_kind: "human".to_owned(),
                text: "hello".to_owned(),
                reason: "no_trigger".to_owned(),
            },
            AmbientMessage {
                timestamp_ms: 2,
                sender_id: Some("user-2".to_owned()),
                sender_name: None,
                sender_kind: "human".to_owned(),
                text: "world".to_owned(),
                reason: "mentioned_other_agent".to_owned(),
            },
        ])
        .expect("context should render");

        assert!(rendered.contains("[ambient context since last reply]"));
        assert!(rendered.contains("- Alice: hello"));
        assert!(rendered.contains("- user-2: world"));
    }

    #[test]
    fn prepend_ambient_context_wraps_current_message() {
        let out = prepend_ambient_context("answer this", Some("[ambient]\n- Alice: hi"));
        assert!(out.contains("[ambient]"));
        assert!(out.contains("[current message]"));
        assert!(out.ends_with("answer this"));
    }
}
