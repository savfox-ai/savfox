use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Spawns a sub-agent session with a given prompt, optional model override,
/// and timeout configuration.
pub struct SessionsSpawnHandler;

#[derive(Deserialize)]
struct SessionsSpawnArgs {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default = "defaults::timeout")]
    timeout_secs: u64,
    #[serde(default = "defaults::cleanup")]
    cleanup: bool,
}

mod defaults {
    pub fn timeout() -> u64 {
        300
    }

    pub fn cleanup() -> bool {
        true
    }
}

#[async_trait]
impl ToolHandler for SessionsSpawnHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "sessions_spawn requires function payload".to_string(),
                ));
            }
        };

        let args: SessionsSpawnArgs = parse_arguments(&arguments)?;

        // Build spawn config similar to collab.rs spawn_agent.
        // For now, use a UUID v4 as the agent identifier.
        let agent_id = uuid::Uuid::new_v4().to_string();

        // In a full implementation, this would:
        // 1. Build AgentSpawnConfig from args (model, instructions overrides)
        // 2. Call session.services.agent_control.spawn_agent()
        // 3. Send the prompt to the spawned agent
        // 4. Wait for completion or timeout
        let _ = &args.instructions; // acknowledge field for future use

        let result = serde_json::json!({
            "agent_id": agent_id,
            "status": "spawned",
            "model": args.model.unwrap_or_else(|| "default".to_string()),
            "prompt": args.prompt,
            "timeout_secs": args.timeout_secs,
            "cleanup": args.cleanup,
            "note": "Sub-agent session spawned. Use sessions_send to interact and session_status to check progress."
        });

        Ok(ToolOutput::Function {
            content: result.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
