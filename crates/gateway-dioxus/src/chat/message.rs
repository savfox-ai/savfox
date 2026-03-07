use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => MessageRole::User,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub timestamp: i64,
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: Option<ToolResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub data_base64: String,
}

impl NormalizedMessage {
    pub fn new(id: String, role: MessageRole, content: String) -> Self {
        Self {
            id,
            role,
            content,
            thinking: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            timestamp: chrono::Utc::now().timestamp(),
            attachments: Vec::new(),
        }
    }

    pub fn extract_text(&self) -> String {
        self.content.clone()
    }

    pub fn extract_thinking(&self) -> Option<String> {
        self.thinking.clone()
    }

    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    pub fn is_tool_result(&self) -> bool {
        self.role == MessageRole::Tool || self.tool_call_id.is_some()
    }
}

pub fn normalize_role(role: &str) -> MessageRole {
    let role_lower = role.to_lowercase();
    match role_lower.as_str() {
        "user" | "human" => MessageRole::User,
        "assistant" | "ai" | "bot" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" | "function" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

pub fn extract_thinking_from_content(content: &str) -> (String, Option<String>) {
    let thinking_patterns = [
        ("<thinking>", "</thinking>"),
        ("<think|thinking>", "</think|thinking>"),
        ("<think", "</think"),
    ];

    for (start, end) in thinking_patterns {
        if let Some(start_idx) = content.find(start) {
            if let Some(end_idx) = content.find(end) {
                if end_idx > start_idx {
                    let thinking = content[start_idx + start.len()..end_idx].to_string();
                    let remaining = format!(
                        "{}{}",
                        &content[..start_idx],
                        &content[end_idx + end.len()..]
                    );
                    return (remaining.trim().to_string(), Some(thinking));
                }
            }
        }
    }

    (content.to_string(), None)
}

pub fn strip_thinking_tags(content: &str) -> String {
    let mut result = content.to_string();

    let patterns = [
        ("<think|thinking>", ""),
        ("</think|thinking>", ""),
        ("<thinking>", ""),
        ("</thinking>", ""),
        ("<think", ""),
        ("</think", ""),
    ];

    for (pattern, replacement) in patterns {
        result = result.replace(pattern, replacement);
    }

    result.trim().to_string()
}
