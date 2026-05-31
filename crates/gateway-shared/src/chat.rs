use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    pub timestamp: Option<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub terminal_events: Vec<ChatTerminalEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatTerminalEvent {
    pub event: String,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}
