#![allow(dead_code)]
use savfox_gateway_shared::is_internal_session_message;
use serde_json::{Value, json};

use crate::api::types::{
    AgentEntry, ArkretDeliveryPreview, ChatAttachment, ChatMessage, ChatTerminalEvent,
    SessionAmbientMessage, SessionEntry, SessionIdleReplyResponse,
};
use crate::api::ws::WsRpc;
const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];
const REASONING_MODES: [&str; 3] = ["off", "on", "stream"];
const VERBOSE_MODES: [&str; 3] = ["off", "on", "full"];
const PENDING_MODEL_STORAGE_KEY: &str = "savfox_pending_session_model";

async fn preview_and_publish_arkret(
    ws: WsRpc,
    session_id: String,
    kind: &'static str,
    title: &'static str,
    summary: String,
) -> Result<String, String> {
    let params = json!({
        "session_id": session_id,
        "kind": kind,
        "title": title,
        "summary": summary,
        "verification": [],
        "next_actions": [],
    });
    let preview = ws
        .call::<ArkretDeliveryPreview>("sessions.arkret.delivery.preview", Some(params.clone()))
        .await?;
    let comparison = preview
        .previous_rendered
        .as_deref()
        .map(|previous| {
            format!(
                "Last public checkpoint:\n{previous}\n\nNew publish candidate:\n{}",
                preview.rendered
            )
        })
        .unwrap_or_else(|| format!("First public checkpoint:\n{}", preview.rendered));
    let confirmation = format!(
        "Publish through Arkret Agent {}?\n\n{}",
        preview.sender_did, comparison
    );
    let confirmed = web_sys::window()
        .and_then(|window| window.confirm_with_message(&confirmation).ok())
        .unwrap_or(false);
    if !confirmed {
        return Ok(format!("## Publish preview\n\n{}", preview.rendered));
    }
    let published = ws
        .call::<Value>("sessions.arkret.delivery.publish", Some(params))
        .await?;
    let event_id = published
        .get("remoteEventId")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(format!(
        "## Published to Arkret\n\nSender: `{}`\n\nEvent: `{event_id}`\n\n{}",
        preview.sender_did, preview.rendered
    ))
}

pub(super) fn request_arkret_publish_candidate(kind: &str) -> Option<String> {
    let message = format!(
        "Write the minimal public {kind} summary for Arkret. Local chat, tool logs, and private paths are not copied automatically."
    );
    web_sys::window()
        .and_then(|window| window.prompt_with_message(&message).ok().flatten())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(super) fn normalize_reasoning_mode(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if REASONING_MODES.iter().any(|mode| *mode == normalized) {
        normalized
    } else {
        "on".to_string()
    }
}

pub(super) fn normalize_verbose_mode(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if VERBOSE_MODES.iter().any(|mode| *mode == normalized) {
        normalized
    } else {
        "on".to_string()
    }
}

pub(super) fn sort_sessions(mut entries: Vec<SessionEntry>) -> Vec<SessionEntry> {
    entries.sort_by(|a, b| {
        let a_activity = a.last_activity.as_deref().unwrap_or("");
        let b_activity = b.last_activity.as_deref().unwrap_or("");
        b_activity.cmp(a_activity)
    });
    entries
}

pub(super) fn should_show_thinking_for_message(
    mode: &str,
    is_streaming: bool,
    is_last: bool,
) -> bool {
    match mode {
        "off" => false,
        "on" => !(is_streaming && is_last),
        "stream" => true,
        _ => true,
    }
}

pub(super) fn find_agent_entry<'a>(
    agents: &'a [AgentEntry],
    selected: &str,
) -> Option<&'a AgentEntry> {
    agents
        .iter()
        .find(|entry| entry.id.as_deref() == Some(selected) || entry.name == selected)
}

pub(super) fn primary_model_for_agent(entry: &AgentEntry) -> Option<String> {
    entry
        .native
        .as_ref()
        .map(|native| native.model.clone())
        .filter(|model| !model.trim().is_empty())
}

pub(super) fn parse_history_messages(payload: &Value) -> Vec<ChatMessage> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(parse_history_message)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn parse_history_message(item: &Value) -> Option<ChatMessage> {
    let role = item
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant")
        .to_string();
    let content = item
        .get("text")
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() || is_internal_session_message(&content) {
        return None;
    }

    Some(ChatMessage {
        role,
        content,
        attachments: vec![],
        timestamp: None,
        thinking: None,
        terminal_events: vec![],
    })
}

pub(super) fn agent_has_terminal_delegate(agents: &[AgentEntry], selected: &str) -> bool {
    find_agent_entry(agents, selected)
        .filter(|entry| entry.kind.as_str() == "terminal")
        .and_then(|entry| entry.terminal.as_ref())
        .and_then(|terminal| terminal.delegate.enabled)
        .unwrap_or(false)
}

pub(super) fn agent_has_managed_terminal(agents: &[AgentEntry], selected: &str) -> bool {
    find_agent_entry(agents, selected)
        .filter(|entry| entry.kind.as_str() == "terminal")
        .and_then(|entry| entry.terminal.as_ref())
        .and_then(|terminal| terminal.delegate.mode.as_ref())
        .is_some_and(|mode| mode.as_str() == "managed_pty")
}

pub(super) fn agent_has_claude_stream(agents: &[AgentEntry], selected: &str) -> bool {
    find_agent_entry(agents, selected)
        .and_then(|entry| entry.terminal.as_ref())
        .and_then(|terminal| terminal.delegate.mode.as_ref())
        .is_some_and(|mode| mode.as_str() == "claude_stream")
}

pub(super) fn parse_terminal_events(value: Option<&Value>) -> Vec<ChatTerminalEvent> {
    value
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| serde_json::from_value::<ChatTerminalEvent>(event.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn format_ambient_sidebar(messages: &[SessionAmbientMessage]) -> String {
    if messages.is_empty() {
        return "## Ambient Context\n\nNo buffered ambient messages for this session.".to_string();
    }

    let mut out = String::from("## Ambient Context\n\n");
    for message in messages {
        let speaker = message
            .sender_name
            .as_deref()
            .or(message.sender_id.as_deref())
            .unwrap_or("unknown");
        out.push_str(&format!(
            "- **{speaker}** (`{}` / `{}`)\n\n  {}\n\n",
            message.sender_kind,
            message.reason,
            message.text.trim()
        ));
    }
    out
}

pub(super) fn format_idle_reply_sidebar(status: &SessionIdleReplyResponse) -> String {
    let mut out = String::from("## Idle Fallback\n\n");
    match status.pending.as_ref() {
        Some(pending) => {
            out.push_str(&format!(
                "- **Pending** for `{}`\n- Agent: `{}`\n- Delay: `{}` seconds\n- Preview: {}\n\n",
                pending.outbound_channel,
                pending.agent_id,
                pending.delay_secs,
                pending.message_preview.trim()
            ));
        }
        None => {
            out.push_str("No pending idle fallback reply for this session.\n\n");
        }
    }

    out.push_str(&format!(
        "- Recent replies in last hour: `{}`\n",
        status.recent_sent_count
    ));
    if !status.recent_sent_at_ms.is_empty() {
        out.push_str("- Recent trigger timestamps (ms):\n");
        for timestamp in &status.recent_sent_at_ms {
            out.push_str(&format!("  - `{timestamp}`\n"));
        }
    }
    if let Some(reason) = status.suppressed_reason.as_deref() {
        out.push_str(&format!("\n- Last suppression reason: `{reason}`\n"));
    }
    if let Some(timestamp) = status.last_suppressed_at_ms {
        out.push_str(&format!("- Last suppression time (ms): `{timestamp}`\n"));
    }
    out
}

pub(super) fn load_pending_session_model() -> Option<String> {
    web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
        .and_then(|storage| storage.get_item(PENDING_MODEL_STORAGE_KEY).ok())
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn save_pending_session_model(value: Option<&str>) {
    let Some(storage) = web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
    else {
        return;
    };

    if let Some(model) = value {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            let _ = storage.remove_item(PENDING_MODEL_STORAGE_KEY);
        } else {
            let _ = storage.set_item(PENDING_MODEL_STORAGE_KEY, trimmed);
        }
    } else {
        let _ = storage.remove_item(PENDING_MODEL_STORAGE_KEY);
    }
}

pub(super) fn build_openai_chat_message_payload(
    prompt: &str,
    attachments: &[ChatAttachment],
) -> Value {
    if attachments.is_empty() {
        return json!({
            "role": "user",
            "content": prompt,
        });
    }

    let mut parts = Vec::new();
    if !prompt.is_empty() {
        parts.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }

    for attachment in attachments {
        if !attachment
            .mime_type
            .to_ascii_lowercase()
            .starts_with("image/")
        {
            continue;
        }
        let data_url = format!(
            "data:{};base64,{}",
            attachment.mime_type, attachment.data_base64
        );
        parts.push(json!({
            "type": "image_url",
            "image_url": { "url": data_url },
        }));
    }

    if parts.is_empty() {
        json!({
            "role": "user",
            "content": prompt,
        })
    } else {
        json!({
            "role": "user",
            "content": parts,
        })
    }
}

async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_history_messages, parse_terminal_events};

    #[test]
    fn parse_terminal_events_keeps_known_terminal_fields_and_skips_invalid_rows() {
        let events = parse_terminal_events(Some(&json!([
            {
                "event": "log",
                "stream": "stderr",
                "text": "warning"
            },
            {
                "event": "completed",
                "exit_code": 0
            },
            "invalid"
        ])));

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "log");
        assert_eq!(events[0].stream.as_deref(), Some("stderr"));
        assert_eq!(events[0].text.as_deref(), Some("warning"));
        assert_eq!(events[1].event, "completed");
        assert_eq!(events[1].exit_code, Some(0));
    }

    #[test]
    fn parse_history_messages_initializes_terminal_event_storage() {
        let messages = parse_history_messages(&json!({
            "messages": [
                {"role": "assistant", "text": "done"}
            ]
        }));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].content, "done");
        assert!(messages[0].terminal_events.is_empty());
    }
}
