use std::collections::VecDeque;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use savfox_core::{find_archived_session_path_by_id_str, find_session_path_by_id_str};
use savfox_gateway_shared::{is_internal_session_message, normalize_session_label};
use savfox_protocol::models::{ContentItem, ResponseItem};
use savfox_protocol::protocol::{EventMsg, RolloutItem, RolloutLine};
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
    principal: Option<&str>,
) -> Value {
    let entry = session_store.get(session_ref).await;

    let session_id = entry
        .as_ref()
        .map(|v| v.session_id.clone())
        .unwrap_or_else(|| session_ref.to_owned());

    let savfox_home = &channel.config().savfox_home;
    if channel
        .claude_runtime
        .authorize_session(principal, &session_id)
        .await
        .is_err()
    {
        return json!({"error":"Claude chat is outside your execution scope","messages":[]});
    }
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
    let history_entry = entry.clone();
    let history_source = source_channel.map(str::to_owned);
    let history_path = path.clone();
    let page = tokio::task::spawn_blocking(move || {
        read_history_page(
            &history_path,
            limit,
            history_entry.as_ref(),
            history_source.as_deref(),
        )
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    match page {
        Ok(page) => {
            let messages = page.messages;
            let total = page.messages_total;
            debug!(
                "Successfully loaded rollout: rollout_items={}, messages={}",
                page.rollout_items,
                messages.len()
            );
            json!({
                "session_ref": session_ref,
                "session_id": session_id,
                "session": entry.as_ref().and_then(|v| serde_json::to_value(v).ok()),
                "rollout_path": path.to_string_lossy(),
                "rollout_items": page.rollout_items,
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

struct HistoryPage {
    messages: Vec<Value>,
    messages_total: usize,
    rollout_items: usize,
}

fn read_history_page(
    path: &Path,
    limit: usize,
    entry: Option<&SessionEntry>,
    source_channel: Option<&str>,
) -> std::io::Result<HistoryPage> {
    let filter = source_channel
        .map(str::trim)
        .filter(|source| !source.is_empty());
    let mut messages = VecDeque::new();
    let mut messages_total = 0;
    let mut user_index = 0;
    let rollout_items = project_history(path, |index, role, text, source| {
        messages_total += 1;
        let provenance = if role == "user" {
            let provenance = entry.and_then(|entry| entry.provenance.get(user_index));
            user_index += 1;
            provenance
        } else {
            None
        };
        // Without metadata the old implementation did not apply a filter.
        if entry.is_some()
            && let Some(filter) = filter
            && (role != "user"
                || !provenance.is_some_and(|p| p.channel.eq_ignore_ascii_case(filter)))
        {
            return;
        }
        if limit == 0 {
            return;
        }
        let mut message = json!({"index":index,"role":role,"text":text,"source":source});
        if let Some(provenance) = provenance {
            message["provenance"] = serde_json::to_value(provenance).unwrap_or(Value::Null);
        }
        if messages.len() == limit {
            messages.pop_front();
        }
        messages.push_back(message);
    })?;
    Ok(HistoryPage {
        messages: messages.into_iter().collect(),
        messages_total,
        rollout_items,
    })
}

// Both the tail and the first-user label use this one chronological projection.
// The first streaming pass selects response-vs-event precedence for each role;
// the second emits only selected messages within the same frozen file length.
// Memory is bounded by the caller's aggregate and the longest JSONL record.
fn project_history(
    path: &Path,
    mut visit: impl FnMut(usize, &str, String, &'static str),
) -> std::io::Result<usize> {
    let length = std::fs::metadata(path)?.len();
    let mut response_users = false;
    let mut response_assistants = false;
    let mut has_session = false;
    let mut rollout_items = 0;
    scan_history(path, length, |_, item| {
        rollout_items += 1;
        has_session |= matches!(item, RolloutItem::SessionMeta(_));
        if let Some((role, _, source)) = history_message(&item)
            && source != "event"
        {
            response_users |= role == "user";
            response_assistants |= role == "assistant";
        }
    })?;
    if !has_session {
        return Err(std::io::Error::other(
            "failed to parse session ID from rollout file",
        ));
    }
    scan_history(path, length, |index, item| {
        let Some((role, text, source)) = history_message(&item) else {
            return;
        };
        if source != "event"
            || !((role == "user" && response_users) || (role == "assistant" && response_assistants))
        {
            visit(index, role, text, source);
        }
    })?;
    Ok(rollout_items)
}

fn read_history_label(path: &Path) -> std::io::Result<Option<String>> {
    let mut label = None;
    project_history(path, |_, role, text, _| {
        if label.is_none() && role == "user" {
            label = normalize_session_label(&text);
        }
    })?;
    Ok(label)
}

fn scan_history(
    path: &Path,
    length: u64,
    mut visit: impl FnMut(usize, RolloutItem),
) -> std::io::Result<()> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file.take(length));
    let mut line = String::new();
    let mut index = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Ok(line) = serde_json::from_str::<RolloutLine>(&line) {
            visit(index, line.item);
            index += 1;
        }
    }
    Ok(())
}

fn history_message(item: &RolloutItem) -> Option<(&str, String, &'static str)> {
    match item {
        RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. })
            if role == "user" || role == "assistant" =>
        {
            let text = content_text(content);
            if text.is_empty() || (role == "user" && is_internal_session_message(&text)) {
                None
            } else {
                Some((role, text, "response_item"))
            }
        }
        RolloutItem::Compacted(compacted) => {
            Some(("assistant", compacted.message.clone(), "compacted"))
        }
        RolloutItem::EventMsg(EventMsg::UserMessage(message))
            if !message.message.is_empty() && !is_internal_session_message(&message.message) =>
        {
            Some(("user", message.message.clone(), "event"))
        }
        RolloutItem::EventMsg(EventMsg::AgentMessage(message)) if !message.message.is_empty() => {
            Some(("assistant", message.message.clone(), "event"))
        }
        _ => None,
    }
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
        if let Some(thread_id) = &existing.core_thread_id {
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

pub(crate) async fn derive_session_label_from_history(
    session_ref: &str,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> Option<String> {
    let entry = session_store.get(session_ref).await;
    let savfox_home = &channel.config().savfox_home;
    let rollout_path = resolve_rollout_path(savfox_home, session_ref, entry.as_ref())
        .await
        .ok()
        .flatten()?;
    tokio::task::spawn_blocking(move || read_history_label(&rollout_path))
        .await
        .ok()?
        .ok()?
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use savfox_protocol::models::{ContentItem, ResponseItem};
    use savfox_protocol::protocol::{
        AgentMessageEvent, CompactedItem, EventMsg, RolloutItem, RolloutLine, SessionMeta,
        SessionMetaLine, UserMessageEvent,
    };

    use super::{read_history_label, read_history_page};

    fn history_file(items: &[RolloutItem]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let meta = RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta::default(),
            git: None,
        });
        for item in std::iter::once(&meta).chain(items) {
            writeln!(
                file,
                "{}",
                serde_json::to_string(&RolloutLine {
                    timestamp: "test".into(),
                    item: item.clone(),
                })
                .unwrap()
            )
            .unwrap();
        }
        file
    }

    #[tokio::test]
    async fn rollout_lookup_uses_core_thread_id_without_remote_thread_fallback() {
        let home = tempfile::tempdir().expect("tempdir");
        let sessions = home.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("sessions directory");
        let mut entry = crate::session::SessionEntry::new();
        let logical_id = entry.session_id.clone();
        let core_id = uuid::Uuid::now_v7().to_string();
        let remote_id = uuid::Uuid::now_v7().to_string();
        let core_path = sessions.join(format!("{core_id}.jsonl"));
        let remote_path = sessions.join(format!("{remote_id}.jsonl"));
        tokio::fs::write(&core_path, b"")
            .await
            .expect("core rollout");
        tokio::fs::write(&remote_path, b"")
            .await
            .expect("unrelated remote-id rollout");
        entry.core_thread_id = Some(core_id);
        entry.remote_thread_id = Some(remote_id);
        assert_eq!(
            super::resolve_rollout_path(home.path(), &logical_id, Some(&entry))
                .await
                .expect("resolve core rollout"),
            Some(core_path)
        );
        entry.core_thread_id = None;
        assert_eq!(
            super::resolve_rollout_path(home.path(), &logical_id, Some(&entry))
                .await
                .expect("remote thread is not a core rollout candidate"),
            None
        );
    }

    #[test]
    fn streaming_tail_preserves_fallback_counts_and_source_filtering() {
        let mut items = Vec::new();
        for index in 0..100 {
            items.push(RolloutItem::EventMsg(EventMsg::UserMessage(
                UserMessageEvent {
                    message: format!("question {index}"),
                    images: None,
                    local_images: vec![],
                    text_elements: vec![],
                },
            )));
            items.push(RolloutItem::EventMsg(EventMsg::AgentMessage(
                AgentMessageEvent {
                    message: "fallback must be suppressed".into(),
                },
            )));
            items.push(RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: format!("answer {index}"),
                }],
                end_turn: None,
                phase: None,
            }));
        }
        let mut file = history_file(&items);
        writeln!(file, "invalid json").unwrap();
        let page = super::read_history_page(file.path(), 3, None, None).unwrap();
        assert_eq!(
            page.messages,
            vec![
                serde_json::json!({"index":297,"role":"assistant","text":"answer 98","source":"response_item"}),
                serde_json::json!({"index":298,"role":"user","text":"question 99","source":"event"}),
                serde_json::json!({"index":300,"role":"assistant","text":"answer 99","source":"response_item"}),
            ]
        );
        assert_eq!(page.messages_total, 200);
        assert_eq!(page.rollout_items, items.len() + 1);
        assert_eq!(
            read_history_label(file.path()).unwrap().as_deref(),
            Some("question 0")
        );
        let mut entry = crate::session::SessionEntry::new();
        for index in 0..100 {
            entry
                .provenance
                .push(crate::session::SessionMessageProvenance {
                    channel: if index % 2 == 0 {
                        "telegram:a".into()
                    } else {
                        "discord:b".into()
                    },
                    user_id: "user".into(),
                    name: "User".into(),
                    timestamp: index,
                    origin: Default::default(),
                    visibility: Default::default(),
                    channel_config_id: None,
                    account_id: None,
                    realm_id: None,
                    strand_id: None,
                    event_id: None,
                    sender_did: None,
                    sender_kind: None,
                });
        }
        let filtered =
            super::read_history_page(file.path(), 2, Some(&entry), Some("TeLeGrAm:a")).unwrap();
        assert_eq!(filtered.messages_total, 200);
        assert_eq!(filtered.messages.len(), 2);
        assert_eq!(filtered.messages[0]["text"], "question 96");
        assert_eq!(filtered.messages[1]["text"], "question 98");
        assert!(
            super::read_history_page(file.path(), 0, Some(&entry), None)
                .unwrap()
                .messages
                .is_empty()
        );
    }

    #[test]
    fn projection_keeps_user_event_when_response_items_only_have_assistant() {
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

        let file = history_file(&items);
        let messages = read_history_page(file.path(), 10, None, None)
            .unwrap()
            .messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "write rust hello world");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["text"], "Sure, here's the code");
        assert_eq!(
            read_history_label(file.path()).unwrap().as_deref(),
            Some("write rust hello world")
        );
    }

    #[test]
    fn projection_prefers_response_users_for_both_page_and_label() {
        let items = vec![
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: "event user must be suppressed".into(),
                images: None,
                local_images: vec![],
                text_elements: vec![],
            })),
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

        let file = history_file(&items);
        let messages = read_history_page(file.path(), 10, None, None)
            .unwrap()
            .messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "question");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["text"], "answer");
        assert_eq!(
            read_history_label(file.path()).unwrap().as_deref(),
            Some("question")
        );
    }

    #[test]
    fn projection_keeps_event_history_and_compacted_assistant_precedence() {
        let mut items = vec![
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: "event question".into(),
                images: None,
                local_images: vec![],
                text_elements: vec![],
            })),
            RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: "event answer".into(),
            })),
        ];
        let file = history_file(&items);
        let page = read_history_page(file.path(), 10, None, None).unwrap();
        assert_eq!(
            page.messages,
            vec![
                serde_json::json!({"index":1,"role":"user","text":"event question","source":"event"}),
                serde_json::json!({"index":2,"role":"assistant","text":"event answer","source":"event"}),
            ]
        );
        items.push(RolloutItem::Compacted(CompactedItem {
            message: "compacted answer".into(),
            replacement_history: None,
        }));
        let file = history_file(&items);
        let page = read_history_page(file.path(), 10, None, None).unwrap();
        assert_eq!(
            page.messages,
            vec![
                serde_json::json!({"index":1,"role":"user","text":"event question","source":"event"}),
                serde_json::json!({"index":3,"role":"assistant","text":"compacted answer","source":"compacted"}),
            ]
        );
        assert_eq!(
            read_history_label(file.path()).unwrap().as_deref(),
            Some("event question")
        );
    }

    #[test]
    fn projection_filters_internal_prefix_messages() {
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

        let file = history_file(&items);
        let messages = read_history_page(file.path(), 10, None, None)
            .unwrap()
            .messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["text"], "actual user question");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(
            read_history_label(file.path()).unwrap().as_deref(),
            Some("actual user question")
        );
    }

    #[test]
    fn streaming_label_prefers_first_meaningful_user_message() {
        let items = [
            ("assistant", "hello"),
            ("user", "   "),
            ("user", "First valid label message"),
            ("user", "Other"),
        ]
        .into_iter()
        .map(|(role, text)| {
            RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: role.into(),
                content: vec![if role == "user" {
                    ContentItem::InputText { text: text.into() }
                } else {
                    ContentItem::OutputText { text: text.into() }
                }],
                end_turn: None,
                phase: None,
            })
        })
        .collect::<Vec<_>>();
        let file = history_file(&items);
        let label = read_history_label(file.path()).unwrap();
        assert_eq!(label.as_deref(), Some("First valid label message"));
    }
}
