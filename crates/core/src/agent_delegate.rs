use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_channel::Receiver;
use async_channel::Sender;
use savfox_async_utils::OrCancelExt;
use savfox_protocol::protocol::ApplyPatchApprovalRequestEvent;
use savfox_protocol::protocol::Event;
use savfox_protocol::protocol::EventMsg;
use savfox_protocol::protocol::ExecApprovalRequestEvent;
use savfox_protocol::protocol::Op;
use savfox_protocol::protocol::RequestUserInputEvent;
use savfox_protocol::protocol::SessionSource;
use savfox_protocol::protocol::SubAgentSource;
use savfox_protocol::protocol::Submission;
use savfox_protocol::request_user_input::RequestUserInputArgs;
use savfox_protocol::request_user_input::RequestUserInputResponse;
use savfox_protocol::user_input::UserInput;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::AuthManager;
use crate::config::Config;
use crate::error::SavfoxError;
use crate::models_manager::manager::ModelsManager;
use crate::savfox::Savfox;
use crate::savfox::SavfoxSpawnOk;
use crate::savfox::SUBMISSION_CHANNEL_CAPACITY;
use crate::savfox::Session;
use crate::savfox::TurnContext;
use savfox_protocol::protocol::InitialHistory;

/// Start an interactive sub-Savfox session and return IO channels.
///
/// The returned `events_rx` yields non-approval events emitted by the sub-agent.
/// Approval requests are handled via `parent_session` and are not surfaced.
/// The returned `ops_tx` allows the caller to submit additional `Op`s to the sub-agent.
pub(crate) async fn run_savfox_session_interactive(
    config: Config,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    cancel_token: CancellationToken,
    initial_history: Option<InitialHistory>,
) -> Result<Savfox, SavfoxError> {
    let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    let (tx_ops, rx_ops) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);

    let SavfoxSpawnOk { savfox, .. } = Savfox::spawn(
        config,
        auth_manager,
        models_manager,
        Arc::clone(&parent_session.services.skills_manager),
        initial_history.unwrap_or(InitialHistory::New),
        SessionSource::SubAgent(SubAgentSource::Review),
        parent_session.services.agent_control.clone(),
        Vec::new(),
    )
    .await?;
    let savfox = Arc::new(savfox);

    // Use a child token so parent cancel cascades but we can scope it to this task
    let cancel_token_events = cancel_token.child_token();
    let cancel_token_ops = cancel_token.child_token();

    // Forward events from the sub-agent to the consumer, filtering approvals and
    // routing them to the parent session for decisions.
    let parent_session_clone = Arc::clone(&parent_session);
    let parent_ctx_clone = Arc::clone(&parent_ctx);
    let savfox_for_events = Arc::clone(&savfox);
    tokio::spawn(async move {
        forward_events(
            savfox_for_events,
            tx_sub,
            parent_session_clone,
            parent_ctx_clone,
            cancel_token_events,
        )
        .await;
    });

    // Forward ops from the caller to the sub-agent.
    let savfox_for_ops = Arc::clone(&savfox);
    tokio::spawn(async move {
        forward_ops(savfox_for_ops, rx_ops, cancel_token_ops).await;
    });

    Ok(Savfox {
        next_id: AtomicU64::new(0),
        tx_sub: tx_ops,
        rx_event: rx_sub,
        agent_status: savfox.agent_status.clone(),
        session: Arc::clone(&savfox.session),
    })
}

/// Convenience wrapper for one-time use with an initial prompt.
///
/// Internally calls the interactive variant, then immediately submits the provided input.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_savfox_session_one_shot(
    config: Config,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    input: Vec<UserInput>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    cancel_token: CancellationToken,
    initial_history: Option<InitialHistory>,
) -> Result<Savfox, SavfoxError> {
    // Use a child token so we can stop the delegate after completion without
    // requiring the caller to cancel the parent token.
    let child_cancel = cancel_token.child_token();
    let io = run_savfox_session_interactive(
        config,
        auth_manager,
        models_manager,
        parent_session,
        parent_ctx,
        child_cancel.clone(),
        initial_history,
    )
    .await?;

    // Send the initial input to kick off the one-shot turn.
    io.submit(Op::UserInput {
        items: input,
        final_output_json_schema: None,
    })
    .await?;

    // Bridge events so we can observe completion and shut down automatically.
    let (tx_bridge, rx_bridge) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    let ops_tx = io.tx_sub.clone();
    let agent_status = io.agent_status.clone();
    let session = Arc::clone(&io.session);
    let io_for_bridge = io;
    tokio::spawn(async move {
        while let Ok(event) = io_for_bridge.next_event().await {
            let should_shutdown = matches!(
                event.msg,
                EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)
            );
            let _ = tx_bridge.send(event).await;
            if should_shutdown {
                let _ = ops_tx
                    .send(Submission {
                        id: "shutdown".to_string(),
                        op: Op::Shutdown {},
                    })
                    .await;
                child_cancel.cancel();
                break;
            }
        }
    });

    // For one-shot usage, return a closed `tx_sub` so callers cannot submit
    // additional ops after the initial request. Create a channel and drop the
    // receiver to close it immediately.
    let (tx_closed, rx_closed) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
    drop(rx_closed);

    Ok(Savfox {
        next_id: AtomicU64::new(0),
        rx_event: rx_bridge,
        tx_sub: tx_closed,
        agent_status,
        session,
    })
}

async fn forward_events(
    savfox: Arc<Savfox>,
    tx_sub: Sender<Event>,
    parent_session: Arc<Session>,
    parent_ctx: Arc<TurnContext>,
    cancel_token: CancellationToken,
) {
    let cancelled = cancel_token.cancelled();
    tokio::pin!(cancelled);

    loop {
        tokio::select! {
            _ = &mut cancelled => {
                shutdown_delegate(&savfox).await;
                break;
            }
            event = savfox.next_event() => {
                let event = match event {
                    Ok(event) => event,
                    Err(_) => break,
                };
                match event {
                    // ignore all legacy delta events
                    Event {
                        id: _,
                        msg: EventMsg::AgentMessageDelta(_) | EventMsg::AgentReasoningDelta(_),
                    } => {}
                    Event {
                        id: _,
                        msg: EventMsg::TokenCount(_),
                    } => {}
                    Event {
                        id: _,
                        msg: EventMsg::SessionConfigured(_),
                    } => {}
                    Event {
                        id: _,
                        msg: EventMsg::SessionNameUpdated(_),
                    } => {}
                    Event {
                        id,
                        msg: EventMsg::ExecApprovalRequest(event),
                    } => {
                        // Initiate approval via parent session; do not surface to consumer.
                        handle_exec_approval(
                            &savfox,
                            id,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        id,
                        msg: EventMsg::ApplyPatchApprovalRequest(event),
                    } => {
                        handle_patch_approval(
                            &savfox,
                            id,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    Event {
                        id,
                        msg: EventMsg::RequestUserInput(event),
                    } => {
                        handle_request_user_input(
                            &savfox,
                            id,
                            &parent_session,
                            &parent_ctx,
                            event,
                            &cancel_token,
                        )
                        .await;
                    }
                    other => {
                        match tx_sub.send(other).or_cancel(&cancel_token).await {
                            Ok(Ok(())) => {}
                            _ => {
                                shutdown_delegate(&savfox).await;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Ask the delegate to stop and drain its events so background sends do not hit a closed channel.
async fn shutdown_delegate(savfox: &Savfox) {
    let _ = savfox.submit(Op::Interrupt).await;
    let _ = savfox.submit(Op::Shutdown {}).await;

    let _ = timeout(Duration::from_millis(500), async {
        while let Ok(event) = savfox.next_event().await {
            if matches!(
                event.msg,
                EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_)
            ) {
                break;
            }
        }
    })
    .await;
}

/// Forward ops from a caller to a sub-agent, respecting cancellation.
async fn forward_ops(
    savfox: Arc<Savfox>,
    rx_ops: Receiver<Submission>,
    cancel_token_ops: CancellationToken,
) {
    loop {
        let op: Op = match rx_ops.recv().or_cancel(&cancel_token_ops).await {
            Ok(Ok(Submission { id: _, op })) => op,
            Ok(Err(_)) | Err(_) => break,
        };
        let _ = savfox.submit(op).await;
    }
}

/// Handle an ExecApprovalRequest by consulting the parent session and replying.
async fn handle_exec_approval(
    savfox: &Savfox,
    id: String,
    parent_session: &Session,
    parent_ctx: &TurnContext,
    event: ExecApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    // Race approval with cancellation and timeout to avoid hangs.
    let approval_fut = parent_session.request_command_approval(
        parent_ctx,
        parent_ctx.sub_id.clone(),
        event.command,
        event.cwd,
        event.reason,
        event.proposed_execpolicy_amendment,
    );
    let decision = await_approval_with_cancel(
        approval_fut,
        parent_session,
        &parent_ctx.sub_id,
        cancel_token,
    )
    .await;

    let _ = savfox.submit(Op::ExecApproval { id, decision }).await;
}

/// Handle an ApplyPatchApprovalRequest by consulting the parent session and replying.
async fn handle_patch_approval(
    savfox: &Savfox,
    id: String,
    parent_session: &Session,
    parent_ctx: &TurnContext,
    event: ApplyPatchApprovalRequestEvent,
    cancel_token: &CancellationToken,
) {
    let decision_rx = parent_session
        .request_patch_approval(
            parent_ctx,
            parent_ctx.sub_id.clone(),
            event.changes,
            event.reason,
            event.grant_root,
        )
        .await;
    let decision = await_approval_with_cancel(
        async move { decision_rx.await.unwrap_or_default() },
        parent_session,
        &parent_ctx.sub_id,
        cancel_token,
    )
    .await;
    let _ = savfox.submit(Op::PatchApproval { id, decision }).await;
}

async fn handle_request_user_input(
    savfox: &Savfox,
    id: String,
    parent_session: &Session,
    parent_ctx: &TurnContext,
    event: RequestUserInputEvent,
    cancel_token: &CancellationToken,
) {
    let args = RequestUserInputArgs {
        questions: event.questions,
    };
    let response_fut =
        parent_session.request_user_input(parent_ctx, parent_ctx.sub_id.clone(), args);
    let response = await_user_input_with_cancel(
        response_fut,
        parent_session,
        &parent_ctx.sub_id,
        cancel_token,
    )
    .await;
    let _ = savfox.submit(Op::UserInputAnswer { id, response }).await;
}

async fn await_user_input_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    sub_id: &str,
    cancel_token: &CancellationToken,
) -> RequestUserInputResponse
where
    F: core::future::Future<Output = Option<RequestUserInputResponse>>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            let empty = RequestUserInputResponse {
                answers: HashMap::new(),
            };
            parent_session
                .notify_user_input_response(sub_id, empty.clone())
                .await;
            empty
        }
        response = fut => response.unwrap_or_else(|| RequestUserInputResponse {
            answers: HashMap::new(),
        }),
    }
}

/// Await an approval decision, aborting on cancellation.
async fn await_approval_with_cancel<F>(
    fut: F,
    parent_session: &Session,
    sub_id: &str,
    cancel_token: &CancellationToken,
) -> savfox_protocol::protocol::ReviewDecision
where
    F: core::future::Future<Output = savfox_protocol::protocol::ReviewDecision>,
{
    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            parent_session
                .notify_approval(sub_id, savfox_protocol::protocol::ReviewDecision::Abort)
                .await;
            savfox_protocol::protocol::ReviewDecision::Abort
        }
        decision = fut => {
            decision
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_channel::bounded;
    use savfox_protocol::models::ResponseItem;
    use savfox_protocol::protocol::AgentStatus;
    use savfox_protocol::protocol::RawResponseItemEvent;
    use savfox_protocol::protocol::TurnAbortReason;
    use savfox_protocol::protocol::TurnAbortedEvent;
    use pretty_assertions::assert_eq;
    use tokio::sync::watch;

    #[tokio::test]
    async fn forward_events_cancelled_while_send_blocked_shuts_down_delegate() {
        let (tx_events, rx_events) = bounded(1);
        let (tx_sub, rx_sub) = bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
        let (session, ctx, _rx_evt) = crate::savfox::make_session_and_context_with_rx().await;
        let savfox = Arc::new(Savfox {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event: rx_events,
            agent_status,
            session: Arc::clone(&session),
        });

        let (tx_out, rx_out) = bounded(1);
        tx_out
            .send(Event {
                id: "full".to_string(),
                msg: EventMsg::TurnAborted(TurnAbortedEvent {
                    reason: TurnAbortReason::Interrupted,
                }),
            })
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let forward = tokio::spawn(forward_events(
            Arc::clone(&savfox),
            tx_out.clone(),
            session,
            ctx,
            cancel.clone(),
        ));

        tx_events
            .send(Event {
                id: "evt".to_string(),
                msg: EventMsg::RawResponseItem(RawResponseItemEvent {
                    item: ResponseItem::CustomToolCall {
                        id: None,
                        status: None,
                        call_id: "call-1".to_string(),
                        name: "tool".to_string(),
                        input: "{}".to_string(),
                    },
                }),
            })
            .await
            .unwrap();

        drop(tx_events);
        cancel.cancel();
        timeout(std::time::Duration::from_millis(1000), forward)
            .await
            .expect("forward_events hung")
            .expect("forward_events join error");

        let received = rx_out.recv().await.expect("prefilled event missing");
        assert_eq!("full", received.id);
        let mut ops = Vec::new();
        while let Ok(sub) = rx_sub.try_recv() {
            ops.push(sub.op);
        }
        assert!(
            ops.iter().any(|op| matches!(op, Op::Interrupt)),
            "expected Interrupt op after cancellation"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::Shutdown)),
            "expected Shutdown op after cancellation"
        );
    }
}
