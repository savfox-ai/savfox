use savfox_core::channel::ChannelAction;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DingtalkMessageMeta {
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub chat_type: Option<String>,
    pub thread_id: Option<String>,
    pub reply_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DingtalkParseResult {
    pub action: ChannelAction,
    pub dedupe_key: Option<String>,
    pub meta: DingtalkMessageMeta,
}

#[must_use]
pub fn parse_text_command(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("/savfox")
        .or_else(|| trimmed.strip_prefix("!savfox"))?;
    if stripped
        .chars()
        .next()
        .is_some_and(|first| !first.is_whitespace())
    {
        return None;
    }
    let prompt = stripped.trim();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt.to_owned())
    }
}

pub fn extract_dingtalk_text(body: &Value) -> Option<String> {
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("content"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("text"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    let content = body.get("content").and_then(Value::as_str)?;
    if let Ok(parsed) = serde_json::from_str::<Value>(content)
        && let Some(text) = parsed.get("text").and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(content)
        && let Some(text) = parsed.get("content").and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    Some(content.to_owned())
}

pub fn extract_dingtalk_channel(body: &Value) -> Option<String> {
    body.get("sessionWebhook")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            body.get("session_webhook")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            body.get("conversationId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            body.get("openConversationId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            body.get("conversation_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
}

fn extract_sender_name(body: &Value) -> Option<String> {
    [
        "senderNick",
        "sender_nick",
        "senderName",
        "sender_name",
        "senderStaffId",
        "sender_staff_id",
    ]
    .iter()
    .find_map(|key| {
        body.get(*key).and_then(Value::as_str).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
    })
}

fn extract_sender_id(body: &Value) -> Option<String> {
    [
        "senderStaffId",
        "sender_staff_id",
        "senderId",
        "sender_id",
        "staffId",
        "staff_id",
        "chatbotUserId",
        "chatbot_user_id",
    ]
    .iter()
    .find_map(|key| {
        body.get(*key).and_then(Value::as_str).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
    })
}

fn extract_chat_type(body: &Value) -> Option<String> {
    let raw = [
        "conversationType",
        "conversation_type",
        "chatType",
        "chat_type",
    ]
    .iter()
    .find_map(|key| body.get(*key))?;

    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            None
        } else {
            Some(match normalized.as_str() {
                "1" | "single" | "private" | "direct" => "dm".to_owned(),
                "2" | "group" | "chat" => "group".to_owned(),
                _ => normalized,
            })
        }
    } else {
        raw.as_i64().map(|number| match number {
            1 => "dm".to_owned(),
            2 => "group".to_owned(),
            _ => number.to_string(),
        })
    }
}

fn extract_thread_id(body: &Value) -> Option<String> {
    [
        "conversationId",
        "conversation_id",
        "openConversationId",
        "open_conversation_id",
    ]
    .iter()
    .find_map(|key| {
        body.get(*key).and_then(Value::as_str).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
    })
}

fn extract_message_id(body: &Value) -> Option<String> {
    ["msgId", "messageId", "msg_id", "message_id"]
        .iter()
        .find_map(|key| {
            body.get(*key).and_then(Value::as_str).and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                }
            })
        })
}

#[must_use]
pub fn parse_inbound_payload(body: &Value) -> DingtalkParseResult {
    let meta = DingtalkMessageMeta {
        sender_id: extract_sender_id(body),
        sender_name: extract_sender_name(body),
        chat_type: extract_chat_type(body),
        thread_id: extract_thread_id(body),
        reply_target: extract_message_id(body),
    };
    let dedupe_key = meta
        .reply_target
        .as_deref()
        .map(|id| format!("dingtalk:{id}"));
    let text = extract_dingtalk_text(body).unwrap_or_default();
    let prompt = parse_text_command(&text);
    let action = match (extract_dingtalk_channel(body), prompt) {
        (Some(channel), Some(prompt)) => ChannelAction::StartThread { channel, prompt },
        _ => ChannelAction::Ignore,
    };

    DingtalkParseResult {
        action,
        dedupe_key,
        meta,
    }
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{parse_inbound_payload, parse_text_command};

    #[test]
    fn parse_text_command_matches_feishu_style() {
        assert_eq!(
            parse_text_command("/savfox summarize"),
            Some("summarize".to_owned())
        );
        assert_eq!(
            parse_text_command("!savfox summarize"),
            Some("summarize".to_owned())
        );
        assert_eq!(parse_text_command("/savfox"), None);
        assert_eq!(parse_text_command("/savfoxtest hi"), None);
    }

    #[test]
    fn parse_inbound_payload_prefers_session_webhook_and_meta() {
        let payload = json!({
            "conversationType": "2",
            "conversationId": "conv-1",
            "sessionWebhook": "https://oapi.dingtalk.com/robot/send?session=abc",
            "senderNick": "Alice",
            "senderStaffId": "staff-1",
            "msgId": "msg-1",
            "text": { "content": "/savfox summarize this" }
        });

        let parsed = parse_inbound_payload(&payload);
        assert_eq!(
            parsed.action,
            ChannelAction::StartThread {
                channel: "https://oapi.dingtalk.com/robot/send?session=abc".to_owned(),
                prompt: "summarize this".to_owned(),
            }
        );
        assert_eq!(parsed.dedupe_key.as_deref(), Some("dingtalk:msg-1"));
        assert_eq!(parsed.meta.sender_name.as_deref(), Some("Alice"));
        assert_eq!(parsed.meta.sender_id.as_deref(), Some("staff-1"));
        assert_eq!(parsed.meta.chat_type.as_deref(), Some("group"));
        assert_eq!(parsed.meta.thread_id.as_deref(), Some("conv-1"));
        assert_eq!(parsed.meta.reply_target.as_deref(), Some("msg-1"));
    }
}
