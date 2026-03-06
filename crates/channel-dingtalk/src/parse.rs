use savfox_core::channel::ChannelAction;
use serde_json::Value;

pub fn extract_dingtalk_text(body: &Value) -> Option<String> {
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("content"))
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("text"))
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    let content = body.get("content").and_then(Value::as_str)?;
    if let Ok(parsed) = serde_json::from_str::<Value>(content)
        && let Some(text) = parsed.get("text").and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    Some(content.to_string())
}

pub fn extract_dingtalk_channel(body: &Value) -> Option<String> {
    body.get("sessionWebhook")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            body.get("session_webhook")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            body.get("conversationId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            body.get("conversation_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
}

pub fn parse_start_thread_action(body: &Value) -> ChannelAction {
    let text = extract_dingtalk_text(body).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return ChannelAction::Ignore;
    }

    let prompt = text
        .strip_prefix("/savfox ")
        .or_else(|| text.strip_prefix("!savfox "))
        .map(str::trim)
        .unwrap_or_default();
    if prompt.is_empty() {
        return ChannelAction::Ignore;
    }

    let Some(channel) = extract_dingtalk_channel(body) else {
        return ChannelAction::Ignore;
    };

    ChannelAction::StartThread {
        channel,
        prompt: prompt.to_string(),
    }
}
