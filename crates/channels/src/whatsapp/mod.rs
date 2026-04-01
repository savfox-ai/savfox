pub mod client;
pub mod config;

pub use config::{
    WhatsAppSavedConfig, resolve_whatsapp_app_secret, resolve_whatsapp_outbound_config,
    resolve_whatsapp_verify_token,
};
use savfox_core::channel::ChannelAction;
use serde_json::Value;

/// Metadata extracted from a WhatsApp webhook event for thread routing.
#[derive(Debug, Clone, Default)]
pub struct WhatsAppStartMeta {
    /// Sender phone number (the "from" field).
    pub from: Option<String>,
    /// Display name from the contacts array, if available.
    pub display_name: Option<String>,
    /// Message ID (wamid).
    pub message_id: Option<String>,
    /// Timestamp of the message.
    pub timestamp: Option<String>,
}

/// Navigate into the first message entry of a WhatsApp Cloud API webhook payload.
fn first_message(payload: &Value) -> Option<&Value> {
    payload
        .get("entry")?
        .as_array()?
        .first()?
        .get("changes")?
        .as_array()?
        .first()?
        .get("value")?
        .get("messages")?
        .as_array()?
        .first()
}

/// Navigate to the `value` object containing messages and contacts.
fn first_value(payload: &Value) -> Option<&Value> {
    payload
        .get("entry")?
        .as_array()?
        .first()?
        .get("changes")?
        .as_array()?
        .first()?
        .get("value")
}

/// Extract thread-routing metadata from the first message.
pub fn parse_start_meta(payload: &Value) -> WhatsAppStartMeta {
    let Some(message) = first_message(payload) else {
        return WhatsAppStartMeta::default();
    };

    let from = message
        .get("from")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    // Try to resolve display name from contacts array.
    let display_name = first_value(payload)
        .and_then(|v| v.get("contacts"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|contact| contact.get("profile"))
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    WhatsAppStartMeta {
        from,
        display_name,
        message_id: message
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        timestamp: message
            .get("timestamp")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
    }
}

/// Parse the webhook payload and return a `ChannelAction`.
pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let Some(value) = first_value(payload) else {
        return ChannelAction::Ignore;
    };

    let messages = value.get("messages").and_then(|m| m.as_array());
    if let Some(messages) = messages {
        for message in messages {
            let from = message
                .get("from")
                .and_then(|f| f.as_str())
                .unwrap_or_default();

            let text = message
                .get("text")
                .and_then(|t| t.get("body"))
                .and_then(|b| b.as_str())
                .unwrap_or("");

            if !text.is_empty() {
                return ChannelAction::StartThread {
                    channel: from.to_string(),
                    prompt: text.to_owned(),
                };
            }
        }
    }

    ChannelAction::Ignore
}
