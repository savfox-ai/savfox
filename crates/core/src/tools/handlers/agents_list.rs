use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

#[derive(Deserialize)]
#[allow(dead_code)]
struct AgentsListArgs {
    /// Optional filter string (e.g. by status or model).
    #[serde(default)]
    filter: Option<String>,
}

/// Lists active agents known to the current session.
///
/// Returns agent metadata once SessionManager integration is available;
/// currently returns an empty placeholder.
pub struct AgentsListHandler;

#[async_trait::async_trait]
impl ToolHandler for AgentsListHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("AgentsListHandler received unsupported payload"),
        };
        let _args: AgentsListArgs = parse_arguments(&arguments)?;
        let result = serde_json::json!({
            "agents": [],
            "count": 0,
            "note": "Agent listing requires SessionManager integration - will enumerate active agents when available"
        });

        Ok(ToolOutput::ok(result.to_string()))
    }
}
