//! Node RPC action handlers  - common actions for connected nodes (devices).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Node action types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NodeAction {
    /// Execute a command on the node (requires approval)
    SystemRun {
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Send an OS notification
    SystemNotify {
        title: String,
        body: String,
        #[serde(default)]
        sound: Option<String>,
    },
    /// Alias for `system.notify`
    Notify {
        title: String,
        body: String,
        #[serde(default)]
        sound: Option<String>,
    },
    /// Capture a screenshot
    SystemScreenshot {
        #[serde(default)]
        display: Option<u32>,
    },
    /// Capture a camera frame
    SystemCamera {
        #[serde(default)]
        device: Option<String>,
    },
    /// Capture a still image from camera
    CameraSnap {
        #[serde(default)]
        device: Option<String>,
    },
    /// Capture a short camera clip
    CameraClip {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    /// Capture a screen recording
    ScreenRecord {
        #[serde(default)]
        display: Option<u32>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    /// Get GPS coordinates
    SystemLocation {},
    /// Alias for `system.location`
    LocationGet {},
    /// Read/write clipboard
    SystemClipboard {
        #[serde(default)]
        write: Option<String>,
    },
}

/// Node action result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeActionResult {
    pub success: bool,
    pub data: Value,
    #[serde(default)]
    pub error: Option<String>,
}

impl NodeAction {
    /// Convert to JSON-RPC params for node.invoke
    #[must_use]
    pub fn to_invoke_params(&self, node_id: &str) -> Value {
        json!({
            "node_id": node_id,
            "method": self.method_name(),
            "params": serde_json::to_value(self).unwrap_or(Value::Null),
        })
    }

    /// Get the RPC method name for this action
    #[must_use]
    pub fn method_name(&self) -> &str {
        match self {
            Self::SystemRun { .. } => "system.run",
            Self::SystemNotify { .. } => "system.notify",
            Self::Notify { .. } => "notify",
            Self::SystemScreenshot { .. } => "system.screenshot",
            Self::SystemCamera { .. } => "system.camera",
            Self::CameraSnap { .. } => "camera.snap",
            Self::CameraClip { .. } => "camera.clip",
            Self::ScreenRecord { .. } => "screen.record",
            Self::SystemLocation { .. } => "system.location",
            Self::LocationGet { .. } => "location.get",
            Self::SystemClipboard { .. } => "system.clipboard",
        }
    }

    /// Check if this action requires approval
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        matches!(
            self,
            Self::SystemRun { .. }
                | Self::SystemNotify { .. }
                | Self::Notify { .. }
                | Self::SystemScreenshot { .. }
                | Self::SystemCamera { .. }
                | Self::CameraSnap { .. }
                | Self::CameraClip { .. }
                | Self::ScreenRecord { .. }
                | Self::SystemLocation { .. }
                | Self::LocationGet { .. }
        )
    }
}
