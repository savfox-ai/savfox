use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCard {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: Option<ToolResult>,
    pub status: ToolStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub truncated: bool,
}

impl ToolCard {
    pub fn new(call_id: String, name: String, arguments: serde_json::Value) -> Self {
        Self {
            call_id,
            name,
            arguments,
            result: None,
            status: ToolStatus::Pending,
        }
    }

    pub fn with_result(mut self, content: String, is_error: bool) -> Self {
        self.result = Some(ToolResult::new(content, is_error));
        self.status = if is_error {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        self
    }

    pub fn is_pending(&self) -> bool {
        self.status == ToolStatus::Pending
    }

    pub fn is_running(&self) -> bool {
        self.status == ToolStatus::Running
    }

    pub fn is_completed(&self) -> bool {
        self.status == ToolStatus::Completed
    }

    pub fn is_failed(&self) -> bool {
        self.status == ToolStatus::Failed
    }

    pub fn display_name(&self) -> &str {
        &self.name
    }

    pub fn argument_preview(&self, max_chars: usize) -> String {
        let args_str = serde_json::to_string(&self.arguments).unwrap_or_default();
        if args_str.len() <= max_chars {
            args_str
        } else {
            format!("{}...", &args_str[..max_chars])
        }
    }

    pub fn result_preview(&self, max_chars: usize) -> Option<String> {
        self.result.as_ref().map(|r| {
            if r.content.len() <= max_chars {
                r.content.clone()
            } else {
                format!("{}...", &r.content[..max_chars])
            }
        })
    }
}

impl ToolResult {
    pub fn new(content: String, is_error: bool) -> Self {
        let truncated = content.len() > 10000;
        Self {
            content: if truncated {
                content[..10000].to_string()
            } else {
                content
            },
            is_error,
            truncated,
        }
    }
}

pub fn extract_tool_cards(content: &str) -> Vec<ToolCard> {
    let mut cards = Vec::new();

    // Look for tool call patterns like: 🔧 tool_name(args)
    // or structured JSON tool calls
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(tool_calls) = json.get("tool_calls").and_then(|t| t.as_array()) {
            for call in tool_calls {
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(|i| i.as_str()),
                    call.get("name").and_then(|n| n.as_str()),
                ) {
                    let args = call
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));
                    cards.push(ToolCard::new(id.to_string(), name.to_string(), args));
                }
            }
        }
    }

    cards
}

pub fn format_tool_output_for_display(output: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = output.lines().take(max_lines).collect();
    let mut result = lines.join("\n");

    if result.len() > max_chars {
        result = format!("{}...", &result[..max_chars]);
    }

    if output.lines().count() > max_lines {
        result = format!(
            "{}\n... ({} more lines)",
            result,
            output.lines().count() - max_lines
        );
    }

    result
}

pub fn get_tool_icon(tool_name: &str) -> &'static str {
    match tool_name {
        "read_file" | "write_file" | "edit_file" | "create_file" | "delete_file" => {
            crate::utils::icons::ICON_FILE_CODE
        }
        "run_command" | "execute" | "shell" => crate::utils::icons::ICON_PLAY,
        "search" | "grep" | "find" => crate::utils::icons::ICON_SEARCH,
        "web_search" | "fetch" | "http" => crate::utils::icons::ICON_GLOBE,
        "code" | "evaluate" | "python" | "javascript" => crate::utils::icons::ICON_FILE_CODE,
        "image" | "screenshot" | "render" => crate::utils::icons::ICON_IMAGE,
        _ => crate::utils::icons::ICON_WRENCH,
    }
}
