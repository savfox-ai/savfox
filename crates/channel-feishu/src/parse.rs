use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use savfox_core::channel::ChannelAction;
use serde_json::Value;

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

pub fn extract_bridge_action(message_event: &FeishuMessageEvent) -> Option<ChannelAction> {
    let message = &message_event.message;
    if message.message_type.as_deref() != Some("text") {
        return None;
    }

    let content = message.content.as_deref().unwrap_or("{}");
    let payload: Value = serde_json::from_str(content).ok()?;
    let text = payload.get("text").and_then(Value::as_str)?;
    let prompt = parse_text_command(text)?;
    let chat_id = message.chat_id.as_deref()?.trim();
    if chat_id.is_empty() {
        return None;
    }

    Some(ChannelAction::StartThread {
        channel: chat_id.to_string(),
        prompt,
    })
}
