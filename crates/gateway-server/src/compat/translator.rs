//! Translates between OpenClaw and Savfox frame formats.

use serde_json::{Value, json};
use tracing::debug;

use super::frame::{FrameType, OpenClawFrame, SavfoxFrame};

/// Method name mapping from OpenClaw to Savfox.
///
/// OpenClaw uses dot-separated method names that may differ from Savfox's.
/// This translates known methods; unknown methods pass through unchanged.
pub(crate) fn map_method_name(openclaw_method: &str) -> &str {
    match openclaw_method {
        // Chat
        "chat.send" => "chat.send",
        "chat.send-rich" => "send",
        // Sessions
        "sessions.list" => "sessions.list",
        "sessions.get" => "sessions.get",
        "sessions.create" => "sessions.create",
        "sessions.delete" => "sessions.delete",
        "sessions.reset" => "sessions.reset",
        "sessions.compact" => "sessions.compact",
        // Models
        "models.list" => "models.list",
        "models.set" => "models.set",
        // Agent
        "agent.turn" => "agent.turn",
        "agent.stop" => "agent.stop",
        "agent.status" => "agent.status",
        // Channels
        "channels.list" => "channels.list",
        "channels.enable" => "channels.enable",
        "channels.disable" => "channels.disable",
        // Cron
        "cron.list" => "cron.list",
        "cron.create" => "cron.create",
        "cron.delete" => "cron.delete",
        "cron.run" => "cron.run",
        // Memory
        "memory.list" => "memory.list",
        "memory.get" => "memory.get",
        "memory.create" => "memory.create",
        "memory.update" => "memory.update",
        "memory.delete" => "memory.delete",
        "memory.search" => "memory.search",
        // Devices
        "devices.list" => "devices.list",
        "devices.pair" => "devices.pair.request",
        "devices.unpair" => "devices.revoke",
        // Connect
        "connect" => "auth",
        "connect.challenge" => "auth.challenge",
        // Pass through
        _ => openclaw_method,
    }
}

/// Translate an OpenClaw frame to Savfox format.
pub(crate) fn openclaw_to_savfox(frame: &OpenClawFrame) -> Option<SavfoxFrame> {
    match frame.frame_type {
        FrameType::Req => {
            let method = frame.name.as_deref()?;
            let savfox_method = map_method_name(method);

            debug!(
                openclaw_method = method,
                savfox_method, "Translating OpenClaw request"
            );

            Some(SavfoxFrame {
                method: Some(savfox_method.to_string()),
                params: frame.data.clone(),
                id: frame.id.map(|id| json!(id)),
                result: None,
                error: None,
            })
        }
        FrameType::Event => {
            // Client-sent events are rare; treat as notifications
            let method = frame.name.as_deref()?;
            Some(SavfoxFrame {
                method: Some(map_method_name(method).to_string()),
                params: frame.data.clone(),
                id: None,
                result: None,
                error: None,
            })
        }
        FrameType::Res => {
            // Clients don't send responses; ignore
            None
        }
    }
}

/// Translate an Savfox frame to OpenClaw format.
pub(crate) fn savfox_to_openclaw(frame: &SavfoxFrame) -> OpenClawFrame {
    if let Some(method) = &frame.method {
        // This is an event/notification from server
        OpenClawFrame::event(method.clone(), frame.params.clone().unwrap_or(Value::Null))
    } else if let Some(result) = &frame.result {
        // This is a response
        let id = frame.id.as_ref().and_then(|v| v.as_u64()).unwrap_or(0);
        OpenClawFrame::response(id, result.clone())
    } else if let Some(error) = &frame.error {
        // Error response
        let id = frame.id.as_ref().and_then(|v| v.as_u64()).unwrap_or(0);
        let (code, message) = if let Some(obj) = error.as_object() {
            (
                obj.get("code").and_then(|c| c.as_i64()).unwrap_or(-1) as i32,
                obj.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string(),
            )
        } else {
            (-1, error.to_string())
        };
        OpenClawFrame::error_response(id, code, message)
    } else {
        // Fallback
        OpenClawFrame::event("unknown", Value::Null)
    }
}

/// Detect whether a raw JSON message is in OpenClaw format.
pub(crate) fn is_openclaw_frame(raw: &Value) -> bool {
    raw.get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|t| matches!(t, "event" | "req" | "res"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_translate_request() {
        let frame = OpenClawFrame {
            frame_type: FrameType::Req,
            name: Some("chat.send".to_string()),
            id: Some(1),
            data: Some(json!({"text": "hello"})),
            error: None,
        };

        let savfox = openclaw_to_savfox(&frame).unwrap();
        assert_eq!(savfox.method.as_deref(), Some("chat.send"));
        assert_eq!(savfox.id, Some(json!(1)));
    }

    #[test]
    fn test_translate_connect_to_auth() {
        let frame = OpenClawFrame {
            frame_type: FrameType::Req,
            name: Some("connect".to_string()),
            id: Some(1),
            data: Some(json!({"token": "abc"})),
            error: None,
        };

        let savfox = openclaw_to_savfox(&frame).unwrap();
        assert_eq!(savfox.method.as_deref(), Some("auth"));
    }

    #[test]
    fn test_translate_response_to_openclaw() {
        let savfox = SavfoxFrame {
            method: None,
            params: None,
            id: Some(json!(1)),
            result: Some(json!({"ok": true})),
            error: None,
        };

        let openclaw = savfox_to_openclaw(&savfox);
        assert_eq!(openclaw.frame_type, FrameType::Res);
        assert_eq!(openclaw.id, Some(1));
    }

    #[test]
    fn test_detect_openclaw_frame() {
        assert!(is_openclaw_frame(&json!({"type": "req", "name": "test"})));
        assert!(is_openclaw_frame(&json!({"type": "event", "name": "test"})));
        assert!(!is_openclaw_frame(&json!({"method": "test", "id": 1})));
    }

    #[test]
    fn test_unknown_method_passthrough() {
        assert_eq!(map_method_name("custom.method"), "custom.method");
    }
}
