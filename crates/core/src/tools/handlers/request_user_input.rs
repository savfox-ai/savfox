use savfox_protocol::config_types::ModeKind;
use savfox_protocol::request_user_input::RequestUserInputArgs;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct RequestUserInputHandler;

#[async_trait::async_trait]
impl ToolHandler for RequestUserInputHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("RequestUserInputHandler received unsupported payload"),
        };
        let args: RequestUserInputArgs = parse_arguments(&arguments)?;

        let session = &invocation.session;
        let turn = &invocation.turn;
        let call_id = invocation.call_id.clone();

        let mode = session.collaboration_mode().await.mode;
        if !matches!(mode, ModeKind::Plan | ModeKind::PairProgramming) {
            let mode_name = match mode {
                ModeKind::Code => "Code",
                ModeKind::Execute => "Execute",
                ModeKind::Custom => "Custom",
                ModeKind::Plan | ModeKind::PairProgramming => unreachable!(),
            };
            return model_err(format!(
                "request_user_input is unavailable in {mode_name} mode"
            ));
        }

        let mut args = args;
        let missing_options = args
            .questions
            .iter()
            .any(|question| question.options.as_ref().is_none_or(Vec::is_empty));
        if missing_options {
            return model_err("request_user_input requires non-empty options for every question");
        }
        for question in &mut args.questions {
            question.is_other = true;
        }
        let response = session
            .request_user_input(turn.as_ref(), call_id, args)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "request_user_input was cancelled before receiving a response".to_string(),
                )
            })?;

        let content = serde_json::to_string(&response).map_err(|err| {
            FunctionCallError::Fatal(format!(
                "failed to serialize request_user_input response: {err}"
            ))
        })?;

        Ok(ToolOutput::ok(content))
    }
}
