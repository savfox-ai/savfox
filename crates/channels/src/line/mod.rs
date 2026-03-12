pub mod client;
pub mod config;

pub use config::{LineChannelConfig, resolve_line_channel_secret, resolve_line_outbound_token};
use savfox_core::channel::ChannelAction;
use serde_json::Value;

/// Metadata extracted from a LINE webhook event for thread routing.
#[derive(Debug, Clone, Default)]
pub struct LineStartMeta {
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub room_id: Option<String>,
    pub reply_token: Option<String>,
    pub message_id: Option<String>,
    /// "user", "group", or "room"
    pub source_type: Option<String>,
}

/// Extract thread-routing metadata from the first text-message event.
pub fn parse_start_meta(payload: &Value) -> LineStartMeta {
    let events = payload
        .get("events")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();

    for event in &events {
        if event.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let message = event.get("message").unwrap_or(&Value::Null);
        if message.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }

        let source = event.get("source").unwrap_or(&Value::Null);
        return LineStartMeta {
            user_id: source
                .get("userId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            group_id: source
                .get("groupId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            room_id: source
                .get("roomId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            reply_token: event
                .get("replyToken")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            message_id: message
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            source_type: source
                .get("type")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
        };
    }

    LineStartMeta::default()
}

/// Parse the webhook payload and return a `ChannelAction`.
pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let events = payload
        .get("events")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();

    for event in &events {
        if event.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let message = event.get("message").unwrap_or(&Value::Null);
        if message.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }
        let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");

        // Support /savfox and !savfox command prefixes.
        let prompt = text
            .strip_prefix("/savfox ")
            .or_else(|| text.strip_prefix("!savfox "));

        if let Some(prompt) = prompt {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                let source = event.get("source").unwrap_or(&Value::Null);
                let channel = source
                    .get("userId")
                    .or_else(|| source.get("groupId"))
                    .or_else(|| source.get("roomId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();

                return ChannelAction::StartThread { channel, prompt };
            }
        }
    }

    ChannelAction::Ignore
}
