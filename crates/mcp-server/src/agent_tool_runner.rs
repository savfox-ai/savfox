//! Asynchronous worker that executes a **Savfox** tool-call inside a spawned
//! Tokio task. Separated from `message_processor.rs` to keep that file small
//! and to make future feature-growth easier to manage.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::model::{CallToolResult, Content, RequestId};
use savfox_core::config::Config as SavfoxConfig;
use savfox_core::protocol::{
    AgentMessageEvent, ApplyPatchApprovalRequestEvent, Event, EventMsg, ExecApprovalRequestEvent,
    Op, Submission, TurnCompleteEvent,
};
use savfox_core::{NewSession, SavfoxSession, SessionManager};
use savfox_protocol::SessionId;
use savfox_protocol::user_input::UserInput;
use serde_json::json;
use tokio::sync::Mutex;

use crate::exec_approval::handle_exec_approval_request;
use crate::outgoing_message::{OutgoingMessageSender, OutgoingNotificationMeta};
use crate::patch_approval::handle_patch_approval_request;

/// To adhere to MCP `tools/call` response format, include the Savfox
/// `sessionId` in the `structured_content` field of the response.
/// Some MCP clients ignore `content` when `structuredContent` is present, so
/// mirror the text there as well.
pub(crate) fn create_call_tool_result_with_session_id(
    session_id: SessionId,
    text: String,
    is_error: Option<bool>,
) -> CallToolResult {
    let content_text = text;
    let content = vec![Content::text(content_text.clone())];
    let structured_content = json!({
        "sessionId": session_id,
        "content": content_text,
    });
    let mut result = if is_error == Some(true) {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = Some(structured_content);
    result
}

/// Run a complete Savfox session and stream events back to the client.
///
/// On completion (success or error) the function sends the appropriate
/// `tools/call` response so the LLM can continue the conversation.
pub async fn run_savfox_tool_session(
    id: RequestId,
    initial_prompt: String,
    config: SavfoxConfig,
    outgoing: Arc<OutgoingMessageSender>,
    session_manager: Arc<SessionManager>,
    running_requests_id_to_savfox_uuid: Arc<Mutex<HashMap<RequestId, SessionId>>>,
) {
    let NewSession {
        session_id,
        session,
        session_configured,
    } = match session_manager.start_session(config).await {
        Ok(res) => res,
        Err(e) => {
            let result = CallToolResult::error(vec![Content::text(format!(
                "Failed to start Savfox session: {e}"
            ))]);
            outgoing.send_response(id.clone(), result).await;
            return;
        }
    };

    let session_configured_event = Event {
        // Use a fake id value for now.
        id: "".to_owned(),
        msg: EventMsg::SessionConfigured(session_configured.clone()),
    };
    outgoing
        .send_event_as_notification(
            &session_configured_event,
            Some(OutgoingNotificationMeta {
                request_id: Some(id.clone()),
                session_id: Some(session_id),
            }),
        )
        .await;

    // Use the original MCP request ID as the `sub_id` for the Savfox submission so that
    // any events emitted for this tool-call can be correlated with the
    // originating `tools/call` request.
    let sub_id = id.to_string();
    running_requests_id_to_savfox_uuid
        .lock()
        .await
        .insert(id.clone(), session_id);
    let submission = Submission {
        id: sub_id.clone(),
        op: Op::UserInput {
            items: vec![UserInput::Text {
                text: initial_prompt.clone(),
                // MCP tool prompts are plain text with no UI element ranges.
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        },
    };

    if let Err(e) = session.submit_with_id(submission).await {
        tracing::error!("Failed to submit initial prompt: {e}");
        let result = create_call_tool_result_with_session_id(
            session_id,
            format!("Failed to submit initial prompt: {e}"),
            Some(true),
        );
        outgoing.send_response(id.clone(), result).await;
        // unregister the id so we don't keep it in the map
        running_requests_id_to_savfox_uuid.lock().await.remove(&id);
        return;
    }

    run_savfox_tool_session_inner(
        session_id,
        session,
        outgoing,
        id,
        running_requests_id_to_savfox_uuid,
    )
    .await;
}

pub async fn run_savfox_tool_session_reply(
    session_id: SessionId,
    session: Arc<SavfoxSession>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    prompt: String,
    running_requests_id_to_savfox_uuid: Arc<Mutex<HashMap<RequestId, SessionId>>>,
) {
    running_requests_id_to_savfox_uuid
        .lock()
        .await
        .insert(request_id.clone(), session_id);
    if let Err(e) = session
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt,
                // MCP tool prompts are plain text with no UI element ranges.
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
    {
        tracing::error!("Failed to submit user input: {e}");
        let result = create_call_tool_result_with_session_id(
            session_id,
            format!("Failed to submit user input: {e}"),
            Some(true),
        );
        outgoing.send_response(request_id.clone(), result).await;
        // unregister the id so we don't keep it in the map
        running_requests_id_to_savfox_uuid
            .lock()
            .await
            .remove(&request_id);
        return;
    }

    run_savfox_tool_session_inner(
        session_id,
        session,
        outgoing,
        request_id,
        running_requests_id_to_savfox_uuid,
    )
    .await;
}

async fn run_savfox_tool_session_inner(
    session_id: SessionId,
    session: Arc<SavfoxSession>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    running_requests_id_to_savfox_uuid: Arc<Mutex<HashMap<RequestId, SessionId>>>,
) {
    let request_id_str = request_id.to_string();

    // Stream events until the task needs to pause for user interaction or
    // completes.
    loop {
        match session.next_event().await {
            Ok(event) => {
                outgoing
                    .send_event_as_notification(
                        &event,
                        Some(OutgoingNotificationMeta {
                            request_id: Some(request_id.clone()),
                            session_id: Some(session_id),
                        }),
                    )
                    .await;

                match event.msg {
                    EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                        turn_id: _,
                        command,
                        cwd,
                        call_id,
                        reason: _,
                        proposed_execpolicy_amendment: _,
                        parsed_cmd,
                    }) => {
                        handle_exec_approval_request(
                            command,
                            cwd,
                            outgoing.clone(),
                            session.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            call_id,
                            parsed_cmd,
                            session_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::PlanDelta(_) => {
                        continue;
                    }
                    EventMsg::Error(err_event) => {
                        // Always respond in tools/call's expected shape, and include conversationId
                        // so the client can resume.
                        let result = create_call_tool_result_with_session_id(
                            session_id,
                            err_event.message,
                            Some(true),
                        );
                        outgoing.send_response(request_id.clone(), result).await;
                        break;
                    }
                    EventMsg::Warning(_) => {
                        continue;
                    }
                    EventMsg::ElicitationRequest(_) => {
                        // TODO: forward elicitation requests to the client?
                        continue;
                    }
                    EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id,
                        turn_id: _,
                        reason,
                        grant_root,
                        changes,
                    }) => {
                        handle_patch_approval_request(
                            call_id,
                            reason,
                            grant_root,
                            changes,
                            outgoing.clone(),
                            session.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            session_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::TurnComplete(TurnCompleteEvent { last_agent_message }) => {
                        let text = match last_agent_message {
                            Some(msg) => msg,
                            None => "".to_owned(),
                        };
                        let result =
                            create_call_tool_result_with_session_id(session_id, text, None);
                        outgoing.send_response(request_id.clone(), result).await;
                        // unregister the id so we don't keep it in the map
                        running_requests_id_to_savfox_uuid
                            .lock()
                            .await
                            .remove(&request_id);
                        break;
                    }
                    EventMsg::SessionConfigured(_) => {
                        tracing::error!("unexpected SessionConfigured event");
                    }
                    EventMsg::SessionNameUpdated(_) => {
                        // Ignore session metadata updates in MCP tool runner.
                    }
                    EventMsg::AgentMessageDelta(_) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::AgentReasoningDelta(_) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::McpStartupUpdate(_) | EventMsg::McpStartupComplete(_) => {
                        // Ignored in MCP tool runner.
                    }
                    EventMsg::AgentMessage(AgentMessageEvent { .. }) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::AgentReasoningRawContent(_)
                    | EventMsg::AgentReasoningRawContentDelta(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::AgentReasoning(_)
                    | EventMsg::AgentReasoningSectionBreak(_)
                    | EventMsg::McpToolCallBegin(_)
                    | EventMsg::McpToolCallEnd(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::ListCustomPromptsResponse(_)
                    | EventMsg::ListSkillsResponse(_)
                    | EventMsg::ExecCommandBegin(_)
                    | EventMsg::TerminalInteraction(_)
                    | EventMsg::ExecCommandOutputDelta(_)
                    | EventMsg::ExecCommandEnd(_)
                    | EventMsg::BackgroundEvent(_)
                    | EventMsg::StreamError(_)
                    | EventMsg::PatchApplyBegin(_)
                    | EventMsg::PatchApplyEnd(_)
                    | EventMsg::TurnDiff(_)
                    | EventMsg::WebSearchBegin(_)
                    | EventMsg::WebSearchEnd(_)
                    | EventMsg::GetHistoryEntryResponse(_)
                    | EventMsg::PlanUpdate(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::UserMessage(_)
                    | EventMsg::ShutdownComplete
                    | EventMsg::ViewImageToolCall(_)
                    | EventMsg::RawResponseItem(_)
                    | EventMsg::EnteredReviewMode(_)
                    | EventMsg::ItemStarted(_)
                    | EventMsg::ItemCompleted(_)
                    | EventMsg::AgentMessageContentDelta(_)
                    | EventMsg::ReasoningContentDelta(_)
                    | EventMsg::ReasoningRawContentDelta(_)
                    | EventMsg::SkillsUpdateAvailable
                    | EventMsg::UndoStarted(_)
                    | EventMsg::UndoCompleted(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::DynamicToolCallRequest(_)
                    | EventMsg::SessionRolledBack(_)
                    | EventMsg::CollabAgentSpawnBegin(_)
                    | EventMsg::CollabAgentSpawnEnd(_)
                    | EventMsg::CollabAgentInteractionBegin(_)
                    | EventMsg::CollabAgentInteractionEnd(_)
                    | EventMsg::CollabWaitingBegin(_)
                    | EventMsg::CollabWaitingEnd(_)
                    | EventMsg::CollabCloseBegin(_)
                    | EventMsg::CollabCloseEnd(_)
                    | EventMsg::DeprecationNotice(_) => {
                        // For now, we do not do anything extra for these
                        // events. Note that
                        // send(savfox_event_to_notification(&event)) above has
                        // already dispatched these events as notifications,
                        // though we may want to do give different treatment to
                        // individual events in the future.
                    }
                }
            }
            Err(e) => {
                let result = create_call_tool_result_with_session_id(
                    session_id,
                    format!("Savfox runtime error: {e}"),
                    Some(true),
                );
                outgoing.send_response(request_id.clone(), result).await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn call_tool_result_includes_session_id_in_structured_content() {
        let session_id = SessionId::new();
        let result = create_call_tool_result_with_session_id(session_id, "done".to_owned(), None);
        assert_eq!(
            result.structured_content,
            Some(json!({
                "sessionId": session_id,
                "content": "done",
            }))
        );
    }
}
