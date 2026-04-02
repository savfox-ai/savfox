use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{ErrorData, RequestId};
use savfox_core::SavfoxSession;
use savfox_core::protocol::{Op, ReviewDecision};
use savfox_protocol::SessionId;
use savfox_protocol::parse_command::ParsedCommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::error;

/// Conforms to the MCP elicitation request params shape, so it can be used as
/// the `params` field of an `elicitation/create` request.
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecApprovalElicitRequestParams {
    // These fields are required so that `params`
    // conforms to ElicitRequestParams.
    pub message: String,

    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,

    // These are additional fields the client can use to
    // correlate the request with the savfox tool call.
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    pub savfox_elicitation: String,
    pub savfox_mcp_tool_call_id: String,
    pub savfox_event_id: String,
    pub savfox_call_id: String,
    pub savfox_command: Vec<String>,
    pub savfox_cwd: PathBuf,
    pub savfox_parsed_cmd: Vec<ParsedCommand>,
}

// TODO(mbolin): ExecApprovalResponse does not conform to ElicitResult. See:
// - https://github.com/modelcontextprotocol/modelcontextprotocol/blob/f962dc1780fa5eed7fb7c8a0232f1fc83ef220cd/schema/2025-06-18/schema.json#L617-L636
// - https://modelcontextprotocol.io/specification/draft/client/elicitation#protocol-messages
// It should have "action" and "content" fields.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecApprovalResponse {
    pub decision: ReviewDecision,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_exec_approval_request(
    command: Vec<String>,
    cwd: PathBuf,
    outgoing: Arc<crate::outgoing_message::OutgoingMessageSender>,
    savfox: Arc<SavfoxSession>,
    request_id: RequestId,
    tool_call_id: String,
    event_id: String,
    call_id: String,
    savfox_parsed_cmd: Vec<ParsedCommand>,
    session_id: SessionId,
) {
    let escaped_command =
        shlex::try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "));
    let message = format!(
        "Allow Savfox to run `{escaped_command}` in `{cwd}`?",
        cwd = cwd.to_string_lossy()
    );

    let params = ExecApprovalElicitRequestParams {
        message,
        requested_schema: json!({"type":"object","properties":{}}),
        session_id,
        savfox_elicitation: "exec-approval".to_owned(),
        savfox_mcp_tool_call_id: tool_call_id.clone(),
        savfox_event_id: event_id.clone(),
        savfox_call_id: call_id,
        savfox_command: command,
        savfox_cwd: cwd,
        savfox_parsed_cmd,
    };
    let params_json = match serde_json::to_value(&params) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("Failed to serialize ExecApprovalElicitRequestParams: {err}");
            error!("{message}");

            outgoing
                .send_error(request_id.clone(), ErrorData::invalid_params(message, None))
                .await;

            return;
        }
    };

    let on_response = outgoing
        .send_request("elicitation/create", Some(params_json))
        .await;

    // Listen for the response on a separate task so we don't block the main agent loop.
    {
        let savfox = savfox.clone();
        let event_id = event_id.clone();
        tokio::spawn(async move {
            on_exec_approval_response(event_id, on_response, savfox).await;
        });
    }
}

async fn on_exec_approval_response(
    event_id: String,
    receiver: tokio::sync::oneshot::Receiver<serde_json::Value>,
    savfox: Arc<SavfoxSession>,
) {
    let response = receiver.await;
    let value = match response {
        Ok(value) => value,
        Err(err) => {
            error!("request failed: {err:?}");
            return;
        }
    };

    // Try to deserialize `value` and then make the appropriate call to `savfox`.
    let response = serde_json::from_value::<ExecApprovalResponse>(value).unwrap_or_else(|err| {
        error!("failed to deserialize ExecApprovalResponse: {err}");
        // If we cannot deserialize the response, we deny the request to be
        // conservative.
        ExecApprovalResponse {
            decision: ReviewDecision::Denied,
        }
    });

    if let Err(err) = savfox
        .submit(Op::ExecApproval {
            id: event_id,
            decision: response.decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}
