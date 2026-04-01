use std::sync::Arc;

use async_trait::async_trait;

use crate::function_tool::{FunctionCallError, model_err};
use crate::mcp_tool_call::handle_mcp_tool_call;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct McpHandler;

#[async_trait]
impl ToolHandler for McpHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Mcp
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let payload = match payload {
            ToolPayload::Mcp {
                server,
                tool,
                raw_arguments,
            } => (server, tool, raw_arguments),
            _ => {
                return model_err("mcp handler received unsupported payload");
            }
        };

        let (server, tool, raw_arguments) = payload;
        let arguments_str = raw_arguments;

        let response = handle_mcp_tool_call(
            Arc::clone(&session),
            turn.as_ref(),
            call_id.clone(),
            server,
            tool,
            arguments_str,
        )
        .await;

        match response {
            savfox_protocol::models::ResponseInputItem::McpToolCallOutput { result, .. } => {
                Ok(ToolOutput::Mcp { result })
            }
            savfox_protocol::models::ResponseInputItem::FunctionCallOutput { output, .. } => {
                let savfox_protocol::models::FunctionCallOutputPayload {
                    content,
                    content_items,
                    success,
                } = output;
                Ok(ToolOutput::Function {
                    content,
                    content_items,
                    success,
                })
            }
            _ => model_err("mcp handler received unexpected response variant"),
        }
    }
}
