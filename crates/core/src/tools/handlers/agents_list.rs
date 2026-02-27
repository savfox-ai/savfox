use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Lists active agents known to the current session.
///
/// Returns agent metadata once SessionManager integration is available;
/// currently returns an empty placeholder.
pub struct AgentsListHandler;

#[derive(Deserialize)]
#[allow(dead_code)]
struct AgentsListArgs {
    /// Optional filter string (e.g. by status or model).
    #[serde(default)]
    filter: Option<String>,
}

#[async_trait]
impl ToolHandler for AgentsListHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "agents_list requires function payload".to_string(),
                ));
            }
        };

        let _args: AgentsListArgs = parse_arguments(&arguments)?;

        let result = serde_json::json!({
            "agents": [],
            "count": 0,
            "note": "Agent listing requires SessionManager integration - will enumerate active agents when available"
        });

        Ok(ToolOutput::Function {
            content: result.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
