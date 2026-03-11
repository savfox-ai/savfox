use async_trait::async_trait;
use savfox_protocol::dynamic_tools::{DynamicToolCallRequest, DynamicToolResponse};
use savfox_protocol::protocol::EventMsg;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::warn;

use crate::function_tool::{FunctionCallError, model_err};
use crate::savfox::{Session, TurnContext};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct DynamicToolHandler;

#[async_trait]
impl ToolHandler for DynamicToolHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return model_err("dynamic tool handler received unsupported payload");
            }
        };

        let args: Value = parse_arguments(&arguments)?;
        let response = request_dynamic_tool(&session, turn.as_ref(), call_id, tool_name, args)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "dynamic tool call was cancelled before receiving a response".to_string(),
                )
            })?;

        Ok(ToolOutput::Function {
            content: response.output,
            content_items: None,
            success: Some(response.success),
        })
    }
}

async fn request_dynamic_tool(
    session: &Session,
    turn_context: &TurnContext,
    call_id: String,
    tool: String,
    arguments: Value,
) -> Option<DynamicToolResponse> {
    let _sub_id = turn_context.sub_id.clone();
    let (tx_response, rx_response) = oneshot::channel();
    let event_id = call_id.clone();
    let prev_entry = {
        let mut active = session.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                ts.insert_pending_dynamic_tool(call_id.clone(), tx_response)
            }
            None => None,
        }
    };
    if prev_entry.is_some() {
        warn!("Overwriting existing pending dynamic tool call for call_id: {event_id}");
    }

    let event = EventMsg::DynamicToolCallRequest(DynamicToolCallRequest {
        call_id,
        turn_id: turn_context.sub_id.clone(),
        tool,
        arguments,
    });
    session.send_event(turn_context, event).await;
    rx_response.await.ok()
}
