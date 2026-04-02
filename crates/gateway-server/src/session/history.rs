use std::path::{Path, PathBuf};
use std::sync::Arc;

use savfox_core::{
    RolloutRecorder, find_archived_session_path_by_id_str, find_session_path_by_id_str,
};
use savfox_gateway_shared::{is_internal_session_message, normalize_session_label};
use savfox_protocol::models::{ContentItem, ResponseItem};
use savfox_protocol::protocol::{EventMsg, RolloutItem};
use serde_json::{Value, json};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::channel::GatewayChannel;
use crate::session::{SessionEntry, SessionStore};

pub(crate) async fn build_history_payload(
    session_ref: &str,
    limit: usize,
    source_channel: Option<&str>,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> Value {
    let entry = match session_store.get(session_ref).await {
        Some(v) => Some(v),
        None => session_store.get_by_session_id(session_ref).await,
    };

    let session_id = entry
        .as_ref()
        .map(|v| v.session_id.clone())
        .unwrap_or_else(|| session_ref.to_owned());

    let savfox_home = &channel.config().savfox_home;
    let rollout_path = match resolve_rollout_path(savfox_home, session_ref, entry.as_ref()).await {
        Ok(v) => v,
        Err(err) => {
            return json!({
                "session_ref": session_ref,
                "session_id": session_id,
                "session": entry.as_ref().and_then(|v| serde_json::to_value(v).ok()),
                "messages": [],
                "limit": limit,
                "error": err,
            });
        }
    };

    let Some(path) = rollout_path else {
        return json!({
            "session_ref": session_ref,
            "session_id": session_id,
            "session": entry.as_ref().and_then(|v| serde_json::to_value(v).ok()),
            "messages": [],
            "limit": limit,
            "note": "no rollout file found for this session yet",
        });
    };

    debug!("Loading rollout history from file: {}", path.display());
    match RolloutRecorder::get_rollout_history(&path).await {
        Ok(history) => {
            let items = history.get_rollout_items();
            let all_messages = extract_messages(&items);
            let total = all_messages.len();
            let filtered = attach_provenance(all_messages, entry.as_ref(), source_channel);
            let messages = tail_limit(filtered, limit);
            debug!(
                "Successfully loaded rollout: rollout_items={}, messages={}",
                items.len(),
                messages.len()
            );
            json!({
                "session_ref": session_ref,
                "session_id": session_id,
                "session": entry.as_ref().and_then(|v| serde_json::to_value(v).ok()),
                "rollout_path": path.to_string_lossy(),
                "rollout_items": items.len(),
                "messages_total": total,
                "source_channel": source_channel,
                "messages": messages,
                "limit": limit,
            })
        }
        Err(err) => {
            warn!(
                "Failed to load rollout history: path={}, error={}",
                path.display(),
                err
            );
            json!({
                "session_ref": session_ref,
                "session_id": session_id,
                "session": entry.as_ref().and_then(|v| serde_json::to_value(v).ok()),
                "rollout_path": path.to_string_lossy(),
                "messages": [],
                "limit": limit,
                "error": format!("failed to load rollout history: {err}"),
            })
        }
    }
}

fn attach_provenance(
    messages: Vec<Value>,
    entry: Option<&SessionEntry>,
    source_channel: Option<&str>,
) -> Vec<Value> {
    let Some(entry) = entry else {
        return messages;
    };
    let filter = source_channel
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    let mut user_index = 0usize;
    let mut out = Vec::with_capacity(messages.len());
    for mut message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "user" {
            let provenance = entry.provenance.get(user_index).cloned();
            user_index = user_index.saturating_add(1);
            if let Some(provenance) = provenance {
                let matches_filter = filter
                    .as_deref()
                    .map(|required| provenance.channel.to_ascii_lowercase() == required)
                    .unwrap_or(true);
                if !matches_filter {
                    continue;
                }
                if let Some(map) = message.as_object_mut() {
                    map.insert(
                        "provenance".to_owned(),
                        serde_json::to_value(provenance).unwrap_or(Value::Null),
                    );
                }
            } else if filter.is_some() {
                continue;
            }
        } else if filter.is_some() {
            continue;
        }
        out.push(message);
    }
    out
}

pub(crate) fn session_file_to_store_value(savfox_home: &Path, path: &Path) -> String {
    let sessions_root = savfox_home.join("sessions");
    if let Ok(relative) = path.strip_prefix(&sessions_root) {
        // Remove .jsonl extension and return just the UUID
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        relative_str
            .strip_suffix(".jsonl")
            .unwrap_or(&relative_str)
            .to_owned()
    } else {
        // If the path is absolute and doesn't match the sessions root,
        // try to extract just the UUID filename
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
            .map(|s| s.to_owned())
            .unwrap_or_else(|| path.to_string_lossy().to_string())
    }
}

async fn resolve_rollout_path(
    savfox_home: &Path,
    session_ref: &str,
    entry: Option<&SessionEntry>,
) -> Result<Option<PathBuf>, String> {
    debug!("resolve_rollout_path called: session_ref={}", session_ref);

    if let Some(session_file) = entry.and_then(|v| v.session_file.as_deref()) {
        let path = PathBuf::from(session_file);
        let absolute = if path.is_absolute() {
            path
        } else {
            savfox_home
                .join("sessions")
                .join(format!("{session_file}.jsonl"))
        };
        debug!(
            "Trying session_file from entry: path={}",
            absolute.display()
        );
        if tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
            debug!(
                "Found rollout file from session_file: {}",
                absolute.display()
            );
            return Ok(Some(absolute));
        }
    }

    let mut candidates = vec![session_ref.to_owned()];
    if let Some(existing) = entry {
        candidates.push(existing.session_id.clone());
        if let Some(thread_id) = &existing.thread_id {
            candidates.push(thread_id.clone());
        }
    }

    debug!("Searching for rollout with {:#?} candidate IDs", candidates);
    for candidate in candidates {
        // First, try direct lookup as UUID v7 filename
        if Uuid::parse_str(&candidate).is_ok() {
            let direct_path = savfox_home
                .join("sessions")
                .join(format!("{candidate}.jsonl"));
            debug!("Trying direct UUID v7 path: {}", direct_path.display());
            if tokio::fs::try_exists(&direct_path).await.unwrap_or(false) {
                debug!(
                    "Found rollout file by direct UUID lookup: {}",
                    direct_path.display()
                );
                return Ok(Some(direct_path));
            }
        }

        // Fall back to the old search methods for compatibility
        debug!("Searching for rollout with thread_id={}", candidate);
        if let Some(path) = find_session_path_by_id_str(savfox_home, &candidate)
            .await
            .map_err(|err| format!("failed to locate rollout by thread id: {err}"))?
        {
            debug!("Found rollout file: {}", path.display());
            return Ok(Some(path));
        }
        if let Some(path) = find_archived_session_path_by_id_str(savfox_home, &candidate)
            .await
            .map_err(|err| format!("failed to locate archived rollout by thread id: {err}"))?
        {
            debug!("Found archived rollout file: {}", path.display());
            return Ok(Some(path));
        }
    }

    debug!("No rollout file found for session_ref={}", session_ref);
    Ok(None)
}

fn extract_messages(items: &[RolloutItem]) -> Vec<Value> {
    let mut from_responses: Vec<(usize, String, String, &'static str)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. }) => {
                // Keep chat-relevant response-item roles and skip non-chat/system entries.
                if role != "assistant" && role != "user" {
                    continue;
                }
                let text = content_text(content);
                if !text.is_empty() {
                    if role == "user" && is_internal_session_message(&text) {
                        continue;
                    }
                    from_responses.push((index, role.clone(), text, "response_item"));
                }
            }
            RolloutItem::Compacted(compacted) => {
                from_responses.push((
                    index,
                    "assistant".to_owned(),
                    compacted.message.clone(),
                    "compacted",
                ));
            }
            _ => {}
        }
    }
    let responses_have_user = from_responses.iter().any(|(_, role, ..)| role == "user");
    let responses_have_assistant = from_responses
        .iter()
        .any(|(_, role, ..)| role == "assistant");

    let mut from_events: Vec<(usize, String, String, &'static str)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            RolloutItem::EventMsg(EventMsg::UserMessage(message)) => {
                if !message.message.is_empty()
                    && !is_internal_session_message(message.message.as_str())
                {
                    from_events.push((index, "user".to_owned(), message.message.clone(), "event"));
                }
            }
            RolloutItem::EventMsg(EventMsg::AgentMessage(message)) => {
                if !message.message.is_empty() {
                    from_events.push((
                        index,
                        "assistant".to_owned(),
                        message.message.clone(),
                        "event",
                    ));
                }
            }
            _ => {}
        }
    }

    let mut merged = from_responses;
    if !responses_have_user {
        merged.extend(
            from_events
                .iter()
                .filter(|(_, role, ..)| role == "user")
                .cloned(),
        );
    }
    if !responses_have_assistant {
        merged.extend(
            from_events
                .iter()
                .filter(|(_, role, ..)| role == "assistant")
                .cloned(),
        );
    }
    if merged.is_empty() {
        merged = from_events;
    }

    // Stable chronological ordering by rollout index.
    merged.sort_by_key(|(index, ..)| *index);

    merged
        .into_iter()
        .map(|(index, role, text, source)| {
            json!({
                "index": index,
                "role": role,
                "text": text,
                "source": source,
            })
        })
        .collect()
}

pub(crate) fn derive_session_label_from_messages(messages: &[Value]) -> Option<String> {
    messages.iter().find_map(|message| {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        message
            .get("text")
            .or_else(|| message.get("content"))
            .and_then(Value::as_str)
            .and_then(normalize_session_label)
    })
}

pub(crate) async fn derive_session_label_from_history(
    session_ref: &str,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> Option<String> {
    let entry = match session_store.get(session_ref).await {
        Some(v) => Some(v),
        None => session_store.get_by_session_id(session_ref).await,
    };
    let savfox_home = &channel.config().savfox_home;
    let rollout_path = resolve_rollout_path(savfox_home, session_ref, entry.as_ref())
        .await
        .ok()
        .flatten()?;
    let history = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .ok()?;
    let items = history.get_rollout_items();
    let messages = extract_messages(&items);
    derive_session_label_from_messages(&messages)
}

fn content_text(content: &[ContentItem]) -> String {
    let mut buf = String::new();
    for item in content {
        let part = match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.as_str(),
            ContentItem::InputImage { .. } => "",
        };
        if part.is_empty() {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(part);
    }
    buf
}

fn tail_limit(mut values: Vec<Value>, limit: usize) -> Vec<Value> {
    if values.len() > limit {
        values = values.split_off(values.len() - limit);
    }
    values
}

#[cfg(test)]
mod tests {
    use savfox_protocol::models::{ContentItem, ResponseItem};
    use savfox_protocol::protocol::{AgentMessageEvent, EventMsg, RolloutItem, UserMessageEvent};

    use super::{derive_session_label_from_messages, extract_messages};

    #[test]
    fn extract_messages_keeps_user_event_when_response_items_only_have_assistant() {
        let items = vec![
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: "write rust hello world".to_owned(),
                images: Some(vec![]),
                local_images: vec![],
                text_elements: vec![],
            })),
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".to_owned(),
                content: vec![ContentItem::OutputText {
                    text: "Sure, here's the code".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
            RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: "fallback assistant event".to_owned(),
            })),
        ];

        let messages = extract_messages(&items);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "write rust hello world");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["text"], "Sure, here's the code");
    }

    #[test]
    fn extract_messages_reads_user_from_response_items_when_present() {
        let items = vec![
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_owned(),
                content: vec![ContentItem::InputText {
                    text: "question".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".to_owned(),
                content: vec![ContentItem::OutputText {
                    text: "answer".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
        ];

        let messages = extract_messages(&items);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "question");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["text"], "answer");
    }

    #[test]
    fn extract_messages_filters_internal_prefix_messages() {
        let items = vec![
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_owned(),
                content: vec![ContentItem::InputText {
                    text: "# AGENTS.md instructions for /repo".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_owned(),
                content: vec![ContentItem::InputText {
                    text: "<environment_context>\nctx\n</environment_context>".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_owned(),
                content: vec![ContentItem::InputText {
                    text: "actual user question".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".to_owned(),
                content: vec![ContentItem::OutputText {
                    text: "answer".to_owned(),
                }],
                end_turn: None,
                phase: None,
            }),
        ];

        let messages = extract_messages(&items);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "actual user question");
        assert_eq!(messages[1]["role"], "assistant");
    }

    #[test]
    fn derive_session_label_prefers_first_meaningful_user_message() {
        let messages = vec![
            serde_json::json!({ "role": "assistant", "text": "hello" }),
            serde_json::json!({ "role": "user", "text": "   " }),
            serde_json::json!({ "role": "user", "text": "First valid label message" }),
            serde_json::json!({ "role": "user", "text": "Other" }),
        ];

        let label = derive_session_label_from_messages(&messages);
        assert_eq!(label.as_deref(), Some("First valid label message"));
    }
}
