use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

/// Manages paired device nodes (list, status, run_command, camera_capture,
/// get_location, send_notification).
///
/// Nodes are mobile or IoT devices paired via the Savfox mobile app.
/// This handler returns placeholder responses until the node pairing
/// protocol is fully integrated.

#[derive(Deserialize)]
#[allow(dead_code)]
struct NodesArgs {
    /// One of "list", "status", "run_command", "camera_capture",
    /// "get_location", "send_notification".
    action: String,
    /// Target node identifier (required for all actions except "list").
    #[serde(default)]
    node_id: Option<String>,
    /// Shell command to execute on the node (used by "run_command").
    #[serde(default)]
    command: Option<String>,
    /// Notification body text (used by "send_notification").
    #[serde(default)]
    message: Option<String>,
    /// Notification title (used by "send_notification").
    #[serde(default)]
    title: Option<String>,
}

pub struct NodesHandler;

#[async_trait::async_trait]
impl ToolHandler for NodesHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("NodesHandler received unsupported payload"),
        };
        let args: NodesArgs = parse_arguments(&arguments)?;

        let result = match args.action.as_str() {
            "list" => serde_json::json!({
                "nodes": [],
                "note": "No nodes are currently paired. Use the Savfox mobile app to pair a device."
            }),
            "status" => serde_json::json!({
                "status": "no_nodes_paired",
                "node_id": args.node_id,
            }),
            "run_command" | "camera_capture" | "get_location" | "send_notification" => {
                serde_json::json!({
                    "error": "no_nodes_paired",
                    "action": args.action,
                    "note": "No nodes are currently paired. Pair a device first using the Savfox mobile app."
                })
            }
            other => {
                return model_err(format!("unknown nodes action: {other}"));
            }
        };

        Ok(ToolOutput::ok(result.to_string()))
    }
}
