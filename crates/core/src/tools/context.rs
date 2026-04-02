use std::borrow::Cow;
use std::sync::Arc;

use savfox_protocol::mcp::CallToolResult;
use savfox_protocol::models::{
    FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseInputItem,
    ShellToolCallParams,
};
use savfox_utils::string::take_bytes_at_char_boundary;
use tokio::sync::Mutex;

use crate::savfox::{Session, TurnContext};
use crate::tools::{
    TELEMETRY_PREVIEW_MAX_BYTES, TELEMETRY_PREVIEW_MAX_LINES, TELEMETRY_PREVIEW_TRUNCATION_NOTICE,
};
use crate::turn_diff_tracker::TurnDiffTracker;

pub type SharedTurnDiffTracker = Arc<Mutex<TurnDiffTracker>>;

#[derive(Clone)]
pub struct ToolInvocation {
    pub session: Arc<Session>,
    pub turn: Arc<TurnContext>,
    pub tracker: SharedTurnDiffTracker,
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolPayload,
}

#[derive(Clone, Debug)]
pub enum ToolPayload {
    Function {
        arguments: String,
    },
    Custom {
        input: String,
    },
    LocalShell {
        params: ShellToolCallParams,
    },
    Mcp {
        server: String,
        tool: String,
        raw_arguments: String,
    },
}

impl ToolPayload {
    pub fn log_payload(&self) -> Cow<'_, str> {
        match self {
            Self::Function { arguments } => Cow::Borrowed(arguments),
            Self::Custom { input } => Cow::Borrowed(input),
            Self::LocalShell { params } => Cow::Owned(params.command.join(" ")),
            Self::Mcp { raw_arguments, .. } => Cow::Borrowed(raw_arguments),
        }
    }
}

#[derive(Clone)]
pub enum ToolOutput {
    Function {
        // Plain text representation of the tool output.
        content: String,
        // Some tool calls such as MCP calls may return structured content that can get parsed into
        // an array of polymorphic content items.
        content_items: Option<Vec<FunctionCallOutputContentItem>>,
        success: Option<bool>,
    },
    Mcp {
        result: Result<CallToolResult, String>,
    },
}

impl ToolOutput {
    /// Create a successful `Function` output with the given content.
    pub fn ok(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            content_items: None,
            success: Some(true),
        }
    }

    /// Create a failed `Function` output with the given content.
    pub fn fail(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            content_items: None,
            success: Some(false),
        }
    }

    pub fn log_preview(&self) -> String {
        match self {
            Self::Function { content, .. } => telemetry_preview(content),
            Self::Mcp { result } => format!("{result:?}"),
        }
    }

    pub fn success_for_logging(&self) -> bool {
        match self {
            Self::Function { success, .. } => success.unwrap_or(true),
            Self::Mcp { result } => result.is_ok(),
        }
    }

    pub fn into_response(self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        match self {
            Self::Function {
                content,
                content_items,
                success,
            } => {
                if matches!(payload, ToolPayload::Custom { .. }) {
                    ResponseInputItem::CustomToolCallOutput {
                        call_id: call_id.to_owned(),
                        output: content,
                    }
                } else {
                    ResponseInputItem::FunctionCallOutput {
                        call_id: call_id.to_owned(),
                        output: FunctionCallOutputPayload {
                            content,
                            content_items,
                            success,
                        },
                    }
                }
            }
            Self::Mcp { result } => ResponseInputItem::McpToolCallOutput {
                call_id: call_id.to_owned(),
                result,
            },
        }
    }
}

fn telemetry_preview(content: &str) -> String {
    let truncated_slice = take_bytes_at_char_boundary(content, TELEMETRY_PREVIEW_MAX_BYTES);
    let truncated_by_bytes = truncated_slice.len() < content.len();

    let mut preview = String::new();
    let mut lines_iter = truncated_slice.lines();
    for idx in 0..TELEMETRY_PREVIEW_MAX_LINES {
        match lines_iter.next() {
            Some(line) => {
                if idx > 0 {
                    preview.push('\n');
                }
                preview.push_str(line);
            }
            None => break,
        }
    }
    let truncated_by_lines = lines_iter.next().is_some();

    if !truncated_by_bytes && !truncated_by_lines {
        return content.to_owned();
    }

    if preview.len() < truncated_slice.len()
        && truncated_slice
            .as_bytes()
            .get(preview.len())
            .is_some_and(|byte| *byte == b'\n')
    {
        preview.push('\n');
    }

    if !preview.is_empty() && !preview.ends_with('\n') {
        preview.push('\n');
    }
    preview.push_str(TELEMETRY_PREVIEW_TRUNCATION_NOTICE);

    preview
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn custom_tool_calls_should_roundtrip_as_custom_outputs() {
        let payload = ToolPayload::Custom {
            input: "patch".to_string(),
        };
        let response = ToolOutput::Function {
            content: "patched".to_string(),
            content_items: None,
            success: Some(true),
        }
        .into_response("call-42", &payload);

        match response {
            ResponseInputItem::CustomToolCallOutput { call_id, output } => {
                assert_eq!(call_id, "call-42");
                assert_eq!(output, "patched");
            }
            other => panic!("expected CustomToolCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn function_payloads_remain_function_outputs() {
        let payload = ToolPayload::Function {
            arguments: "{}".to_string(),
        };
        let response = ToolOutput::Function {
            content: "ok".to_string(),
            content_items: None,
            success: Some(true),
        }
        .into_response("fn-1", &payload);

        match response {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "fn-1");
                assert_eq!(output.content, "ok");
                assert!(output.content_items.is_none());
                assert_eq!(output.success, Some(true));
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_preview_returns_original_within_limits() {
        let content = "short output";
        assert_eq!(telemetry_preview(content), content);
    }

    #[test]
    fn telemetry_preview_truncates_by_bytes() {
        let content = "x".repeat(TELEMETRY_PREVIEW_MAX_BYTES + 8);
        let preview = telemetry_preview(&content);

        assert!(preview.contains(TELEMETRY_PREVIEW_TRUNCATION_NOTICE));
        assert!(
            preview.len()
                <= TELEMETRY_PREVIEW_MAX_BYTES + TELEMETRY_PREVIEW_TRUNCATION_NOTICE.len() + 1
        );
    }

    #[test]
    fn telemetry_preview_truncates_by_lines() {
        let content = (0..(TELEMETRY_PREVIEW_MAX_LINES + 5))
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let preview = telemetry_preview(&content);
        let lines: Vec<&str> = preview.lines().collect();

        assert!(lines.len() <= TELEMETRY_PREVIEW_MAX_LINES + 1);
        assert_eq!(lines.last(), Some(&TELEMETRY_PREVIEW_TRUNCATION_NOTICE));
    }
}
