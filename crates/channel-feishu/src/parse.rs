use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use savfox_core::channel::ChannelAction;
use serde_json::Value;
use tracing::debug;

pub fn parse_text_command(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let stripped = trimmed.strip_prefix("/savfox")?;
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
        Some(prompt.to_string())
    }
}

pub fn extract_channel_action(message_event: &FeishuMessageEvent) -> Option<ChannelAction> {
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
