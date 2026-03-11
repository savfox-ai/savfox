use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use savfox_core::channel::ChannelAction;
use serde_json::Value;
use tracing::debug;

pub fn parse_text_command(text: &str) -> Option<String> {
    let trimmed = text.trim();
    for prefix in ["/savfox", "!savfox"] {
        if let Some(stripped) = trimmed.strip_prefix(prefix) {
            if stripped
                .chars()
                .next()
                .is_some_and(|first| !first.is_whitespace())
            {
                return None;
            }
            let prompt = stripped.trim();
            return if prompt.is_empty() {
                None
            } else {
                Some(prompt.to_string())
            };
        }
    }

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn extract_channel_action(message_event: &FeishuMessageEvent) -> Option<ChannelAction> {
    let sender_type = message_event
        .sender
        .sender_type
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    if !matches!(sender_type.as_deref(), None | Some("user")) {
        debug!(sender_type = ?sender_type, "Ignoring non-user Feishu message");
        return None;
    }

    let message = &message_event.message;
    debug!(message_type = ?message.message_type, "Checking Feishu message type");
    if message.message_type.as_deref() != Some("text") {
        debug!("Not a text message, skipping");
        return None;
    }

    let content = message.content.as_deref().unwrap_or("{}");
    debug!(content = %content, "Raw Feishu message content");
    let payload: Value = serde_json::from_str(content).ok()?;
    let text = payload.get("text").and_then(Value::as_str)?;
    debug!(text = %text, "Extracted text from Feishu message");
    let prompt = parse_text_command(text)?;
    let chat_id = message.chat_id.as_deref()?.trim();
    if chat_id.is_empty() {
        debug!("Empty chat ID, skipping");
        return None;
    }
    debug!(chat_id = %chat_id, prompt = %prompt, "Extracted valid channel action");

    Some(ChannelAction::StartThread {
        channel: chat_id.to_string(),
        prompt,
    })
}

#[cfg(test)]
mod tests {
    use feishu_sdk::event::models::{Message, MessageEvent, MessageId, MessageSender};
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{extract_channel_action, parse_text_command};

    #[test]
    fn parse_text_command_supports_commands_and_plain_text() {
        assert_eq!(
            parse_text_command("/savfox summarize"),
            Some("summarize".to_string())
        );
        assert_eq!(
            parse_text_command("!savfox summarize"),
            Some("summarize".to_string())
        );
        assert_eq!(
            parse_text_command("summarize this"),
            Some("summarize this".to_string())
        );
        assert_eq!(parse_text_command("/savfox"), None);
        assert_eq!(parse_text_command("/savfoxtest hi"), None);
        assert_eq!(parse_text_command("   "), None);
    }

    #[test]
    fn extract_channel_action_ignores_non_user_messages() {
        let event = MessageEvent {
            sender: MessageSender {
                sender_id: Some(MessageId {
                    open_id: Some("ou_bot".to_string()),
                    user_id: None,
                    union_id: None,
                }),
                sender_type: Some("app".to_string()),
                tenant_key: None,
            },
            message: Message {
                message_id: Some("om_1".to_string()),
                root_id: None,
                parent_id: None,
                create_time: None,
                chat_id: Some("oc_1".to_string()),
                message_type: Some("text".to_string()),
                content: Some(json!({ "text": "hello" }).to_string()),
                mentions: None,
            },
        };

        assert!(extract_channel_action(&event).is_none());
    }

    #[test]
    fn extract_channel_action_accepts_plain_text_messages() {
        let event = MessageEvent {
            sender: MessageSender {
                sender_id: Some(MessageId {
                    open_id: Some("ou_user".to_string()),
                    user_id: None,
                    union_id: None,
                }),
                sender_type: Some("user".to_string()),
                tenant_key: None,
            },
            message: Message {
                message_id: Some("om_1".to_string()),
                root_id: None,
                parent_id: None,
                create_time: None,
                chat_id: Some("oc_1".to_string()),
                message_type: Some("text".to_string()),
                content: Some(json!({ "text": "hello world" }).to_string()),
                mentions: None,
            },
        };

        assert_eq!(
            extract_channel_action(&event),
            Some(ChannelAction::StartThread {
                channel: "oc_1".to_string(),
                prompt: "hello world".to_string(),
            })
        );
    }
}
