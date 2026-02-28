//! OpenClaw frame format definitions.
//!
//! OpenClaw uses a JSON frame format with `type` field:
//! - `"event"`  - server-to-client notification
//! - `"req"`    - client-to-server request
//! - `"res"`    - server-to-client response

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An OpenClaw protocol frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenClawFrame {
    /// Frame type: "event", "req", or "res".
    #[serde(rename = "type")]
    pub(crate) frame_type: FrameType,
    /// Event name or method name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Request/response ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<u64>,
    /// Payload data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
    /// Error information (for responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<OpenClawError>,
}

/// Frame type enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FrameType {
    Event,
    Req,
    Res,
}

/// Error in an OpenClaw response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenClawError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

/// An Savfox native WS-RPC frame (JSON-RPC 2.0 style).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavfoxFrame {
    /// Method name (for requests/notifications).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) method: Option<String>,
    /// Parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) params: Option<Value>,
    /// Request ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<Value>,
    /// Result (for responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    /// Error (for responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<Value>,
}

impl OpenClawFrame {
    /// Create an event frame.
    pub(crate) fn event(name: impl Into<String>, data: Value) -> Self {
        Self {
            frame_type: FrameType::Event,
            name: Some(name.into()),
            id: None,
            data: Some(data),
            error: None,
        }
    }

    /// Create a response frame.
    pub(crate) fn response(id: u64, data: Value) -> Self {
        Self {
            frame_type: FrameType::Res,
            name: None,
            id: Some(id),
            data: Some(data),
            error: None,
        }
    }

    /// Create an error response frame.
    pub(crate) fn error_response(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            frame_type: FrameType::Res,
            name: None,
            id: Some(id),
            data: None,
            error: Some(OpenClawError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_parse_openclaw_request() {
        let json = r#"{"type":"req","name":"chat.send","id":1,"data":{"text":"hello"}}"#;
        let frame: OpenClawFrame = serde_json::from_str(json).unwrap();
        assert_eq!(frame.frame_type, FrameType::Req);
        assert_eq!(frame.name.as_deref(), Some("chat.send"));
        assert_eq!(frame.id, Some(1));
    }

    #[test]
    fn test_parse_openclaw_event() {
        let json = r#"{"type":"event","name":"message","data":{"text":"hi"}}"#;
        let frame: OpenClawFrame = serde_json::from_str(json).unwrap();
        assert_eq!(frame.frame_type, FrameType::Event);
        assert_eq!(frame.name.as_deref(), Some("message"));
    }

    #[test]
    fn test_serialize_response() {
        let frame = OpenClawFrame::response(1, json!({"ok": true}));
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"type\":\"res\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_serialize_error_response() {
        let frame = OpenClawFrame::error_response(2, -32600, "Invalid request");
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"code\":-32600"));
        assert!(json.contains("Invalid request"));
    }
}
