use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::PathBuf;
use std::sync::Arc;

use savfox_app_server_protocol::{
    AccountRateLimitsUpdatedNotification, AgentMessageDeltaNotification,
    CollabAgentState as V2CollabAgentStatus, CollabAgentTool,
    CollabAgentToolCallStatus as V2CollabToolCallStatus, CommandAction as V2ParsedCommand,
    CommandExecutionApprovalDecision, CommandExecutionOutputDeltaNotification,
    CommandExecutionRequestApprovalParams, CommandExecutionRequestApprovalResponse,
    CommandExecutionStatus, DeprecationNoticeNotification, DynamicToolCallParams,
    ErrorNotification, ExecPolicyAmendment as V2ExecPolicyAmendment, FileChangeApprovalDecision,
    FileChangeOutputDeltaNotification, FileChangeRequestApprovalParams,
    FileChangeRequestApprovalResponse, FileUpdateChange, ItemCompletedNotification,
    ItemStartedNotification, JSONRPCErrorError, McpToolCallError, McpToolCallResult,
    McpToolCallStatus, PatchApplyStatus, PatchChangeKind as V2PatchChangeKind,
    PlanDeltaNotification, RawResponseItemCompletedNotification,
    ReasoningSummaryPartAddedNotification, ReasoningSummaryTextDeltaNotification,
    ReasoningTextDeltaNotification, SavfoxErrorInfo as V2SavfoxErrorInfo, ServerNotification,
    ServerRequestPayload, SessionItem, SessionNameUpdatedNotification, SessionRollbackResponse,
    SessionTokenUsage, SessionTokenUsageUpdatedNotification, TerminalInteractionNotification,
    ToolRequestUserInputOption, ToolRequestUserInputParams, ToolRequestUserInputQuestion,
    ToolRequestUserInputResponse, Turn, TurnCompletedNotification, TurnDiffUpdatedNotification,
    TurnError, TurnInterruptResponse, TurnPlanStep, TurnPlanUpdatedNotification, TurnStatus,
    build_turns_from_event_msgs,
};
use savfox_core::parse_command::shlex_join;
use savfox_core::protocol::{
    ApplyPatchApprovalRequestEvent, Event, EventMsg, ExecApprovalRequestEvent, ExecCommandEndEvent,
    FileChange as CoreFileChange, McpToolCallBeginEvent, McpToolCallEndEvent, Op, ReviewDecision,
    SavfoxErrorInfo as CoreSavfoxErrorInfo, TokenCountEvent, TurnDiffEvent,
};
use savfox_core::review_format::format_review_findings_block;
use savfox_core::{SavfoxSession, review_prompts};
use savfox_protocol::SessionId;
use savfox_protocol::plan_tool::UpdatePlanArgs;
use savfox_protocol::protocol::ReviewOutputEvent;
use savfox_protocol::request_user_input::{
    RequestUserInputAnswer as CoreRequestUserInputAnswer,
    RequestUserInputResponse as CoreRequestUserInputResponse,
};
use tokio::sync::oneshot;
use tracing::error;

use crate::error_code::{INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE};
use crate::outgoing_message::OutgoingMessageSender;
use crate::savfox_message_processor::{
    PendingInterrupts, PendingRollbacks, TurnSummary, TurnSummaryStore,
    read_event_msgs_from_rollout, read_summary_from_rollout, summary_to_session,
};

type JsonValue = serde_json::Value;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_bespoke_event_handling(
    event: Event,
    conversation_id: SessionId,
    conversation: Arc<SavfoxSession>,
    outgoing: Arc<OutgoingMessageSender>,
    pending_interrupts: PendingInterrupts,
    pending_rollbacks: PendingRollbacks,
    turn_summary_store: TurnSummaryStore,
    fallback_model_provider: String,
) {
    let Event {
        id: event_turn_id,
        msg,
    } = event;
    match msg {
        EventMsg::TurnStarted(_) => {}
        EventMsg::TurnComplete(_ev) => {
            handle_turn_complete(
                conversation_id,
                event_turn_id,
                &outgoing,
                &turn_summary_store,
            )
            .await;
        }
        EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id,
            changes,
            reason,
            grant_root,
        }) => {
            // Until we migrate the core to be aware of a first class FileChangeItem
            // and emit the corresponding EventMsg, we repurpose the call_id as the item_id.
            let item_id = call_id.clone();
            let patch_changes = convert_patch_changes(&changes);

            let first_start = {
                let mut map = turn_summary_store.lock().await;
                let summary = map.entry(conversation_id).or_default();
                summary.file_change_started.insert(item_id.clone())
            };
            if first_start {
                let item = SessionItem::FileChange {
                    id: item_id.clone(),
                    changes: patch_changes.clone(),
                    status: PatchApplyStatus::InProgress,
                };
                let notification = ItemStartedNotification {
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                    item,
                };
                outgoing
                    .send_server_notification(ServerNotification::ItemStarted(notification))
                    .await;
            }

            let params = FileChangeRequestApprovalParams {
                session_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                item_id: item_id.clone(),
                reason,
                grant_root,
            };
            let rx = outgoing
                .send_request(ServerRequestPayload::FileChangeRequestApproval(params))
                .await;
            tokio::spawn(async move {
                on_file_change_request_approval_response(
                    event_turn_id,
                    conversation_id,
                    item_id,
                    patch_changes,
                    rx,
                    conversation,
                    outgoing,
                    turn_summary_store,
                )
                .await;
            });
        }
        EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            turn_id,
            command,
            cwd,
            reason,
            proposed_execpolicy_amendment,
            parsed_cmd,
        }) => {
            let item_id = call_id.clone();
            let command_actions = parsed_cmd
                .iter()
                .cloned()
                .map(V2ParsedCommand::from)
                .collect::<Vec<_>>();
            let command_string = shlex_join(&command);
            let proposed_execpolicy_amendment_v2 =
                proposed_execpolicy_amendment.map(V2ExecPolicyAmendment::from);

            let params = CommandExecutionRequestApprovalParams {
                session_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                // Until we migrate the core to be aware of a first class CommandExecutionItem
                // and emit the corresponding EventMsg, we repurpose the call_id as the item_id.
                item_id: item_id.clone(),
                reason,
                command: Some(command_string.clone()),
                cwd: Some(cwd.clone()),
                command_actions: Some(command_actions.clone()),
                proposed_execpolicy_amendment: proposed_execpolicy_amendment_v2,
            };
            let rx = outgoing
                .send_request(ServerRequestPayload::CommandExecutionRequestApproval(
                    params,
                ))
                .await;
            tokio::spawn(async move {
                on_command_execution_request_approval_response(
                    event_turn_id,
                    conversation_id,
                    item_id,
                    command_string,
                    cwd,
                    command_actions,
                    rx,
                    conversation,
                    outgoing,
                )
                .await;
            });
        }
        EventMsg::RequestUserInput(request) => {
            let questions = request
                .questions
                .into_iter()
                .map(|question| ToolRequestUserInputQuestion {
                    id: question.id,
                    header: question.header,
                    question: question.question,
                    is_other: question.is_other,
                    is_secret: question.is_secret,
                    options: question.options.map(|options| {
                        options
                            .into_iter()
                            .map(|option| ToolRequestUserInputOption {
                                label: option.label,
                                description: option.description,
                            })
                            .collect()
                    }),
                })
                .collect();
            let params = ToolRequestUserInputParams {
                session_id: conversation_id.to_string(),
                turn_id: request.turn_id,
                item_id: request.call_id,
                questions,
            };
            let rx = outgoing
                .send_request(ServerRequestPayload::ToolRequestUserInput(params))
                .await;
            tokio::spawn(async move {
                on_request_user_input_response(event_turn_id, rx, conversation).await;
            });
        }
        EventMsg::DynamicToolCallRequest(request) => {
            let call_id = request.call_id;
            let params = DynamicToolCallParams {
                session_id: conversation_id.to_string(),
                turn_id: request.turn_id,
                call_id: call_id.clone(),
                tool: request.tool,
                arguments: request.arguments,
            };
            let rx = outgoing
                .send_request(ServerRequestPayload::DynamicToolCall(params))
                .await;
            tokio::spawn(async move {
                crate::dynamic_tools::on_call_response(call_id, rx, conversation).await;
            });
        }
        // TODO(celia): properly construct McpToolCall TurnItem in core.
        EventMsg::McpToolCallBegin(begin_event) => {
            let notification = construct_mcp_tool_call_notification(
                begin_event,
                conversation_id.to_string(),
                event_turn_id.clone(),
            )
            .await;
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::McpToolCallEnd(end_event) => {
            let notification = construct_mcp_tool_call_end_notification(
                end_event,
                conversation_id.to_string(),
                event_turn_id.clone(),
            )
            .await;
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::CollabAgentSpawnBegin(begin_event) => {
            let item = SessionItem::CollabAgentToolCall {
                id: begin_event.call_id,
                tool: CollabAgentTool::SpawnAgent,
                status: V2CollabToolCallStatus::InProgress,
                sender_session_id: begin_event.sender_session_id.to_string(),
                receiver_session_ids: Vec::new(),
                prompt: Some(begin_event.prompt),
                agents_states: HashMap::new(),
            };
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::CollabAgentSpawnEnd(end_event) => {
            let has_receiver = end_event.new_session_id.is_some();
            let status = match &end_event.status {
                savfox_protocol::protocol::AgentStatus::Errored(_)
                | savfox_protocol::protocol::AgentStatus::NotFound => {
                    V2CollabToolCallStatus::Failed
                }
                _ if has_receiver => V2CollabToolCallStatus::Completed,
                _ => V2CollabToolCallStatus::Failed,
            };
            let (receiver_session_ids, agents_states) = match end_event.new_session_id {
                Some(id) => {
                    let receiver_id = id.to_string();
                    let received_status = V2CollabAgentStatus::from(end_event.status.clone());
                    (
                        vec![receiver_id.clone()],
                        [(receiver_id, received_status)].into_iter().collect(),
                    )
                }
                None => (Vec::new(), HashMap::new()),
            };
            let item = SessionItem::CollabAgentToolCall {
                id: end_event.call_id,
                tool: CollabAgentTool::SpawnAgent,
                status,
                sender_session_id: end_event.sender_session_id.to_string(),
                receiver_session_ids,
                prompt: Some(end_event.prompt),
                agents_states,
            };
            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::CollabAgentInteractionBegin(begin_event) => {
            let receiver_session_ids = vec![begin_event.receiver_session_id.to_string()];
            let item = SessionItem::CollabAgentToolCall {
                id: begin_event.call_id,
                tool: CollabAgentTool::SendInput,
                status: V2CollabToolCallStatus::InProgress,
                sender_session_id: begin_event.sender_session_id.to_string(),
                receiver_session_ids,
                prompt: Some(begin_event.prompt),
                agents_states: HashMap::new(),
            };
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::CollabAgentInteractionEnd(end_event) => {
            let status = match &end_event.status {
                savfox_protocol::protocol::AgentStatus::Errored(_)
                | savfox_protocol::protocol::AgentStatus::NotFound => {
                    V2CollabToolCallStatus::Failed
                }
                _ => V2CollabToolCallStatus::Completed,
            };
            let receiver_id = end_event.receiver_session_id.to_string();
            let received_status = V2CollabAgentStatus::from(end_event.status);
            let item = SessionItem::CollabAgentToolCall {
                id: end_event.call_id,
                tool: CollabAgentTool::SendInput,
                status,
                sender_session_id: end_event.sender_session_id.to_string(),
                receiver_session_ids: vec![receiver_id.clone()],
                prompt: Some(end_event.prompt),
                agents_states: [(receiver_id, received_status)].into_iter().collect(),
            };
            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::CollabWaitingBegin(begin_event) => {
            let receiver_session_ids = begin_event
                .receiver_session_ids
                .iter()
                .map(ToString::to_string)
                .collect();
            let item = SessionItem::CollabAgentToolCall {
                id: begin_event.call_id,
                tool: CollabAgentTool::Wait,
                status: V2CollabToolCallStatus::InProgress,
                sender_session_id: begin_event.sender_session_id.to_string(),
                receiver_session_ids,
                prompt: None,
                agents_states: HashMap::new(),
            };
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::CollabWaitingEnd(end_event) => {
            let status = if end_event.statuses.values().any(|status| {
                matches!(
                    status,
                    savfox_protocol::protocol::AgentStatus::Errored(_)
                        | savfox_protocol::protocol::AgentStatus::NotFound
                )
            }) {
                V2CollabToolCallStatus::Failed
            } else {
                V2CollabToolCallStatus::Completed
            };
            let receiver_session_ids = end_event.statuses.keys().map(ToString::to_string).collect();
            let agents_states = end_event
                .statuses
                .iter()
                .map(|(id, status)| (id.to_string(), V2CollabAgentStatus::from(status.clone())))
                .collect();
            let item = SessionItem::CollabAgentToolCall {
                id: end_event.call_id,
                tool: CollabAgentTool::Wait,
                status,
                sender_session_id: end_event.sender_session_id.to_string(),
                receiver_session_ids,
                prompt: None,
                agents_states,
            };
            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::CollabCloseBegin(begin_event) => {
            let item = SessionItem::CollabAgentToolCall {
                id: begin_event.call_id,
                tool: CollabAgentTool::CloseAgent,
                status: V2CollabToolCallStatus::InProgress,
                sender_session_id: begin_event.sender_session_id.to_string(),
                receiver_session_ids: vec![begin_event.receiver_session_id.to_string()],
                prompt: None,
                agents_states: HashMap::new(),
            };
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::CollabCloseEnd(end_event) => {
            let status = match &end_event.status {
                savfox_protocol::protocol::AgentStatus::Errored(_)
                | savfox_protocol::protocol::AgentStatus::NotFound => {
                    V2CollabToolCallStatus::Failed
                }
                _ => V2CollabToolCallStatus::Completed,
            };
            let receiver_id = end_event.receiver_session_id.to_string();
            let agents_states = [(
                receiver_id.clone(),
                V2CollabAgentStatus::from(end_event.status),
            )]
            .into_iter()
            .collect();
            let item = SessionItem::CollabAgentToolCall {
                id: end_event.call_id,
                tool: CollabAgentTool::CloseAgent,
                status,
                sender_session_id: end_event.sender_session_id.to_string(),
                receiver_session_ids: vec![receiver_id],
                prompt: None,
                agents_states,
            };
            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::AgentMessageContentDelta(event) => {
            let savfox_protocol::protocol::AgentMessageContentDeltaEvent { item_id, delta, .. } =
                event;
            let notification = AgentMessageDeltaNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id,
                delta,
            };
            outgoing
                .send_server_notification(ServerNotification::AgentMessageDelta(notification))
                .await;
        }
        EventMsg::PlanDelta(event) => {
            let notification = PlanDeltaNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id: event.item_id,
                delta: event.delta,
            };
            outgoing
                .send_server_notification(ServerNotification::PlanDelta(notification))
                .await;
        }
        EventMsg::DeprecationNotice(event) => {
            let notification = DeprecationNoticeNotification {
                summary: event.summary,
                details: event.details,
            };
            outgoing
                .send_server_notification(ServerNotification::DeprecationNotice(notification))
                .await;
        }
        EventMsg::ReasoningContentDelta(event) => {
            let notification = ReasoningSummaryTextDeltaNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id: event.item_id,
                delta: event.delta,
                summary_index: event.summary_index,
            };
            outgoing
                .send_server_notification(ServerNotification::ReasoningSummaryTextDelta(
                    notification,
                ))
                .await;
        }
        EventMsg::ReasoningRawContentDelta(event) => {
            let notification = ReasoningTextDeltaNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id: event.item_id,
                delta: event.delta,
                content_index: event.content_index,
            };
            outgoing
                .send_server_notification(ServerNotification::ReasoningTextDelta(notification))
                .await;
        }
        EventMsg::AgentReasoningSectionBreak(event) => {
            let notification = ReasoningSummaryPartAddedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id: event.item_id,
                summary_index: event.summary_index,
            };
            outgoing
                .send_server_notification(ServerNotification::ReasoningSummaryPartAdded(
                    notification,
                ))
                .await;
        }
        EventMsg::TokenCount(token_count_event) => {
            handle_token_count_event(conversation_id, event_turn_id, token_count_event, &outgoing)
                .await;
        }
        EventMsg::Error(ev) => {
            let message = ev.message.clone();
            let savfox_error_info = ev.savfox_error_info.clone();

            // If this error belongs to an in-flight `session/rollback` request, fail that request
            // (and clear pending state) so subsequent rollbacks are unblocked.
            //
            // Don't send a notification for this error.
            if matches!(
                savfox_error_info,
                Some(CoreSavfoxErrorInfo::SessionRollbackFailed)
            ) {
                return handle_session_rollback_failed(
                    conversation_id,
                    message,
                    &pending_rollbacks,
                    &outgoing,
                )
                .await;
            };

            let turn_error = TurnError {
                message: ev.message,
                savfox_error_info: ev.savfox_error_info.map(V2SavfoxErrorInfo::from),
                additional_details: None,
            };
            handle_error(conversation_id, turn_error.clone(), &turn_summary_store).await;
            outgoing
                .send_server_notification(ServerNotification::Error(ErrorNotification {
                    error: turn_error.clone(),
                    will_retry: false,
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                }))
                .await;
        }
        EventMsg::StreamError(ev) => {
            // We don't need to update the turn summary store for stream errors as they are
            // intermediate error states for retries, but we notify the client.
            let turn_error = TurnError {
                message: ev.message,
                savfox_error_info: ev.savfox_error_info.map(V2SavfoxErrorInfo::from),
                additional_details: ev.additional_details,
            };
            outgoing
                .send_server_notification(ServerNotification::Error(ErrorNotification {
                    error: turn_error,
                    will_retry: true,
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                }))
                .await;
        }
        EventMsg::ViewImageToolCall(view_image_event) => {
            let item = SessionItem::ImageView {
                id: view_image_event.call_id.clone(),
                path: view_image_event.path.to_string_lossy().into_owned(),
            };
            let started = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item: item.clone(),
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(started))
                .await;
            let completed = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(completed))
                .await;
        }
        EventMsg::EnteredReviewMode(review_request) => {
            let review = review_request
                .user_facing_hint
                .unwrap_or_else(|| review_prompts::user_facing_hint(&review_request.target));
            let item = SessionItem::EnteredReviewMode {
                id: event_turn_id.clone(),
                review,
            };
            let started = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item: item.clone(),
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(started))
                .await;
            let completed = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(completed))
                .await;
        }
        EventMsg::ItemStarted(item_started_event) => {
            let item: SessionItem = item_started_event.item.clone().into();
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::ItemCompleted(item_completed_event) => {
            let item: SessionItem = item_completed_event.item.clone().into();
            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        EventMsg::ExitedReviewMode(review_event) => {
            let review = match review_event.review_output {
                Some(output) => render_review_output_text(&output),
                None => REVIEW_FALLBACK_MESSAGE.to_owned(),
            };
            let item = SessionItem::ExitedReviewMode {
                id: event_turn_id.clone(),
                review,
            };
            let started = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item: item.clone(),
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(started))
                .await;
            let completed = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(completed))
                .await;
        }
        EventMsg::RawResponseItem(raw_response_item_event) => {
            maybe_emit_raw_response_item_completed(
                conversation_id,
                &event_turn_id,
                raw_response_item_event.item,
                outgoing.as_ref(),
            )
            .await;
        }
        EventMsg::PatchApplyBegin(patch_begin_event) => {
            // Until we migrate the core to be aware of a first class FileChangeItem
            // and emit the corresponding EventMsg, we repurpose the call_id as the item_id.
            let item_id = patch_begin_event.call_id.clone();

            let first_start = {
                let mut map = turn_summary_store.lock().await;
                let summary = map.entry(conversation_id).or_default();
                summary.file_change_started.insert(item_id.clone())
            };
            if first_start {
                let item = SessionItem::FileChange {
                    id: item_id.clone(),
                    changes: convert_patch_changes(&patch_begin_event.changes),
                    status: PatchApplyStatus::InProgress,
                };
                let notification = ItemStartedNotification {
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                    item,
                };
                outgoing
                    .send_server_notification(ServerNotification::ItemStarted(notification))
                    .await;
            }
        }
        EventMsg::PatchApplyEnd(patch_end_event) => {
            // Until we migrate the core to be aware of a first class FileChangeItem
            // and emit the corresponding EventMsg, we repurpose the call_id as the item_id.
            let item_id = patch_end_event.call_id.clone();

            let status = if patch_end_event.success {
                PatchApplyStatus::Completed
            } else {
                PatchApplyStatus::Failed
            };
            let changes = convert_patch_changes(&patch_end_event.changes);
            complete_file_change_item(
                conversation_id,
                item_id,
                changes,
                status,
                event_turn_id.clone(),
                outgoing.as_ref(),
                &turn_summary_store,
            )
            .await;
        }
        EventMsg::ExecCommandBegin(exec_command_begin_event) => {
            let item_id = exec_command_begin_event.call_id.clone();
            let command_actions = exec_command_begin_event
                .parsed_cmd
                .into_iter()
                .map(V2ParsedCommand::from)
                .collect::<Vec<_>>();
            let command = shlex_join(&exec_command_begin_event.command);
            let cwd = exec_command_begin_event.cwd;
            let process_id = exec_command_begin_event.process_id;

            let item = SessionItem::CommandExecution {
                id: item_id,
                command,
                cwd,
                process_id,
                status: CommandExecutionStatus::InProgress,
                command_actions,
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
            };
            let notification = ItemStartedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        EventMsg::ExecCommandOutputDelta(exec_command_output_delta_event) => {
            let item_id = exec_command_output_delta_event.call_id.clone();
            let delta = String::from_utf8_lossy(&exec_command_output_delta_event.chunk).to_string();
            // The underlying EventMsg::ExecCommandOutputDelta is used for shell, unified_exec,
            // and apply_patch tool calls. We represent apply_patch with the FileChange item, and
            // everything else with the CommandExecution item.
            //
            // We need to detect which item type it is so we can emit the right notification.
            // We already have state tracking FileChange items on item/started, so let's use that.
            let is_file_change = {
                let map = turn_summary_store.lock().await;
                map.get(&conversation_id)
                    .is_some_and(|summary| summary.file_change_started.contains(&item_id))
            };
            if is_file_change {
                let notification = FileChangeOutputDeltaNotification {
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                    item_id,
                    delta,
                };
                outgoing
                    .send_server_notification(ServerNotification::FileChangeOutputDelta(
                        notification,
                    ))
                    .await;
            } else {
                let notification = CommandExecutionOutputDeltaNotification {
                    session_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                    item_id,
                    delta,
                };
                outgoing
                    .send_server_notification(ServerNotification::CommandExecutionOutputDelta(
                        notification,
                    ))
                    .await;
            }
        }
        EventMsg::TerminalInteraction(terminal_event) => {
            let item_id = terminal_event.call_id.clone();

            let notification = TerminalInteractionNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item_id,
                process_id: terminal_event.process_id,
                stdin: terminal_event.stdin,
            };
            outgoing
                .send_server_notification(ServerNotification::TerminalInteraction(notification))
                .await;
        }
        EventMsg::ExecCommandEnd(exec_command_end_event) => {
            let ExecCommandEndEvent {
                call_id,
                command,
                cwd,
                parsed_cmd,
                process_id,
                aggregated_output,
                exit_code,
                duration,
                ..
            } = exec_command_end_event;

            let status = if exit_code == 0 {
                CommandExecutionStatus::Completed
            } else {
                CommandExecutionStatus::Failed
            };
            let command_actions = parsed_cmd
                .into_iter()
                .map(V2ParsedCommand::from)
                .collect::<Vec<_>>();

            let aggregated_output = if aggregated_output.is_empty() {
                None
            } else {
                Some(aggregated_output)
            };

            let duration_ms = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);

            let item = SessionItem::CommandExecution {
                id: call_id,
                command: shlex_join(&command),
                cwd,
                process_id,
                status,
                command_actions,
                aggregated_output,
                exit_code: Some(exit_code),
                duration_ms: Some(duration_ms),
            };

            let notification = ItemCompletedNotification {
                session_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                item,
            };
            outgoing
                .send_server_notification(ServerNotification::ItemCompleted(notification))
                .await;
        }
        // If this is a TurnAborted, reply to any pending interrupt requests.
        EventMsg::TurnAborted(_turn_aborted_event) => {
            let pending = {
                let mut map = pending_interrupts.lock().await;
                map.remove(&conversation_id).unwrap_or_default()
            };
            if !pending.is_empty() {
                for rid in pending {
                    let response = TurnInterruptResponse {};
                    outgoing.send_response(rid, response).await;
                }
            }

            handle_turn_interrupted(
                conversation_id,
                event_turn_id,
                &outgoing,
                &turn_summary_store,
            )
            .await;
        }
        EventMsg::SessionRolledBack(_rollback_event) => {
            let pending = {
                let mut map = pending_rollbacks.lock().await;
                map.remove(&conversation_id)
            };

            if let Some(request_id) = pending {
                let Some(rollout_path) = conversation.rollout_path() else {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "session has no persisted rollout".to_owned(),
                        data: None,
                    };
                    outgoing.send_error(request_id, error).await;
                    return;
                };
                let response = match read_summary_from_rollout(
                    rollout_path.as_path(),
                    fallback_model_provider.as_str(),
                )
                .await
                {
                    Ok(summary) => {
                        let mut session = summary_to_session(summary);
                        match read_event_msgs_from_rollout(rollout_path.as_path()).await {
                            Ok(events) => {
                                session.turns = build_turns_from_event_msgs(&events);
                                SessionRollbackResponse { session }
                            }
                            Err(err) => {
                                let error = JSONRPCErrorError {
                                    code: INTERNAL_ERROR_CODE,
                                    message: format!(
                                        "failed to load rollout `{}`: {err}",
                                        rollout_path.display()
                                    ),
                                    data: None,
                                };
                                outgoing.send_error(request_id, error).await;
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        let error = JSONRPCErrorError {
                            code: INTERNAL_ERROR_CODE,
                            message: format!(
                                "failed to load rollout `{}`: {err}",
                                rollout_path.display()
                            ),
                            data: None,
                        };
                        outgoing.send_error(request_id, error).await;
                        return;
                    }
                };

                outgoing.send_response(request_id, response).await;
            }
        }
        EventMsg::SessionNameUpdated(session_name_event) => {
            let notification = SessionNameUpdatedNotification {
                session_id: session_name_event.session_id.to_string(),
                session_name: session_name_event.session_name,
            };
            outgoing
                .send_server_notification(ServerNotification::SessionNameUpdated(notification))
                .await;
        }
        EventMsg::TurnDiff(turn_diff_event) => {
            handle_turn_diff(
                conversation_id,
                &event_turn_id,
                turn_diff_event,
                outgoing.as_ref(),
            )
            .await;
        }
        EventMsg::PlanUpdate(plan_update_event) => {
            handle_turn_plan_update(
                conversation_id,
                &event_turn_id,
                plan_update_event,
                outgoing.as_ref(),
            )
            .await;
        }

        _ => {}
    }
}

async fn handle_turn_diff(
    conversation_id: SessionId,
    event_turn_id: &str,
    turn_diff_event: TurnDiffEvent,
    outgoing: &OutgoingMessageSender,
) {
    let notification = TurnDiffUpdatedNotification {
        session_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_owned(),
        diff: turn_diff_event.unified_diff,
    };
    outgoing
        .send_server_notification(ServerNotification::TurnDiffUpdated(notification))
        .await;
}

async fn handle_turn_plan_update(
    conversation_id: SessionId,
    event_turn_id: &str,
    plan_update_event: UpdatePlanArgs,
    outgoing: &OutgoingMessageSender,
) {
    // `update_plan` is a todo/checklist tool; it is not related to plan-mode updates
    let notification = TurnPlanUpdatedNotification {
        session_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_owned(),
        explanation: plan_update_event.explanation,
        plan: plan_update_event
            .plan
            .into_iter()
            .map(TurnPlanStep::from)
            .collect(),
    };
    outgoing
        .send_server_notification(ServerNotification::TurnPlanUpdated(notification))
        .await;
}

async fn emit_turn_completed_with_status(
    conversation_id: SessionId,
    event_turn_id: String,
    status: TurnStatus,
    error: Option<TurnError>,
    outgoing: &OutgoingMessageSender,
) {
    let notification = TurnCompletedNotification {
        session_id: conversation_id.to_string(),
        turn: Turn {
            id: event_turn_id,
            items: vec![],
            error,
            status,
        },
    };
    outgoing
        .send_server_notification(ServerNotification::TurnCompleted(notification))
        .await;
}

async fn complete_file_change_item(
    conversation_id: SessionId,
    item_id: String,
    changes: Vec<FileUpdateChange>,
    status: PatchApplyStatus,
    turn_id: String,
    outgoing: &OutgoingMessageSender,
    turn_summary_store: &TurnSummaryStore,
) {
    {
        let mut map = turn_summary_store.lock().await;
        if let Some(summary) = map.get_mut(&conversation_id) {
            summary.file_change_started.remove(&item_id);
        }
    }

    let item = SessionItem::FileChange {
        id: item_id,
        changes,
        status,
    };
    let notification = ItemCompletedNotification {
        session_id: conversation_id.to_string(),
        turn_id,
        item,
    };
    outgoing
        .send_server_notification(ServerNotification::ItemCompleted(notification))
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn complete_command_execution_item(
    conversation_id: SessionId,
    turn_id: String,
    item_id: String,
    command: String,
    cwd: PathBuf,
    process_id: Option<String>,
    command_actions: Vec<V2ParsedCommand>,
    status: CommandExecutionStatus,
    outgoing: &OutgoingMessageSender,
) {
    let item = SessionItem::CommandExecution {
        id: item_id,
        command,
        cwd,
        process_id,
        status,
        command_actions,
        aggregated_output: None,
        exit_code: None,
        duration_ms: None,
    };
    let notification = ItemCompletedNotification {
        session_id: conversation_id.to_string(),
        turn_id,
        item,
    };
    outgoing
        .send_server_notification(ServerNotification::ItemCompleted(notification))
        .await;
}

async fn maybe_emit_raw_response_item_completed(
    conversation_id: SessionId,
    turn_id: &str,
    item: savfox_protocol::models::ResponseItem,
    outgoing: &OutgoingMessageSender,
) {
    let notification = RawResponseItemCompletedNotification {
        session_id: conversation_id.to_string(),
        turn_id: turn_id.to_owned(),
        item,
    };
    outgoing
        .send_server_notification(ServerNotification::RawResponseItemCompleted(notification))
        .await;
}

async fn find_and_remove_turn_summary(
    conversation_id: SessionId,
    turn_summary_store: &TurnSummaryStore,
) -> TurnSummary {
    let mut map = turn_summary_store.lock().await;
    map.remove(&conversation_id).unwrap_or_default()
}

async fn handle_turn_complete(
    conversation_id: SessionId,
    event_turn_id: String,
    outgoing: &OutgoingMessageSender,
    turn_summary_store: &TurnSummaryStore,
) {
    let turn_summary = find_and_remove_turn_summary(conversation_id, turn_summary_store).await;

    let (status, error) = match turn_summary.last_error {
        Some(error) => (TurnStatus::Failed, Some(error)),
        None => (TurnStatus::Completed, None),
    };

    emit_turn_completed_with_status(conversation_id, event_turn_id, status, error, outgoing).await;
}

async fn handle_turn_interrupted(
    conversation_id: SessionId,
    event_turn_id: String,
    outgoing: &OutgoingMessageSender,
    turn_summary_store: &TurnSummaryStore,
) {
    find_and_remove_turn_summary(conversation_id, turn_summary_store).await;

    emit_turn_completed_with_status(
        conversation_id,
        event_turn_id,
        TurnStatus::Interrupted,
        None,
        outgoing,
    )
    .await;
}

async fn handle_session_rollback_failed(
    conversation_id: SessionId,
    message: String,
    pending_rollbacks: &PendingRollbacks,
    outgoing: &OutgoingMessageSender,
) {
    let pending_rollback = {
        let mut map = pending_rollbacks.lock().await;
        map.remove(&conversation_id)
    };

    if let Some(request_id) = pending_rollback {
        outgoing
            .send_error(
                request_id,
                JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: message.clone(),
                    data: None,
                },
            )
            .await;
    }
}

async fn handle_token_count_event(
    conversation_id: SessionId,
    turn_id: String,
    token_count_event: TokenCountEvent,
    outgoing: &OutgoingMessageSender,
) {
    let TokenCountEvent { info, rate_limits } = token_count_event;
    if let Some(token_usage) = info.map(SessionTokenUsage::from) {
        let notification = SessionTokenUsageUpdatedNotification {
            session_id: conversation_id.to_string(),
            turn_id,
            token_usage,
        };
        outgoing
            .send_server_notification(ServerNotification::SessionTokenUsageUpdated(notification))
            .await;
    }
    if let Some(rate_limits) = rate_limits {
        outgoing
            .send_server_notification(ServerNotification::AccountRateLimitsUpdated(
                AccountRateLimitsUpdatedNotification {
                    rate_limits: rate_limits.into(),
                },
            ))
            .await;
    }
}

async fn handle_error(
    conversation_id: SessionId,
    error: TurnError,
    turn_summary_store: &TurnSummaryStore,
) {
    let mut map = turn_summary_store.lock().await;
    map.entry(conversation_id).or_default().last_error = Some(error);
}

async fn on_request_user_input_response(
    event_turn_id: String,
    receiver: oneshot::Receiver<JsonValue>,
    conversation: Arc<SavfoxSession>,
) {
    let response = receiver.await;
    let value = match response {
        Ok(value) => value,
        Err(err) => {
            error!("request failed: {err:?}");
            let empty = CoreRequestUserInputResponse {
                answers: HashMap::new(),
            };
            if let Err(err) = conversation
                .submit(Op::UserInputAnswer {
                    id: event_turn_id,
                    response: empty,
                })
                .await
            {
                error!("failed to submit UserInputAnswer: {err}");
            }
            return;
        }
    };

    let response =
        serde_json::from_value::<ToolRequestUserInputResponse>(value).unwrap_or_else(|err| {
            error!("failed to deserialize ToolRequestUserInputResponse: {err}");
            ToolRequestUserInputResponse {
                answers: HashMap::new(),
            }
        });
    let response = CoreRequestUserInputResponse {
        answers: response
            .answers
            .into_iter()
            .map(|(id, answer)| {
                (
                    id,
                    CoreRequestUserInputAnswer {
                        answers: answer.answers,
                    },
                )
            })
            .collect(),
    };

    if let Err(err) = conversation
        .submit(Op::UserInputAnswer {
            id: event_turn_id,
            response,
        })
        .await
    {
        error!("failed to submit UserInputAnswer: {err}");
    }
}

const REVIEW_FALLBACK_MESSAGE: &str = "Reviewer failed to output a response.";

fn render_review_output_text(output: &ReviewOutputEvent) -> String {
    let mut sections = Vec::new();
    let explanation = output.overall_explanation.trim();
    if !explanation.is_empty() {
        sections.push(explanation.to_owned());
    }
    if !output.findings.is_empty() {
        let findings = format_review_findings_block(&output.findings, None);
        let trimmed = findings.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_owned());
        }
    }
    if sections.is_empty() {
        REVIEW_FALLBACK_MESSAGE.to_owned()
    } else {
        sections.join("\n\n")
    }
}

fn convert_patch_changes(changes: &HashMap<PathBuf, CoreFileChange>) -> Vec<FileUpdateChange> {
    let mut converted: Vec<FileUpdateChange> = changes
        .iter()
        .map(|(path, change)| FileUpdateChange {
            path: path.to_string_lossy().into_owned(),
            kind: map_patch_change_kind(change),
            diff: format_file_change_diff(change),
        })
        .collect();
    converted.sort_by(|a, b| a.path.cmp(&b.path));
    converted
}

fn map_patch_change_kind(change: &CoreFileChange) -> V2PatchChangeKind {
    match change {
        CoreFileChange::Add { .. } => V2PatchChangeKind::Add,
        CoreFileChange::Delete { .. } => V2PatchChangeKind::Delete,
        CoreFileChange::Update { move_path, .. } => V2PatchChangeKind::Update {
            move_path: move_path.clone(),
        },
    }
}

fn format_file_change_diff(change: &CoreFileChange) -> String {
    match change {
        CoreFileChange::Add { content } => content.clone(),
        CoreFileChange::Delete { content } => content.clone(),
        CoreFileChange::Update {
            unified_diff,
            move_path,
        } => {
            if let Some(path) = move_path {
                format!("{unified_diff}\n\nMoved to: {}", path.display())
            } else {
                unified_diff.clone()
            }
        }
    }
}

fn map_file_change_approval_decision(
    decision: FileChangeApprovalDecision,
) -> (ReviewDecision, Option<PatchApplyStatus>) {
    match decision {
        FileChangeApprovalDecision::Accept => (ReviewDecision::Approved, None),
        FileChangeApprovalDecision::AcceptForSession => (ReviewDecision::ApprovedForSession, None),
        FileChangeApprovalDecision::Decline => {
            (ReviewDecision::Denied, Some(PatchApplyStatus::Declined))
        }
        FileChangeApprovalDecision::Cancel => {
            (ReviewDecision::Abort, Some(PatchApplyStatus::Declined))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_file_change_request_approval_response(
    event_turn_id: String,
    conversation_id: SessionId,
    item_id: String,
    changes: Vec<FileUpdateChange>,
    receiver: oneshot::Receiver<JsonValue>,
    savfox: Arc<SavfoxSession>,
    outgoing: Arc<OutgoingMessageSender>,
    turn_summary_store: TurnSummaryStore,
) {
    let response = receiver.await;
    let (decision, completion_status) = match response {
        Ok(value) => {
            let response = serde_json::from_value::<FileChangeRequestApprovalResponse>(value)
                .unwrap_or_else(|err| {
                    error!("failed to deserialize FileChangeRequestApprovalResponse: {err}");
                    FileChangeRequestApprovalResponse {
                        decision: FileChangeApprovalDecision::Decline,
                    }
                });

            let (decision, completion_status) =
                map_file_change_approval_decision(response.decision);
            // Allow EventMsg::PatchApplyEnd to emit ItemCompleted for accepted patches.
            // Only short-circuit on declines/cancels/failures.
            (decision, completion_status)
        }
        Err(err) => {
            error!("request failed: {err:?}");
            (ReviewDecision::Denied, Some(PatchApplyStatus::Failed))
        }
    };

    if let Some(status) = completion_status {
        complete_file_change_item(
            conversation_id,
            item_id,
            changes,
            status,
            event_turn_id.clone(),
            outgoing.as_ref(),
            &turn_summary_store,
        )
        .await;
    }

    if let Err(err) = savfox
        .submit(Op::PatchApproval {
            id: event_turn_id,
            decision,
        })
        .await
    {
        error!("failed to submit PatchApproval: {err}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_command_execution_request_approval_response(
    event_turn_id: String,
    conversation_id: SessionId,
    item_id: String,
    command: String,
    cwd: PathBuf,
    command_actions: Vec<V2ParsedCommand>,
    receiver: oneshot::Receiver<JsonValue>,
    conversation: Arc<SavfoxSession>,
    outgoing: Arc<OutgoingMessageSender>,
) {
    let response = receiver.await;
    let (decision, completion_status) = match response {
        Ok(value) => {
            let response = serde_json::from_value::<CommandExecutionRequestApprovalResponse>(value)
                .unwrap_or_else(|err| {
                    error!("failed to deserialize CommandExecutionRequestApprovalResponse: {err}");
                    CommandExecutionRequestApprovalResponse {
                        decision: CommandExecutionApprovalDecision::Decline,
                    }
                });

            let decision = response.decision;

            let (decision, completion_status) = match decision {
                CommandExecutionApprovalDecision::Accept => (ReviewDecision::Approved, None),
                CommandExecutionApprovalDecision::AcceptForSession => {
                    (ReviewDecision::ApprovedForSession, None)
                }
                CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                    execpolicy_amendment,
                } => (
                    ReviewDecision::ApprovedExecpolicyAmendment {
                        proposed_execpolicy_amendment: execpolicy_amendment.into_core(),
                    },
                    None,
                ),
                CommandExecutionApprovalDecision::Decline => (
                    ReviewDecision::Denied,
                    Some(CommandExecutionStatus::Declined),
                ),
                CommandExecutionApprovalDecision::Cancel => (
                    ReviewDecision::Abort,
                    Some(CommandExecutionStatus::Declined),
                ),
            };
            (decision, completion_status)
        }
        Err(err) => {
            error!("request failed: {err:?}");
            (ReviewDecision::Denied, Some(CommandExecutionStatus::Failed))
        }
    };

    if let Some(status) = completion_status {
        complete_command_execution_item(
            conversation_id,
            event_turn_id.clone(),
            item_id.clone(),
            command.clone(),
            cwd.clone(),
            None,
            command_actions.clone(),
            status,
            outgoing.as_ref(),
        )
        .await;
    }

    if let Err(err) = conversation
        .submit(Op::ExecApproval {
            id: event_turn_id,
            decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}

/// similar to handle_mcp_tool_call_begin in exec
async fn construct_mcp_tool_call_notification(
    begin_event: McpToolCallBeginEvent,
    session_id: String,
    turn_id: String,
) -> ItemStartedNotification {
    let item = SessionItem::McpToolCall {
        id: begin_event.call_id,
        server: begin_event.invocation.server,
        tool: begin_event.invocation.tool,
        status: McpToolCallStatus::InProgress,
        arguments: begin_event.invocation.arguments.unwrap_or(JsonValue::Null),
        result: None,
        error: None,
        duration_ms: None,
    };
    ItemStartedNotification {
        session_id,
        turn_id,
        item,
    }
}

/// similar to handle_mcp_tool_call_end in exec
async fn construct_mcp_tool_call_end_notification(
    end_event: McpToolCallEndEvent,
    session_id: String,
    turn_id: String,
) -> ItemCompletedNotification {
    let status = if end_event.is_success() {
        McpToolCallStatus::Completed
    } else {
        McpToolCallStatus::Failed
    };
    let duration_ms = i64::try_from(end_event.duration.as_millis()).ok();

    let (result, error) = match &end_event.result {
        Ok(value) => (
            Some(McpToolCallResult {
                content: value.content.clone(),
                structured_content: value.structured_content.clone(),
            }),
            None,
        ),
        Err(message) => (
            None,
            Some(McpToolCallError {
                message: message.clone(),
            }),
        ),
    };

    let item = SessionItem::McpToolCall {
        id: end_event.call_id,
        server: end_event.invocation.server,
        tool: end_event.invocation.tool,
        status,
        arguments: end_event.invocation.arguments.unwrap_or(JsonValue::Null),
        result,
        error,
        duration_ms,
    };
    ItemCompletedNotification {
        session_id,
        turn_id,
        item,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use anyhow::{Result, anyhow, bail};
    use pretty_assertions::assert_eq;
    use rmcp::model::Content;
    use savfox_app_server_protocol::TurnPlanStepStatus;
    use savfox_core::protocol::{
        CreditsSnapshot, McpInvocation, RateLimitSnapshot, RateLimitWindow, TokenUsage,
        TokenUsageInfo,
    };
    use savfox_protocol::mcp::CallToolResult;
    use savfox_protocol::plan_tool::{PlanItemArg, StepStatus};
    use serde_json::Value as JsonValue;
    use tokio::sync::{Mutex, mpsc};

    use super::*;
    use crate::CHANNEL_CAPACITY;
    use crate::outgoing_message::{OutgoingMessage, OutgoingMessageSender};

    fn new_turn_summary_store() -> TurnSummaryStore {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn file_change_accept_for_session_maps_to_approved_for_session() {
        let (decision, completion_status) =
            map_file_change_approval_decision(FileChangeApprovalDecision::AcceptForSession);
        assert_eq!(decision, ReviewDecision::ApprovedForSession);
        assert_eq!(completion_status, None);
    }

    #[tokio::test]
    async fn test_handle_error_records_message() -> Result<()> {
        let conversation_id = SessionId::new();
        let turn_summary_store = new_turn_summary_store();

        handle_error(
            conversation_id,
            TurnError {
                message: "boom".to_string(),
                savfox_error_info: Some(V2SavfoxErrorInfo::InternalServerError),
                additional_details: None,
            },
            &turn_summary_store,
        )
        .await;

        let turn_summary = find_and_remove_turn_summary(conversation_id, &turn_summary_store).await;
        assert_eq!(
            turn_summary.last_error,
            Some(TurnError {
                message: "boom".to_string(),
                savfox_error_info: Some(V2SavfoxErrorInfo::InternalServerError),
                additional_details: None,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_turn_complete_emits_completed_without_error() -> Result<()> {
        let conversation_id = SessionId::new();
        let event_turn_id = "complete1".to_string();
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));
        let turn_summary_store = new_turn_summary_store();

        handle_turn_complete(
            conversation_id,
            event_turn_id.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send one notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, event_turn_id);
                assert_eq!(n.turn.status, TurnStatus::Completed);
                assert_eq!(n.turn.error, None);
            }
            other => bail!("unexpected message: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_turn_interrupted_emits_interrupted_with_error() -> Result<()> {
        let conversation_id = SessionId::new();
        let event_turn_id = "interrupt1".to_string();
        let turn_summary_store = new_turn_summary_store();
        handle_error(
            conversation_id,
            TurnError {
                message: "oops".to_string(),
                savfox_error_info: None,
                additional_details: None,
            },
            &turn_summary_store,
        )
        .await;
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));

        handle_turn_interrupted(
            conversation_id,
            event_turn_id.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send one notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, event_turn_id);
                assert_eq!(n.turn.status, TurnStatus::Interrupted);
                assert_eq!(n.turn.error, None);
            }
            other => bail!("unexpected message: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_turn_complete_emits_failed_with_error() -> Result<()> {
        let conversation_id = SessionId::new();
        let event_turn_id = "complete_err1".to_string();
        let turn_summary_store = new_turn_summary_store();
        handle_error(
            conversation_id,
            TurnError {
                message: "bad".to_string(),
                savfox_error_info: Some(V2SavfoxErrorInfo::Other),
                additional_details: None,
            },
            &turn_summary_store,
        )
        .await;
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));

        handle_turn_complete(
            conversation_id,
            event_turn_id.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send one notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, event_turn_id);
                assert_eq!(n.turn.status, TurnStatus::Failed);
                assert_eq!(
                    n.turn.error,
                    Some(TurnError {
                        message: "bad".to_string(),
                        savfox_error_info: Some(V2SavfoxErrorInfo::Other),
                        additional_details: None,
                    })
                );
            }
            other => bail!("unexpected message: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_turn_plan_update_emits_notification_for_v2() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = OutgoingMessageSender::new(tx);
        let update = UpdatePlanArgs {
            explanation: Some("need plan".to_string()),
            plan: vec![
                PlanItemArg {
                    step: "first".to_string(),
                    status: StepStatus::Pending,
                },
                PlanItemArg {
                    step: "second".to_string(),
                    status: StepStatus::Completed,
                },
            ],
        };

        let conversation_id = SessionId::new();

        handle_turn_plan_update(conversation_id, "turn-123", update, &outgoing).await;

        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send one notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnPlanUpdated(n)) => {
                assert_eq!(n.session_id, conversation_id.to_string());
                assert_eq!(n.turn_id, "turn-123");
                assert_eq!(n.explanation.as_deref(), Some("need plan"));
                assert_eq!(n.plan.len(), 2);
                assert_eq!(n.plan[0].step, "first");
                assert_eq!(n.plan[0].status, TurnPlanStepStatus::Pending);
                assert_eq!(n.plan[1].step, "second");
                assert_eq!(n.plan[1].status, TurnPlanStepStatus::Completed);
            }
            other => bail!("unexpected message: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_token_count_event_emits_usage_and_rate_limits() -> Result<()> {
        let conversation_id = SessionId::new();
        let turn_id = "turn-123".to_string();
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));

        let info = TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens: 100,
                cached_input_tokens: 25,
                output_tokens: 50,
                reasoning_output_tokens: 9,
                total_tokens: 200,
            },
            last_token_usage: TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 5,
                output_tokens: 7,
                reasoning_output_tokens: 1,
                total_tokens: 23,
            },
            model_context_window: Some(4096),
        };
        let rate_limits = RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 42.5,
                window_minutes: Some(15),
                resets_at: Some(1700000000),
            }),
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("5".to_string()),
            }),
            plan_type: None,
        };

        handle_token_count_event(
            conversation_id,
            turn_id.clone(),
            TokenCountEvent {
                info: Some(info),
                rate_limits: Some(rate_limits),
            },
            &outgoing,
        )
        .await;

        let first = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("expected usage notification"))?;
        match first {
            OutgoingMessage::AppServerNotification(
                ServerNotification::SessionTokenUsageUpdated(payload),
            ) => {
                assert_eq!(payload.session_id, conversation_id.to_string());
                assert_eq!(payload.turn_id, turn_id);
                let usage = payload.token_usage;
                assert_eq!(usage.total.total_tokens, 200);
                assert_eq!(usage.total.cached_input_tokens, 25);
                assert_eq!(usage.last.output_tokens, 7);
                assert_eq!(usage.model_context_window, Some(4096));
            }
            other => bail!("unexpected notification: {other:?}"),
        }

        let second = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("expected rate limit notification"))?;
        match second {
            OutgoingMessage::AppServerNotification(
                ServerNotification::AccountRateLimitsUpdated(payload),
            ) => {
                assert!(payload.rate_limits.primary.is_some());
                assert!(payload.rate_limits.credits.is_some());
            }
            other => bail!("unexpected notification: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_token_count_event_without_usage_info() -> Result<()> {
        let conversation_id = SessionId::new();
        let turn_id = "turn-456".to_string();
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));

        handle_token_count_event(
            conversation_id,
            turn_id.clone(),
            TokenCountEvent {
                info: None,
                rate_limits: None,
            },
            &outgoing,
        )
        .await;

        assert!(
            rx.try_recv().is_err(),
            "no notifications should be emitted when token usage info is absent"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_construct_mcp_tool_call_begin_notification_with_args() {
        let begin_event = McpToolCallBeginEvent {
            call_id: "call_123".to_string(),
            invocation: McpInvocation {
                server: "savfox".to_string(),
                tool: "list_mcp_resources".to_string(),
                arguments: Some(serde_json::json!({"server": ""})),
            },
        };

        let session_id = SessionId::new().to_string();
        let turn_id = "turn_1".to_string();
        let notification = construct_mcp_tool_call_notification(
            begin_event.clone(),
            session_id.clone(),
            turn_id.clone(),
        )
        .await;

        let expected = ItemStartedNotification {
            session_id,
            turn_id,
            item: SessionItem::McpToolCall {
                id: begin_event.call_id,
                server: begin_event.invocation.server,
                tool: begin_event.invocation.tool,
                status: McpToolCallStatus::InProgress,
                arguments: serde_json::json!({"server": ""}),
                result: None,
                error: None,
                duration_ms: None,
            },
        };

        assert_eq!(notification, expected);
    }

    #[tokio::test]
    async fn test_handle_turn_complete_emits_error_multiple_turns() -> Result<()> {
        // Conversation A will have two turns; Conversation B will have one turn.
        let conversation_a = SessionId::new();
        let conversation_b = SessionId::new();
        let turn_summary_store = new_turn_summary_store();

        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));

        // Turn 1 on conversation A
        let a_turn1 = "a_turn1".to_string();
        handle_error(
            conversation_a,
            TurnError {
                message: "a1".to_string(),
                savfox_error_info: Some(V2SavfoxErrorInfo::BadRequest),
                additional_details: None,
            },
            &turn_summary_store,
        )
        .await;
        handle_turn_complete(
            conversation_a,
            a_turn1.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        // Turn 1 on conversation B
        let b_turn1 = "b_turn1".to_string();
        handle_error(
            conversation_b,
            TurnError {
                message: "b1".to_string(),
                savfox_error_info: None,
                additional_details: None,
            },
            &turn_summary_store,
        )
        .await;
        handle_turn_complete(
            conversation_b,
            b_turn1.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        // Turn 2 on conversation A
        let a_turn2 = "a_turn2".to_string();
        handle_turn_complete(
            conversation_a,
            a_turn2.clone(),
            &outgoing,
            &turn_summary_store,
        )
        .await;

        // Verify: A turn 1
        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send first notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, a_turn1);
                assert_eq!(n.turn.status, TurnStatus::Failed);
                assert_eq!(
                    n.turn.error,
                    Some(TurnError {
                        message: "a1".to_string(),
                        savfox_error_info: Some(V2SavfoxErrorInfo::BadRequest),
                        additional_details: None,
                    })
                );
            }
            other => bail!("unexpected message: {other:?}"),
        }

        // Verify: B turn 1
        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send second notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, b_turn1);
                assert_eq!(n.turn.status, TurnStatus::Failed);
                assert_eq!(
                    n.turn.error,
                    Some(TurnError {
                        message: "b1".to_string(),
                        savfox_error_info: None,
                        additional_details: None,
                    })
                );
            }
            other => bail!("unexpected message: {other:?}"),
        }

        // Verify: A turn 2
        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send third notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnCompleted(n)) => {
                assert_eq!(n.turn.id, a_turn2);
                assert_eq!(n.turn.status, TurnStatus::Completed);
                assert_eq!(n.turn.error, None);
            }
            other => bail!("unexpected message: {other:?}"),
        }

        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }

    #[tokio::test]
    async fn test_construct_mcp_tool_call_begin_notification_without_args() {
        let begin_event = McpToolCallBeginEvent {
            call_id: "call_456".to_string(),
            invocation: McpInvocation {
                server: "savfox".to_string(),
                tool: "list_mcp_resources".to_string(),
                arguments: None,
            },
        };

        let session_id = SessionId::new().to_string();
        let turn_id = "turn_2".to_string();
        let notification = construct_mcp_tool_call_notification(
            begin_event.clone(),
            session_id.clone(),
            turn_id.clone(),
        )
        .await;

        let expected = ItemStartedNotification {
            session_id,
            turn_id,
            item: SessionItem::McpToolCall {
                id: begin_event.call_id,
                server: begin_event.invocation.server,
                tool: begin_event.invocation.tool,
                status: McpToolCallStatus::InProgress,
                arguments: JsonValue::Null,
                result: None,
                error: None,
                duration_ms: None,
            },
        };

        assert_eq!(notification, expected);
    }

    #[tokio::test]
    async fn test_construct_mcp_tool_call_end_notification_success() {
        let content = vec![
            serde_json::to_value(Content::text("{\"resources\":[]}"))
                .expect("content should serialize"),
        ];
        let result = CallToolResult {
            content: content.clone(),
            is_error: Some(false),
            structured_content: None,
            meta: None,
        };

        let end_event = McpToolCallEndEvent {
            call_id: "call_789".to_string(),
            invocation: McpInvocation {
                server: "savfox".to_string(),
                tool: "list_mcp_resources".to_string(),
                arguments: Some(serde_json::json!({"server": ""})),
            },
            duration: Duration::from_nanos(92708),
            result: Ok(result),
        };

        let session_id = SessionId::new().to_string();
        let turn_id = "turn_3".to_string();
        let notification = construct_mcp_tool_call_end_notification(
            end_event.clone(),
            session_id.clone(),
            turn_id.clone(),
        )
        .await;

        let expected = ItemCompletedNotification {
            session_id,
            turn_id,
            item: SessionItem::McpToolCall {
                id: end_event.call_id,
                server: end_event.invocation.server,
                tool: end_event.invocation.tool,
                status: McpToolCallStatus::Completed,
                arguments: serde_json::json!({"server": ""}),
                result: Some(McpToolCallResult {
                    content,
                    structured_content: None,
                }),
                error: None,
                duration_ms: Some(0),
            },
        };

        assert_eq!(notification, expected);
    }

    #[tokio::test]
    async fn test_construct_mcp_tool_call_end_notification_error() {
        let end_event = McpToolCallEndEvent {
            call_id: "call_err".to_string(),
            invocation: McpInvocation {
                server: "savfox".to_string(),
                tool: "list_mcp_resources".to_string(),
                arguments: None,
            },
            duration: Duration::from_millis(1),
            result: Err("boom".to_string()),
        };

        let session_id = SessionId::new().to_string();
        let turn_id = "turn_4".to_string();
        let notification = construct_mcp_tool_call_end_notification(
            end_event.clone(),
            session_id.clone(),
            turn_id.clone(),
        )
        .await;

        let expected = ItemCompletedNotification {
            session_id,
            turn_id,
            item: SessionItem::McpToolCall {
                id: end_event.call_id,
                server: end_event.invocation.server,
                tool: end_event.invocation.tool,
                status: McpToolCallStatus::Failed,
                arguments: JsonValue::Null,
                result: None,
                error: Some(McpToolCallError {
                    message: "boom".to_string(),
                }),
                duration_ms: Some(1),
            },
        };

        assert_eq!(notification, expected);
    }

    #[tokio::test]
    async fn test_handle_turn_diff_emits_v2_notification() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let outgoing = OutgoingMessageSender::new(tx);
        let unified_diff = "--- a\n+++ b\n".to_string();
        let conversation_id = SessionId::new();

        handle_turn_diff(
            conversation_id,
            "turn-1",
            TurnDiffEvent {
                unified_diff: unified_diff.clone(),
            },
            &outgoing,
        )
        .await;

        let msg = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("should send one notification"))?;
        match msg {
            OutgoingMessage::AppServerNotification(ServerNotification::TurnDiffUpdated(
                notification,
            )) => {
                assert_eq!(notification.session_id, conversation_id.to_string());
                assert_eq!(notification.turn_id, "turn-1");
                assert_eq!(notification.diff, unified_diff);
            }
            other => bail!("unexpected message: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra messages expected");
        Ok(())
    }
}
