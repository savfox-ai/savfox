//! Session tracking utilities for webhook and bridge handlers.
//!
//! Provides helper functions to automatically create/update session entries
//! when messages arrive from chat platforms.

use std::sync::Arc;

use tracing::info;

use crate::session::{
    DmScope, SessionEntry, SessionMessageProvenance, SessionSender, SessionStore, build_routing_id,
};

const AUTO_LABEL_MAX_CHARS: usize = 48;

pub struct InboundSessionMeta<'a> {
    pub agent_id: &'a str,
    pub platform: &'a str,
    pub channel_id: &'a str,
    pub routing_group_id: Option<&'a str>,
    pub routing_thread_id: Option<&'a str>,
    pub peer_id: Option<&'a str>,
    pub identity: Option<&'a str>,
    pub group_id: Option<&'a str>,
    pub thread_id: Option<&'a str>,
    pub parent_thread_id: Option<&'a str>,
    pub reply_target: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub topic: Option<&'a str>,
    pub first_message: Option<&'a str>,
    pub chat_type: Option<&'a str>,
    pub dm_scope: DmScope,
}

/// Track an inbound message by creating or updating a session entry.
///
/// Call this from webhook handlers when a message arrives from a chat platform.
pub async fn track_inbound_message(
    session_store: &Arc<SessionStore>,
    meta: InboundSessionMeta<'_>,
) -> SessionEntry {
    let linked_identity = meta
        .identity
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut effective_dm_scope = meta.dm_scope;
    let mut effective_peer_id = meta.peer_id.map(str::to_owned);
    if let Some(identity) = linked_identity.as_ref() {
        effective_dm_scope = DmScope::PerPeer;
        effective_peer_id = Some(identity.clone());
    }

    let routing_id = build_routing_id(
        meta.agent_id,
        Some(&format!("{}:{}", meta.platform, meta.channel_id)),
        meta.routing_group_id.or(meta.group_id),
        meta.routing_thread_id.or(meta.thread_id),
        effective_peer_id.as_deref(),
        meta.account_id,
        effective_dm_scope,
    );

    let mut entry = session_store
        .get_or_create_for_routing_id(&routing_id)
        .await;

    // Update delivery context.
    entry.last_channel = entry.channel.take();
    let source_channel = format!("{}:{}", meta.platform, meta.channel_id);
    entry.channel = Some(source_channel.clone());
    entry.from = Some(meta.platform.to_owned());
    entry.last_to = entry.to.take();
    if let Some(peer_id) = effective_peer_id {
        entry.to = Some(peer_id);
    }

    if let Some(gid) = meta.group_id {
        entry.group_id = Some(gid.to_owned());
    }

    if let Some(tid) = meta.thread_id {
        entry.thread_id = Some(tid.to_owned());
    }

    if let Some(parent_tid) = meta.parent_thread_id {
        entry.parent_thread_id = Some(parent_tid.to_owned());
    }

    if let Some(reply_target) = meta.reply_target {
        entry.reply_target = Some(reply_target.to_owned());
        entry.parent_message_id = Some(reply_target.to_owned());
    }

    if let Some(account_id) = meta.account_id {
        entry.account_id = Some(account_id.to_owned());
    }
    if let Some(identity) = linked_identity {
        entry.identity = Some(identity);
    }

    if let Some(chat_type) = meta.chat_type {
        entry.chat_type = Some(chat_type.to_owned());
    } else if meta.group_id.is_some() {
        entry.chat_type = Some("group".to_owned());
    } else {
        entry.chat_type = Some("dm".to_owned());
    }

    if let Some(user_id) = meta.peer_id {
        let display = meta.name.unwrap_or(user_id);
        entry.sender = Some(SessionSender {
            user_id: Some(user_id.to_owned()),
            name: Some(display.to_owned()),
        });
        entry.push_provenance(SessionMessageProvenance {
            channel: source_channel,
            user_id: user_id.to_owned(),
            name: display.to_owned(),
            timestamp: crate::json_store::now_ms(),
        });
    }

    if let Some(topic) = normalize_label_candidate(meta.topic) {
        entry.subject = Some(topic);
    }

    if entry
        .label
        .as_deref()
        .map(|v| v.trim().is_empty())
        .unwrap_or(true)
    {
        entry.label = derive_auto_label(meta.first_message, entry.subject.as_deref(), meta.name);
    }

    entry.touch();
    session_store.upsert(entry.clone()).await;

    info!(
        session_id = %entry.session_id,
        routing_id = %routing_id,
        platform = %meta.platform,
        "tracked inbound message"
    );

    entry
}

fn derive_auto_label(
    first_message: Option<&str>,
    topic: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    normalize_label_candidate(first_message)
        .or_else(|| normalize_label_candidate(topic))
        .or_else(|| normalize_label_candidate(name))
}

fn normalize_label_candidate(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().count() <= AUTO_LABEL_MAX_CHARS {
        return Some(compact.to_owned());
    }
    let truncated: String = compact.chars().take(AUTO_LABEL_MAX_CHARS).collect();
    Some(format!("{truncated}..."))
}

/// Track token usage for a session after an agent invocation.
pub async fn track_token_usage(
    session_store: &Arc<SessionStore>,
    session_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) {
    session_store
        .update(session_id, |entry| {
            entry.add_tokens(input_tokens, output_tokens);
        })
        .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::{InboundSessionMeta, derive_auto_label, track_inbound_message};
    use crate::session::{DmScope, SessionStore};

    #[test]
    fn auto_label_prefers_first_message() {
        let label = derive_auto_label(
            Some("  Build  deployment  checklist for release  "),
            Some("Ops"),
            Some("Alice"),
        );
        assert_eq!(
            label.as_deref(),
            Some("Build deployment checklist for release")
        );
    }

    #[test]
    fn auto_label_falls_back_to_topic_then_name() {
        let from_topic = derive_auto_label(None, Some("Q1 Planning"), Some("Alice"));
        assert_eq!(from_topic.as_deref(), Some("Q1 Planning"));

        let from_name = derive_auto_label(None, None, Some("Alice"));
        assert_eq!(from_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn auto_label_truncates_long_message() {
        let label = derive_auto_label(
            Some("This is a very long first message that should be truncated into a concise preview label"),
            None,
            None,
        )
        .unwrap();
        assert!(label.ends_with("..."));
        assert!(label.chars().count() <= 51);
    }

    #[tokio::test]
    async fn routing_group_id_scopes_sessions_by_chat() {
        let home = tempdir().expect("tempdir");
        let store = Arc::new(SessionStore::from_home(home.path()));

        let first = track_inbound_message(
            &store,
            InboundSessionMeta {
                agent_id: "default",
                platform: "feishu",
                channel_id: "oc_group_1",
                routing_group_id: Some("oc_group_1"),
                routing_thread_id: None,
                peer_id: Some("user-a"),
                identity: None,
                group_id: None,
                thread_id: None,
                parent_thread_id: None,
                reply_target: None,
                account_id: None,
                name: Some("Alice"),
                topic: None,
                first_message: Some("hello"),
                chat_type: None,
                dm_scope: DmScope::Main,
            },
        )
        .await;

        let second = track_inbound_message(
            &store,
            InboundSessionMeta {
                agent_id: "default",
                platform: "feishu",
                channel_id: "oc_group_2",
                routing_group_id: Some("oc_group_2"),
                routing_thread_id: None,
                peer_id: Some("user-a"),
                identity: None,
                group_id: None,
                thread_id: None,
                parent_thread_id: None,
                reply_target: None,
                account_id: None,
                name: Some("Alice"),
                topic: None,
                first_message: Some("hello"),
                chat_type: None,
                dm_scope: DmScope::Main,
            },
        )
        .await;

        assert_ne!(first.session_id, second.session_id);
    }
}
