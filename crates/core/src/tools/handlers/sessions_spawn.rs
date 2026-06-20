use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

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

/// Spawns a sub-agent session with a given prompt, optional model override,
/// and timeout configuration.
pub struct SessionsSpawnHandler;

#[async_trait::async_trait]
impl ToolHandler for SessionsSpawnHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("SessionsSpawnHandler received unsupported payload"),
        };
        let args: SessionsSpawnArgs = parse_arguments(&arguments)?;

        // Sub-agent spawning is not yet wired to `agent_control.spawn_agent()`.
        // Previously this returned a fabricated `agent_id` with status "spawned"
        // and told the model to drive it via `sessions_send`/`session_status`
        // (which are likewise unimplemented) — a success-shaped response that
        // sent the model down a broken workflow. Fail explicitly instead.
        let _ = (
            &args.prompt,
            &args.model,
            &args.instructions,
            args.timeout_secs,
            args.cleanup,
        );
        model_err(
            "sessions_spawn is not implemented yet: sub-agent spawning is not wired up. \
             Do not rely on sessions_send/session_status either.",
        )
    }
}
