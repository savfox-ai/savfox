use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use crate::home_paths::ambient_state_path;
use crate::json_store;

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

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedAmbientState {
    sessions: HashMap<String, Vec<AmbientMessage>>,
}

#[derive(Debug, Default)]
struct AmbientRuntimeStore {
    sessions_by_path: HashMap<PathBuf, HashMap<String, Vec<AmbientMessage>>>,
}

fn ambient_store() -> &'static Mutex<AmbientRuntimeStore> {
    static STORE: OnceLock<Mutex<AmbientRuntimeStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(AmbientRuntimeStore::default()))
}

async fn ensure_store_loaded(savfox_home: &Path) -> PathBuf {
    let path = ambient_state_path(savfox_home);
    {
        let store = ambient_store().lock().await;
        if store.sessions_by_path.contains_key(&path) {
            return path;
        }
    }
    let persisted: PersistedAmbientState = json_store::load_json(&path, "ambient state")
        .await
        .unwrap_or_default();
    let mut store = ambient_store().lock().await;
    store
        .sessions_by_path
        .entry(path.clone())
        .or_insert(persisted.sessions);
    path
}

async fn persist_snapshot(path: PathBuf, sessions: HashMap<String, Vec<AmbientMessage>>) {
    let data = PersistedAmbientState { sessions };
    if let Err(err) = json_store::save_json(&path, &data, "ambient state").await {
        warn!("failed to persist ambient state: {err}");
    }
}

pub async fn push_ambient_message(savfox_home: &Path, session_id: &str, message: AmbientMessage) {
    let session_id = session_id.trim();
    if session_id.is_empty() || message.text.trim().is_empty() {
        return;
    }

    let path = ensure_store_loaded(savfox_home).await;
    let mut store = ambient_store().lock().await;
    {
        let entry = store
            .sessions_by_path
            .entry(path.clone())
            .or_default()
            .entry(session_id.to_owned())
            .or_default();
        entry.push(message);
        if entry.len() > MAX_AMBIENT_MESSAGES {
            let overflow = entry.len() - MAX_AMBIENT_MESSAGES;
            entry.drain(0..overflow);
        }
    }
    let snapshot = store
        .sessions_by_path
        .get(&path)
        .cloned()
        .unwrap_or_default();
    drop(store);
    persist_snapshot(path, snapshot).await;
}

pub async fn take_ambient_messages(savfox_home: &Path, session_id: &str) -> Vec<AmbientMessage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }
    let path = ensure_store_loaded(savfox_home).await;
    let mut store = ambient_store().lock().await;
    let sessions = store.sessions_by_path.entry(path.clone()).or_default();
    let messages = sessions.remove(session_id).unwrap_or_default();
    let snapshot = sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
    messages
}

pub async fn clear_ambient_messages(savfox_home: &Path, session_id: &str) -> usize {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return 0;
    }
    let path = ensure_store_loaded(savfox_home).await;
    let mut store = ambient_store().lock().await;
    let sessions = store.sessions_by_path.entry(path.clone()).or_default();
    let removed = sessions
        .remove(session_id)
        .map(|messages| messages.len())
        .unwrap_or(0);
    let snapshot = sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
    removed
}

pub async fn peek_ambient_messages(savfox_home: &Path, session_id: &str) -> Vec<AmbientMessage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }
    let path = ensure_store_loaded(savfox_home).await;
    ambient_store()
        .lock()
        .await
        .sessions_by_path
        .get(&path)
        .and_then(|sessions| sessions.get(session_id))
        .cloned()
        .unwrap_or_default()
}

pub async fn remove_ambient_session(savfox_home: &Path, session_id: &str) {
    let _ = clear_ambient_messages(savfox_home, session_id).await;
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
    use tempfile::tempdir;

    use super::{
        AmbientMessage, ambient_state_path, ambient_store, clear_ambient_messages,
        format_ambient_context, peek_ambient_messages, prepend_ambient_context,
        push_ambient_message, take_ambient_messages,
    };

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

    #[tokio::test]
    async fn ambient_messages_reload_from_disk() {
        let temp = tempdir().expect("tempdir");
        let session_id = "session-ambient-persist";
        let path = ambient_state_path(temp.path());
        let message = AmbientMessage {
            timestamp_ms: 42,
            sender_id: Some("user-1".to_owned()),
            sender_name: Some("Alice".to_owned()),
            sender_kind: "human".to_owned(),
            text: "persist me".to_owned(),
            reason: "no_trigger".to_owned(),
        };

        push_ambient_message(temp.path(), session_id, message.clone()).await;

        {
            let mut store = ambient_store().lock().await;
            store.sessions_by_path.remove(&path);
        }

        let peeked = peek_ambient_messages(temp.path(), session_id).await;
        assert_eq!(peeked, vec![message.clone()]);

        let taken = take_ambient_messages(temp.path(), session_id).await;
        assert_eq!(taken, vec![message.clone()]);

        {
            let mut store = ambient_store().lock().await;
            store.sessions_by_path.remove(&path);
        }

        let after_take = peek_ambient_messages(temp.path(), session_id).await;
        assert!(after_take.is_empty());
    }

    #[tokio::test]
    async fn clear_ambient_messages_removes_persisted_entries() {
        let temp = tempdir().expect("tempdir");
        let session_id = "session-ambient-clear";
        let path = ambient_state_path(temp.path());
        push_ambient_message(
            temp.path(),
            session_id,
            AmbientMessage {
                timestamp_ms: 7,
                sender_id: None,
                sender_name: Some("Bob".to_owned()),
                sender_kind: "human".to_owned(),
                text: "clear me".to_owned(),
                reason: "no_trigger".to_owned(),
            },
        )
        .await;

        let removed = clear_ambient_messages(temp.path(), session_id).await;
        assert_eq!(removed, 1);

        {
            let mut store = ambient_store().lock().await;
            store.sessions_by_path.remove(&path);
        }

        assert!(
            peek_ambient_messages(temp.path(), session_id)
                .await
                .is_empty()
        );
    }
}
