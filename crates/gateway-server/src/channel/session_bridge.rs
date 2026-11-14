use std::path::PathBuf;

use savfox_core::config::Config;
use savfox_core::{
    SESSIONS_SUBDIR, find_archived_session_path_by_id_str, find_session_path_by_id_str,
};
use savfox_protocol::SessionId;
use tracing::{debug, warn};

use super::{AgentInvocationResult, GatewayChannel, ResolvedAgentSession};
use crate::runtime::routing::sse_stream::{StreamCancellation, run_cancellable};
use crate::security::approval_coordinator::{
    ApprovalCoordinator, ApprovalNotification, ApprovalRequestMetadata, ChannelApprovalKind,
    ChannelApprovalScope, PendingChannelApproval,
};
use crate::security::execution_policy::{ExecutionMode, ResolvedExecutionSecurity};

// Windows tree termination may take 10s; user-shell cooperative cleanup has a
// 20s grace period. Observe the complete core shutdown before releasing lifecycle work.
const CORE_SESSION_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy)]
enum PendingApprovalKind {
    Exec,
    Patch,
}

struct PendingApproval {
    core_id: String,
    request_id: Option<String>,
    kind: PendingApprovalKind,
    summary: String,
}

impl PendingApproval {
    fn abort_op(&self) -> savfox_protocol::protocol::Op {
        use savfox_protocol::protocol::{Op, ReviewDecision};

        match self.kind {
            PendingApprovalKind::Exec => Op::ExecApproval {
                id: self.core_id.clone(),
                decision: ReviewDecision::Abort,
            },
            PendingApprovalKind::Patch => Op::PatchApproval {
                id: self.core_id.clone(),
                decision: ReviewDecision::Abort,
            },
        }
    }
}

fn approval_reply_instructions(pending: &PendingChannelApproval) -> String {
    let mut actions = vec![
        format!("approve:{}", pending.request_id),
        format!("deny:{}", pending.request_id),
        format!("abort:{}", pending.request_id),
    ];
    if pending.supports_session_grant() {
        actions.insert(1, format!("approve-session:{}", pending.request_id));
    }
    if pending.supports_persisted_rule() {
        let index = actions.len().saturating_sub(2);
        actions.insert(index, format!("allow-rule:{}", pending.request_id));
    }
    format!("Reply with one of: {}.", actions.join(", "))
}

async fn remove_invocation_approvals(coordinator: &ApprovalCoordinator, request_ids: &[String]) {
    for request_id in request_ids {
        coordinator.remove(request_id).await;
    }
}

impl GatewayChannel {
    /// Invoke the agent with a text prompt and return the response text.
    ///
    /// Creates a temporary thread, submits the user message as a `UserInput` Op,
    /// and collects the assistant's text reply by reading `Event` messages.
    /// Used by OpenAI-compatible and OpenResponses API endpoints.
    pub(crate) async fn invoke_agent_text(
        &self,
        prompt: &str,
        model: &str,
    ) -> anyhow::Result<String> {
        let result = self
            .invoke_agent_text_in_session_with_metadata_impl(
                prompt,
                model,
                None,
                |_| {},
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await?;
        Ok(result.reply)
    }

    /// Invoke the agent with an optional persisted session_id and return response text.
    pub(crate) async fn invoke_agent_text_in_session(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let result = self
            .invoke_agent_text_in_session_with_metadata_impl(
                prompt,
                model,
                session_id,
                |_| {},
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await?;
        Ok(result.reply)
    }

    /// Invoke the agent with optional persisted session context and stream text deltas.
    pub(crate) async fn invoke_agent_text_in_session_stream<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        on_delta: F,
        cancellation: StreamCancellation,
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt,
            model,
            session_id,
            on_delta,
            None,
            false,
            None,
            None,
            None,
            Some(cancellation),
        )
        .await
    }

    /// Invoke the agent and include thread metadata for history tracking.
    pub(crate) async fn invoke_agent_text_with_metadata(
        &self,
        prompt: &str,
        model: &str,
    ) -> anyhow::Result<AgentInvocationResult> {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt,
            model,
            None,
            |_| {},
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Invoke the agent with optional persisted session context and include thread metadata.
    pub(crate) async fn invoke_agent_text_in_session_with_metadata(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<AgentInvocationResult> {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt,
            model,
            session_id,
            |_| {},
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Invoke agent with approval notification support for channel messages.
    /// When a tool needs approval, `on_approval` is called with a user-facing
    /// message and the timeout is extended to give the user time to respond.
    ///
    /// Uses concurrent fork mode: if the session is already processing a
    /// previous message, a new independent invocation is forked so the user
    /// does not have to wait.
    pub(crate) async fn invoke_agent_text_in_session_with_approval<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        security: ResolvedExecutionSecurity,
        approval_scope: ChannelApprovalScope,
        on_delta: F,
        on_approval: Box<dyn FnMut(ApprovalNotification) + Send>,
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt,
            model,
            session_id,
            on_delta,
            Some(on_approval),
            true, // concurrent_fork: channel messages use fork-on-busy
            Some(security),
            Some(approval_scope),
            None,
            None,
        )
        .await
    }

    pub(crate) async fn invoke_agent_text_in_session_with_approval_and_context<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        security: ResolvedExecutionSecurity,
        approval_scope: ChannelApprovalScope,
        trusted_developer_context: Option<String>,
        on_delta: F,
        on_approval: Box<dyn FnMut(ApprovalNotification) + Send>,
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt,
            model,
            session_id,
            on_delta,
            Some(on_approval),
            true,
            Some(security),
            Some(approval_scope),
            trusted_developer_context,
            None,
        )
        .await
    }

    async fn invoke_agent_text_in_session_with_metadata_impl<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        mut on_delta: F,
        mut on_approval: Option<Box<dyn FnMut(ApprovalNotification) + Send>>,
        concurrent_fork: bool,
        security: Option<ResolvedExecutionSecurity>,
        approval_scope: Option<ChannelApprovalScope>,
        trusted_developer_context: Option<String>,
        cancellation: Option<StreamCancellation>,
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        use savfox_protocol::protocol::{EventMsg, Op, ReviewDecision};
        use savfox_protocol::user_input::UserInput;

        // Strip "[user]:" prefix if present (some clients add this prefix)
        let prompt = prompt
            .strip_prefix("[user]:")
            .map(|s| s.trim())
            .unwrap_or(prompt.trim());
        if let Some(failure) = cancellation.as_ref().and_then(StreamCancellation::reason) {
            return Err(anyhow::anyhow!("{failure}"));
        }

        let (
            mut config,
            execution_mode,
            approval_capabilities,
            approval_agent_id,
            policy_fingerprint,
        ) = match security {
            Some(security) => (
                security.config,
                security.context.mode,
                security.context.capabilities,
                security.context.agent_id,
                security.context.policy_fingerprint,
            ),
            None => (
                (*self.config).clone(),
                ExecutionMode::Unattended,
                Default::default(),
                String::new(),
                String::new(),
            ),
        };
        let execution_mode = if execution_mode == ExecutionMode::Interactive
            && (on_approval.is_none() || approval_scope.is_none())
        {
            warn!("interactive execution requested without a correlated approval response channel");
            ExecutionMode::Unattended
        } else {
            execution_mode
        };
        let model = model.trim();
        // Only override the config model when a real model slug is provided.
        // The callers often pass "default" to mean "use the default agent",
        // which should NOT replace the configured model name.
        if !model.is_empty() && model != "default" {
            config.model = Some(model.to_owned());
        }
        if let Some(context) = trusted_developer_context.as_deref() {
            let combined = match config.developer_instructions.take() {
                Some(existing) if !existing.trim().is_empty() => {
                    format!("{existing}\n\n{context}")
                }
                _ => context.to_owned(),
            };
            config.developer_instructions = Some(combined);
        }
        let approval_cwd = config.cwd.clone();

        let requested_session_id = session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        // A trusted remote envelope is turn-specific. Rehydrate the same
        // rollout with the refreshed developer context rather than leaving a
        // prior sender/event envelope attached to an active core session.
        if trusted_developer_context.is_some()
            && let Some(logical_id) = requested_session_id.as_deref()
        {
            self.invalidate_logical_session_security(logical_id).await;
        }
        if let Some(sid) = requested_session_id.as_deref() {
            let parsed = uuid::Uuid::parse_str(sid)
                .map_err(|err| anyhow::anyhow!("invalid session_id '{sid}': {err}"))?;
            if parsed.get_version_num() != 7 {
                return Err(anyhow::anyhow!(
                    "invalid session_id '{sid}': UUID v7 required"
                ));
            }
        }

        let claude_execution = if self
            .resolve_terminal_delegate(model)
            .await?
            .is_some_and(|delegate| delegate.is_claude_stream())
        {
            let scope = approval_scope.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Claude Channel execution requires an authenticated peer scope")
            })?;
            let owner = format!(
                "channel:{}",
                crate::claude_code::digest(&serde_json::to_vec(
                    &serde_json::json!({"channel":scope.channel,"instance":scope.channel_config_id,"account":scope.account_id,"peer":scope.peer_id})
                )?)
            );
            let logical_id = requested_session_id
                .clone()
                .unwrap_or_else(|| scope.logical_session_id.clone());
            Some(crate::claude_code::Execution {
                owner,
                // This caller has no stable upstream event ID. Identical text
                // on a later turn is a new request, not a cached response.
                request_id: uuid::Uuid::now_v7().to_string(),
                session_id: logical_id,
                agent_id: model.to_owned(),
                approval_client: false,
                trusted_context: trusted_developer_context.clone(),
            })
        } else {
            None
        };
        let terminal_session_id = claude_execution
            .as_ref()
            .map(|execution| execution.session_id.as_str())
            .or(requested_session_id.as_deref());
        if let Some(invocation) = self
            .invoke_terminal_delegate_with_execution(
                prompt,
                model,
                model,
                terminal_session_id,
                &[],
                None,
                claude_execution.clone(),
            )
            .await?
        {
            on_delta(&invocation.result.reply);
            // Terminal delegates retain their own process/session lifecycle.
            // Closing HTTP must not terminate a persistent managed terminal.
            if let Some(failure) = cancellation.as_ref().and_then(StreamCancellation::reason) {
                return Err(anyhow::anyhow!("{failure}"));
            }
            return Ok(invocation.result);
        }

        let cancellable = cancellation.is_some();
        let resolved_session = if cancellable {
            // A stream owns this exact core session. Never interrupt a shared
            // logical session: another HTTP/WS turn may already be using it.
            let history = match requested_session_id.as_deref() {
                Some(logical_id) => self.resolve_session_rollout_path(logical_id).await?,
                None => None,
            };
            self.fork_concurrent_session(config, history).await?
        } else if concurrent_fork {
            self.resolve_agent_session_concurrent(config, requested_session_id.as_deref())
                .await?
        } else {
            self.resolve_agent_session(config, requested_session_id.as_deref())
                .await?
        };
        let session_id = resolved_session.session_id;

        let session = self
            .session_manager
            .get_session(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("failed to get session: {e}"))?;
        let rollout_path = session.rollout_path();

        let mut submitted_turn_id = None;
        let mut invocation_approval_ids = Vec::new();
        let invocation = async {
            // Persist the logical-session → thread/rollout linkage *before* running
            // the turn. The agent runs in a freshly minted thread whose id differs
            // from the caller's logical session id, and the rollout file is named
            // after the thread id. If we only recorded this linkage on success
            // (as the HTTP handlers do), a turn that errors or times out would
            // leave the web session entry with no `session_file`, so `chat.history`
            // could not locate the rollout and the conversation would vanish on
            // refresh. Recording it up front keeps history recoverable regardless
            // of how the turn ends.
            if let Some(logical_id) = requested_session_id.as_deref() {
                crate::chat_session::persist_session_thread_link(
                    self,
                    &self.session_store,
                    &self.config.savfox_home,
                    logical_id,
                    &session_id.to_string(),
                    rollout_path.as_deref(),
                    None,
                )
                .await;
            }

            // Submit the user message.
            let user_input = UserInput::Text {
                text: prompt.to_owned(),
                text_elements: Vec::new(),
            };
            let turn_id = session
                .submit(Op::UserInput {
                    items: vec![user_input],
                    final_output_json_schema: None,
                })
                .await
                .map_err(|e| anyhow::anyhow!("failed to submit message: {e}"))?;
            submitted_turn_id = Some(turn_id);

            // Collect assistant output by reading Events from the session.
            let mut reply = String::new();
            let mut fallback_agent_reply = String::new();
            let mut saw_delta = false;
            let mut saw_content_delta_event = false;
            let mut last_token_usage: Option<savfox_protocol::protocol::TokenUsage> = None;
            let mut pending_approval: Option<PendingApproval> = None;
            let normal_timeout = tokio::time::Duration::from_secs(120);
            let approval_timeout = tokio::time::Duration::from_secs(180);
            let mut deadline = tokio::time::Instant::now() + normal_timeout;

            loop {
                match tokio::time::timeout_at(deadline, session.next_event()).await {
                    Ok(Ok(event)) => {
                        if let Some(pending) = pending_approval.as_ref()
                            && let Some(request_id) = pending.request_id.as_deref()
                            && !self.approval_coordinator().is_pending(request_id).await
                        {
                            pending_approval = None;
                            deadline = tokio::time::Instant::now() + normal_timeout;
                        }
                        match &event.msg {
                            EventMsg::TokenCount(token_count) => {
                                if let Some(info) = &token_count.info {
                                    last_token_usage = Some(info.last_token_usage.clone());
                                }
                            }
                            EventMsg::AgentMessage(msg) => {
                                if msg.message.is_empty() {
                                    continue;
                                }
                                // Some providers emit only AgentMessage (no deltas).
                                // Buffer this as a fallback and emit only if no deltas arrived.
                                if !saw_delta {
                                    fallback_agent_reply.push_str(&msg.message);
                                }
                            }
                            EventMsg::AgentMessageDelta(delta) => {
                                // If modern content-delta events are present, skip legacy mirrors.
                                if saw_content_delta_event {
                                    continue;
                                }
                                if delta.delta.is_empty() {
                                    continue;
                                }
                                saw_delta = true;
                                reply.push_str(&delta.delta);
                                on_delta(&delta.delta);
                            }
                            EventMsg::AgentMessageContentDelta(delta) => {
                                if delta.delta.is_empty() {
                                    continue;
                                }
                                saw_content_delta_event = true;
                                saw_delta = true;
                                reply.push_str(&delta.delta);
                                on_delta(&delta.delta);
                            }
                            EventMsg::ExecApprovalRequest(req) => {
                                let approval_id = if req.turn_id.trim().is_empty() {
                                    event.id.clone()
                                } else {
                                    req.turn_id.clone()
                                };
                                if execution_mode == ExecutionMode::Unattended {
                                    warn!(
                                        session_id = %session_id,
                                        approval_id,
                                        "denying exec boundary request for unattended entry point"
                                    );
                                    session
                                        .submit(Op::ExecApproval {
                                            id: approval_id,
                                            decision: ReviewDecision::Denied,
                                        })
                                        .await
                                        .map_err(|error| {
                                            anyhow::anyhow!(
                                                "failed to deny unattended exec approval: {error}"
                                            )
                                        })?;
                                    continue;
                                }

                                if let Some(previous) = pending_approval.take() {
                                    if let Some(request_id) = previous.request_id.as_deref() {
                                        self.approval_coordinator().remove(request_id).await;
                                    }
                                    let _ = session.submit(previous.abort_op()).await;
                                }
                                let registered = match self
                                    .approval_coordinator()
                                    .register(
                                        approval_id.clone(),
                                        session_id,
                                        approval_scope
                                            .clone()
                                            .expect("interactive mode requires approval scope"),
                                        ChannelApprovalKind::Exec {
                                            proposed_execpolicy_amendment: req
                                                .proposed_execpolicy_amendment
                                                .clone(),
                                        },
                                        approval_capabilities,
                                        ApprovalRequestMetadata {
                                            agent_id: approval_agent_id.clone(),
                                            policy_fingerprint: policy_fingerprint.clone(),
                                            cwd: req.cwd.clone(),
                                            summary: req.command.join(" "),
                                            command: Some(req.command.clone()),
                                            reason: req.reason.clone(),
                                        },
                                    )
                                    .await
                                {
                                    Ok(registered) => registered,
                                    Err(error) => {
                                        let _ = session
                                            .submit(Op::ExecApproval {
                                                id: approval_id,
                                                decision: ReviewDecision::Denied,
                                            })
                                            .await;
                                        return Err(anyhow::anyhow!(
                                            "failed to register durable exec approval: {error}"
                                        ));
                                    }
                                };
                                let request_id = registered.request_id.clone();
                                invocation_approval_ids.push(request_id.clone());
                                pending_approval = Some(PendingApproval {
                                    core_id: approval_id,
                                    request_id: Some(request_id.clone()),
                                    kind: PendingApprovalKind::Exec,
                                    summary: format!("exec approval {request_id}"),
                                });
                                if let Some(ref mut notify) = on_approval {
                                    let cmd = &registered.envelope.redacted_summary;
                                    let reason = registered
                                        .envelope
                                        .reason
                                        .as_deref()
                                        .map(|r| format!("\nReason: {r}"))
                                        .unwrap_or_default();
                                    let instructions = approval_reply_instructions(&registered);
                                    let msg = format!(
                                        "[Approval Required]\nRequest: {request_id}\nCommand: {cmd}{reason}\n\n{instructions}"
                                    );
                                    notify(registered.notification(msg));
                                    deadline = tokio::time::Instant::now() + approval_timeout;
                                }
                            }
                            EventMsg::ApplyPatchApprovalRequest(req) => {
                                let approval_id = if req.turn_id.trim().is_empty() {
                                    event.id.clone()
                                } else {
                                    req.turn_id.clone()
                                };
                                if execution_mode == ExecutionMode::Unattended {
                                    warn!(
                                        session_id = %session_id,
                                        approval_id,
                                        "denying patch boundary request for unattended entry point"
                                    );
                                    session
                                        .submit(Op::PatchApproval {
                                            id: approval_id,
                                            decision: ReviewDecision::Denied,
                                        })
                                        .await
                                        .map_err(|error| {
                                            anyhow::anyhow!(
                                                "failed to deny unattended patch approval: {error}"
                                            )
                                        })?;
                                    continue;
                                }

                                if let Some(previous) = pending_approval.take() {
                                    if let Some(request_id) = previous.request_id.as_deref() {
                                        self.approval_coordinator().remove(request_id).await;
                                    }
                                    let _ = session.submit(previous.abort_op()).await;
                                }
                                let registered = match self
                                    .approval_coordinator()
                                    .register(
                                        approval_id.clone(),
                                        session_id,
                                        approval_scope
                                            .clone()
                                            .expect("interactive mode requires approval scope"),
                                        ChannelApprovalKind::Patch,
                                        approval_capabilities,
                                        ApprovalRequestMetadata {
                                            agent_id: approval_agent_id.clone(),
                                            policy_fingerprint: policy_fingerprint.clone(),
                                            cwd: approval_cwd.clone(),
                                            summary: req
                                                .changes
                                                .keys()
                                                .map(|path| path.display().to_string())
                                                .collect::<Vec<_>>()
                                                .join(", "),
                                            command: None,
                                            reason: req.reason.clone(),
                                        },
                                    )
                                    .await
                                {
                                    Ok(registered) => registered,
                                    Err(error) => {
                                        let _ = session
                                            .submit(Op::PatchApproval {
                                                id: approval_id,
                                                decision: ReviewDecision::Denied,
                                            })
                                            .await;
                                        return Err(anyhow::anyhow!(
                                            "failed to register durable patch approval: {error}"
                                        ));
                                    }
                                };
                                let request_id = registered.request_id.clone();
                                invocation_approval_ids.push(request_id.clone());
                                pending_approval = Some(PendingApproval {
                                    core_id: approval_id,
                                    request_id: Some(request_id.clone()),
                                    kind: PendingApprovalKind::Patch,
                                    summary: format!("patch approval {request_id}"),
                                });
                                if let Some(ref mut notify) = on_approval {
                                    let files = &registered.envelope.redacted_summary;
                                    let reason = registered
                                        .envelope
                                        .reason
                                        .as_deref()
                                        .map(|r| format!("\nReason: {r}"))
                                        .unwrap_or_default();
                                    let instructions = approval_reply_instructions(&registered);
                                    let msg = format!(
                                        "[Approval Required]\nRequest: {request_id}\nFile changes: {files}{reason}\n\n{instructions}",
                                    );
                                    notify(registered.notification(msg));
                                    deadline = tokio::time::Instant::now() + approval_timeout;
                                }
                            }
                            EventMsg::TurnComplete(_) => {
                                break;
                            }
                            EventMsg::Error(err) => {
                                return Err(anyhow::anyhow!("agent error: {}", err.message));
                            }
                            _ => {
                                // Skip other events (tool calls, reasoning, etc.).
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        return Err(anyhow::anyhow!("thread error: {e}"));
                    }
                    Err(_) => {
                        // Timeout.
                        if let Some(pending) = pending_approval.take() {
                            warn!(
                                session_id = %session_id,
                                pending_approval = %pending.summary,
                                "agent invocation timed out while waiting for approval"
                            );
                            if let Some(request_id) = pending.request_id.as_deref() {
                                self.approval_coordinator().remove(request_id).await;
                            }
                            let _ = session.submit(pending.abort_op()).await;
                            return Err(anyhow::anyhow!(
                                "agent invocation timed out while waiting for {}",
                                pending.summary
                            ));
                        }
                        warn!(session_id = %session_id, "agent invocation timed out");
                        return Err(anyhow::anyhow!(
                            "agent invocation timed out; response is incomplete"
                        ));
                    }
                }
            }

            if let Some(pending) = pending_approval.take() {
                if let Some(request_id) = pending.request_id.as_deref() {
                    self.approval_coordinator().remove(request_id).await;
                }
                let _ = session.submit(pending.abort_op()).await;
            }
            remove_invocation_approvals(self.approval_coordinator(), &invocation_approval_ids)
                .await;

            if !saw_delta && !fallback_agent_reply.is_empty() {
                reply.push_str(&fallback_agent_reply);
                on_delta(&fallback_agent_reply);
            }

            if reply.is_empty() {
                reply = "(no response from agent)".to_owned();
            }

            Ok(AgentInvocationResult {
                reply,
                session_id: requested_session_id.unwrap_or_else(|| session_id.to_string()),
                thread_id: session_id.to_string(),
                rollout_path,
                last_token_usage,
                lifecycle_generation: None,
            })
        };

        let result = if let Some(cancellation) = cancellation.as_ref() {
            run_cancellable(cancellation, invocation, async {
                // Remove approvals even when cancellation dropped the collector
                // while it was waiting for a decision. Shutdown aborts the turn,
                // terminates its processes and flushes the rollout recorder.
                self.approval_coordinator()
                    .remove_for_core_session(session_id)
                    .await;
                if session.submit(Op::Shutdown).await.is_ok() {
                    let shutdown = async {
                        while let Ok(event) = session.next_event().await {
                            if matches!(event.msg, EventMsg::ShutdownComplete) {
                                break;
                            }
                        }
                    };
                    if tokio::time::timeout(CORE_SESSION_SHUTDOWN_TIMEOUT, shutdown)
                        .await
                        .is_err()
                    {
                        warn!(%session_id, "timed out waiting for SSE invocation shutdown");
                    }
                }
                let _ = self.session_manager.remove_session(&session_id).await;
            })
            .await
            .map_err(|failure| anyhow::anyhow!("{failure}"))?
        } else {
            invocation.await
        };

        // Always clean up errors, including early submission/approval failures.
        // The queued turn-specific interrupt cannot abort a later turn that has
        // already replaced this invocation on a shared persistent session.
        if result.is_err() {
            if let Some(turn_id) = submitted_turn_id.as_deref() {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    session.interrupt_turn(turn_id),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        warn!(%session_id, %error, "failed to interrupt incomplete invocation")
                    }
                    Err(_) => warn!(%session_id, "timed out interrupting incomplete invocation"),
                }
            }
            remove_invocation_approvals(self.approval_coordinator(), &invocation_approval_ids)
                .await;
        }
        if resolved_session.cleanup_after_turn {
            // Temporary/forked sessions belong solely to this invocation.
            let _ = session.submit(Op::Shutdown).await;
            let _ = self.session_manager.remove_session(&session_id).await;
        }
        result
    }

    pub(crate) async fn resolve_agent_session(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
    ) -> anyhow::Result<ResolvedAgentSession> {
        // Keep the core startup state machine out of callers' futures. Two
        // concurrent resolutions otherwise inline large startup futures twice.
        Box::pin(self.resolve_agent_session_inner(config, logical_session_id, false)).await
    }

    /// Like `resolve_agent_session` but forks a new session when the existing
    /// one is still processing a previous invocation.  This allows concurrent
    /// messages to be handled independently without waiting.
    pub(crate) async fn resolve_agent_session_concurrent(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
    ) -> anyhow::Result<ResolvedAgentSession> {
        Box::pin(self.resolve_agent_session_inner(config, logical_session_id, true)).await
    }

    /// Drop the active in-memory core session for a logical session after its
    /// effective security policy changes. The rollout remains available and
    /// will be resumed with the newly resolved config on the next invocation.
    pub(crate) async fn invalidate_logical_session_security(&self, logical_session_id: &str) {
        self.retire_logical_session(logical_session_id).await;
    }

    pub(crate) async fn logical_session_lock(
        &self,
        logical_id: &str,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.logical_session_lifecycle.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(logical_id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(logical_id.to_owned(), std::sync::Arc::downgrade(&lock));
        lock
    }

    /// End the core session as well as its logical binding. Serialize with
    /// creation so concurrent callers cannot create an untracked second session.
    pub(crate) async fn retire_logical_session(&self, logical_session_id: &str) {
        let lifecycle = self.logical_session_lock(logical_session_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.retire_logical_session_locked(logical_session_id).await;
    }

    pub(crate) async fn retire_and_remove_logical_session(
        &self,
        logical_session_id: &str,
    ) -> std::io::Result<(bool, usize)> {
        let lifecycle = self.logical_session_lock(logical_session_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.retire_logical_session_locked(logical_session_id).await;
        let removed = self.session_store.try_remove(logical_session_id).await?;
        let staging_cleaned = self.cleanup_logical_session_files(logical_session_id).await;
        Ok((removed, staging_cleaned))
    }

    async fn cleanup_logical_session_files(&self, logical_id: &str) -> usize {
        crate::channels::runtime::cleanup_session_runtime_state(
            &self.config.savfox_home,
            &[logical_id.to_owned()],
        )
        .await;
        crate::media_store::MediaStore::from_home(&self.config.savfox_home)
            .cleanup_staging_for_session(logical_id)
            .await
    }

    pub(crate) async fn restart_logical_session(&self, logical_id: &str) -> std::io::Result<()> {
        let lifecycle = self.logical_session_lock(logical_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.retire_logical_session_locked(logical_id).await;
        self.session_store
            .try_update(logical_id, |entry| {
                entry.model = None;
                entry.provider = None;
                entry.overrides = None;
                entry.core_thread_id = None;
                entry.session_file = None;
                entry.lifecycle_generation = uuid::Uuid::now_v7().to_string();
            })
            .await?;
        self.cleanup_logical_session_files(logical_id).await;
        Ok(())
    }

    pub(crate) async fn begin_terminal_generation(
        &self,
        logical_id: &str,
    ) -> anyhow::Result<String> {
        let lifecycle = self.logical_session_lock(logical_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.session_store
            .get_or_create(logical_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("failed to persist terminal session"))?;
        let entry = self
            .session_store
            .try_update(logical_id, |_| {})
            .await?
            .ok_or_else(|| anyhow::anyhow!("terminal session disappeared"))?;
        Ok(entry.lifecycle_generation)
    }

    pub(crate) async fn logical_session_is_current(&self, logical_id: &str, core_id: &str) -> bool {
        self.logical_session_threads
            .lock()
            .await
            .get(logical_id)
            .is_some_and(|id| id.to_string() == core_id)
    }

    async fn retire_core_session(&self, core_id: SessionId) {
        self.approval_coordinator()
            .remove_for_core_session(core_id)
            .await;
        if let Some(session) = self.session_manager.remove_session(&core_id).await {
            let shutdown = async {
                session
                    .submit(savfox_protocol::protocol::Op::Shutdown)
                    .await?;
                // A turn collector can also consume events, so observe the stable
                // agent status rather than competing for ShutdownComplete.
                while !matches!(
                    session.agent_status().await,
                    savfox_protocol::protocol::AgentStatus::Shutdown
                ) {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Ok::<(), savfox_core::error::SavfoxError>(())
            };
            match tokio::time::timeout(CORE_SESSION_SHUTDOWN_TIMEOUT, shutdown).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!(%core_id, %error, "failed to shut down retired session"),
                Err(_) => warn!(%core_id, "timed out shutting down retired session"),
            }
        }
    }

    async fn retire_logical_session_locked(&self, logical_session_id: &str) {
        if let Some(core_id) = self
            .resolve_active_logical_session_thread(logical_session_id)
            .await
        {
            self.retire_core_session(core_id).await;
        }
        self.logical_session_threads
            .lock()
            .await
            .remove(logical_session_id);
    }

    /// A prune result names the removed generation. New work can have rebound
    /// the logical ID since the database deletion, and must remain untouched.
    pub(crate) async fn retire_pruned_logical_session(
        &self,
        logical_id: &str,
        expected_core: Option<&str>,
    ) {
        let lifecycle = self.logical_session_lock(logical_id).await;
        let _lifecycle = lifecycle.lock().await;
        if let Some(expected_core) = expected_core {
            let Ok(core_id) = SessionId::from_string(expected_core) else {
                return;
            };
            self.retire_core_session(core_id).await;
            let mut bindings = self.logical_session_threads.lock().await;
            if bindings.get(logical_id) == Some(&core_id) {
                bindings.remove(logical_id);
            }
        } else if self.session_store.get(logical_id).await.is_none() {
            // Legacy metadata may lack the core pointer. Explicit creation
            // persists metadata under this same lock before binding a new core.
            self.retire_logical_session_locked(logical_id).await;
        }
        if self.session_store.get(logical_id).await.is_none() {
            self.cleanup_logical_session_files(logical_id).await;
        }
    }

    async fn resolve_agent_session_inner(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
        concurrent_fork: bool,
    ) -> anyhow::Result<ResolvedAgentSession> {
        use savfox_protocol::protocol::AgentStatus;

        let lifecycle = match logical_session_id {
            Some(id) => Some(self.logical_session_lock(id.trim()).await),
            None => None,
        };
        let _lifecycle = match lifecycle.as_ref() {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let logical_session_id = logical_session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        if let Some(logical_session_id) = logical_session_id.as_deref() {
            if self.session_store.is_tombstoned(logical_session_id).await? {
                self.retire_logical_session_locked(logical_session_id).await;
            }
            // Explicitly accepted work may recreate a reset session; stale
            // completion helpers may not clear a durable tombstone themselves.
            if self
                .session_store
                .get_or_create(logical_session_id)
                .await
                .is_none()
            {
                anyhow::bail!("failed to persist logical session before execution");
            }
            if let Some(active_thread_id) = self
                .resolve_active_logical_session_thread(logical_session_id)
                .await
            {
                // When concurrent_fork is enabled, check whether the existing
                // session is busy.  If so, fork a new ephemeral session that
                // shares the completed history but processes independently.
                if concurrent_fork
                    && let Ok(session) = self.session_manager.get_session(active_thread_id).await
                {
                    let status = session.agent_status().await;
                    if matches!(status, AgentStatus::Running | AgentStatus::PendingInit) {
                        debug!(
                            session_id = %active_thread_id,
                            status = ?status,
                            "Session is busy, forking for concurrent invocation"
                        );
                        return self
                            .fork_concurrent_session(config, session.rollout_path())
                            .await;
                    }
                }

                return Ok(ResolvedAgentSession {
                    session_id: active_thread_id,
                    cleanup_after_turn: false,
                });
            }

            if let Some(path) = self
                .resolve_session_rollout_path(logical_session_id)
                .await?
            {
                let resumed = Box::pin(self.session_manager.resume_session_from_rollout(
                    config,
                    path,
                    self.auth_manager.clone(),
                ))
                .await
                .map_err(|err| anyhow::anyhow!("failed to resume session thread: {err}"))?;
                self.bind_logical_session_thread(logical_session_id, &resumed.session_id)
                    .await;
                return Ok(ResolvedAgentSession {
                    session_id: resumed.session_id,
                    cleanup_after_turn: false,
                });
            }

            let new_session = Box::pin(self.session_manager.start_session(config))
                .await
                .map_err(|err| anyhow::anyhow!("failed to start thread: {err}"))?;
            self.bind_logical_session_thread(logical_session_id, &new_session.session_id)
                .await;
            return Ok(ResolvedAgentSession {
                session_id: new_session.session_id,
                cleanup_after_turn: false,
            });
        }

        let new_session = Box::pin(self.session_manager.start_session(config))
            .await
            .map_err(|err| anyhow::anyhow!("failed to start thread: {err}"))?;
        Ok(ResolvedAgentSession {
            session_id: new_session.session_id,
            cleanup_after_turn: true,
        })
    }

    /// Fork a concurrent session from the completed history in the rollout
    /// file.  If no rollout path is available (brand new session), start fresh.
    async fn fork_concurrent_session(
        &self,
        config: Config,
        rollout_path: Option<std::path::PathBuf>,
    ) -> anyhow::Result<ResolvedAgentSession> {
        let forked = if let Some(path) = rollout_path {
            Box::pin(self.session_manager.fork_session(usize::MAX, config, path))
                .await
                .map_err(|err| anyhow::anyhow!("failed to fork concurrent session: {err}"))?
        } else {
            Box::pin(self.session_manager.start_session(config))
                .await
                .map_err(|err| anyhow::anyhow!("failed to start fresh concurrent session: {err}"))?
        };
        debug!(
            session_id = %forked.session_id,
            "Forked concurrent session"
        );
        Ok(ResolvedAgentSession {
            session_id: forked.session_id,
            cleanup_after_turn: true,
        })
    }

    async fn resolve_active_logical_session_thread(
        &self,
        logical_session_id: &str,
    ) -> Option<SessionId> {
        let mapped_thread_id = {
            let bindings = self.logical_session_threads.lock().await;
            bindings.get(logical_session_id).copied()
        };

        if let Some(thread_id) = mapped_thread_id {
            if self.session_manager.get_session(thread_id).await.is_ok() {
                return Some(thread_id);
            }

            let mut bindings = self.logical_session_threads.lock().await;
            bindings.remove(logical_session_id);
        }

        if let Some(entry) = self.session_store.get(logical_session_id).await
            && let Some(thread_id) = entry
                .core_thread_id
                .as_deref()
                .or(entry.thread_id.as_deref())
            && let Ok(parsed_thread_id) = SessionId::from_string(thread_id)
            && self
                .session_manager
                .get_session(parsed_thread_id)
                .await
                .is_ok()
        {
            self.bind_logical_session_thread(logical_session_id, &parsed_thread_id)
                .await;
            return Some(parsed_thread_id);
        }

        if let Ok(parsed_requested_id) = SessionId::from_string(logical_session_id)
            && self
                .session_manager
                .get_session(parsed_requested_id)
                .await
                .is_ok()
        {
            self.bind_logical_session_thread(logical_session_id, &parsed_requested_id)
                .await;
            return Some(parsed_requested_id);
        }

        None
    }

    async fn bind_logical_session_thread(
        &self,
        logical_session_id: &str,
        thread_session_id: &SessionId,
    ) {
        let mut bindings = self.logical_session_threads.lock().await;
        bindings.insert(logical_session_id.to_owned(), *thread_session_id);
    }

    /// Interrupt the active agent session bound to a logical session, if any.
    pub(crate) async fn interrupt_logical_session(&self, logical_session_id: &str) {
        if let Some(session) = self.get_logical_session(logical_session_id).await
            && let Err(err) = session
                .submit(savfox_protocol::protocol::Op::Interrupt)
                .await
        {
            tracing::warn!(
                session_id = %logical_session_id,
                "failed to interrupt session: {err}"
            );
        }
    }

    /// Roll back conversation history on the active agent thread.
    /// Pass `u32::MAX` to clear all history.
    pub(crate) async fn rollback_logical_session(&self, logical_session_id: &str, num_turns: u32) {
        if let Some(session) = self.get_logical_session(logical_session_id).await
            && let Err(err) = session
                .submit(savfox_protocol::protocol::Op::SessionRollback { num_turns })
                .await
        {
            tracing::warn!(
                session_id = %logical_session_id,
                "failed to rollback session: {err}"
            );
        }
    }

    /// Resolve the active `SavfoxSession` for a logical session id, if any.
    async fn get_logical_session(
        &self,
        logical_session_id: &str,
    ) -> Option<std::sync::Arc<savfox_core::SavfoxSession>> {
        let thread_id = {
            let bindings = self.logical_session_threads.lock().await;
            bindings.get(logical_session_id).copied()
        };
        let thread_id = thread_id?;
        self.session_manager.get_session(thread_id).await.ok()
    }

    async fn resolve_session_rollout_path(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        if let Some(entry) = self.session_store.get(session_id).await {
            if let Some(session_file) = entry.session_file.as_deref() {
                let stored_path = PathBuf::from(session_file);
                let absolute = if stored_path.is_absolute() {
                    stored_path
                } else {
                    self.config
                        .savfox_home
                        .join(SESSIONS_SUBDIR)
                        .join(format!("{session_file}.jsonl"))
                };

                if tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
                    return Ok(Some(absolute));
                }
            }

            if let Some(thread_id) = entry
                .core_thread_id
                .as_deref()
                .or(entry.thread_id.as_deref())
                && let Some(path) = self.find_rollout_path_candidate(thread_id).await?
            {
                return Ok(Some(path));
            }
        }

        self.find_rollout_path_candidate(session_id).await
    }

    async fn find_rollout_path_candidate(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        let canonical = self
            .config
            .savfox_home
            .join(SESSIONS_SUBDIR)
            .join(format!("{session_id}.jsonl"));
        if tokio::fs::try_exists(&canonical).await.unwrap_or(false) {
            return Ok(Some(canonical));
        }

        if let Some(path) =
            find_session_path_by_id_str(&self.config.savfox_home, session_id).await?
        {
            return Ok(Some(path));
        }
        if let Some(path) =
            find_archived_session_path_by_id_str(&self.config.savfox_home, session_id).await?
        {
            return Ok(Some(path));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use savfox_core::config::ConfigBuilder;
    use savfox_protocol::protocol::AgentStatus;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::channel::GatewayBridgeArgs;
    use crate::session::{GatewaySessionManager, SessionStore};

    #[tokio::test]
    async fn logical_session_creation_is_single_flight_and_retirement_releases_core() {
        let home = tempfile::tempdir().unwrap();
        for root in [home.path().to_path_buf(), home.path().join("skills")] {
            std::fs::create_dir_all(root.join("registry/github.com/savhub-ai/registry")).unwrap();
        }
        let mut config = Box::pin(
            ConfigBuilder::default()
                .savfox_home(home.path().to_path_buf())
                .fallback_cwd(Some(home.path().to_path_buf()))
                .build(),
        )
        .await
        .unwrap();
        config.cwd = home.path().to_path_buf();
        config.model = Some("offline-lifecycle-model".to_owned());
        config.model_provider_id = "offline-lifecycle".to_owned();
        config.model_provider = serde_json::from_value(serde_json::json!({
            "id":"offline-lifecycle", "name":"Offline lifecycle fixture",
            "base_url":"http://127.0.0.1:1/v1", "wire_api":"responses",
            "requires_openai_auth":false, "supports_websockets":false,
            "request_max_retries":0, "stream_max_retries":0
        }))
        .unwrap();
        config.model_providers.clear();
        config.model_providers.insert(
            config.model_provider_id.clone(),
            config.model_provider.clone(),
        );
        config
            .features
            .disable(savfox_core::features::Feature::ResponsesWebsockets);
        config
            .features
            .disable(savfox_core::features::Feature::RemoteModels);
        let gateway = GatewayChannel::new(GatewayBridgeArgs {
            config: Arc::new(config.clone()),
            session_store: Arc::new(SessionStore::from_home(home.path())),
            cli_overrides: vec![],
            cloud_requirements: Default::default(),
            websocket_manager: GatewaySessionManager::new(),
            channel_registry: crate::channels::create_channel_registry(),
            channel_recovery_registry: crate::channels::recovery::create_channel_recovery_registry(
            ),
            channel_recovery_supervisors:
                crate::channels::recovery::create_channel_recovery_supervisors(),
        });
        let id = uuid::Uuid::now_v7().to_string();
        // Regression budget for production-facing futures, independent of test
        // thread stack configuration. Heavy startup phases live behind boxes.
        let single = gateway.resolve_agent_session(config.clone(), Some(&id));
        let concurrent = gateway.resolve_agent_session_concurrent(config.clone(), Some(&id));
        assert!(
            std::mem::size_of_val(&single) < 128 * 1024,
            "single resolver future uses {} bytes",
            std::mem::size_of_val(&single)
        );
        assert!(
            std::mem::size_of_val(&concurrent) < 128 * 1024,
            "concurrent resolver future uses {} bytes",
            std::mem::size_of_val(&concurrent)
        );
        drop((single, concurrent));

        let (first, second) = tokio::join!(
            gateway.resolve_agent_session(config.clone(), Some(&id)),
            gateway.resolve_agent_session(config.clone(), Some(&id)),
        );
        assert_eq!(first.unwrap().session_id, second.unwrap().session_id);
        assert_eq!(gateway.session_manager().list_session_ids().await.len(), 1);
        for _ in 0..3 {
            gateway.retire_logical_session(&id).await;
            assert!(
                gateway
                    .session_manager()
                    .list_session_ids()
                    .await
                    .is_empty()
            );
            assert!(gateway.logical_session_threads.lock().await.is_empty());
            gateway
                .resolve_agent_session(config.clone(), Some(&id))
                .await
                .unwrap();
        }
        let old_core = gateway
            .resolve_agent_session(config.clone(), Some(&id))
            .await
            .unwrap()
            .session_id;
        let old_core_string = old_core.to_string();
        let deleting = gateway.retire_and_remove_logical_session(&id);
        let stale_persist = crate::chat_session::persist_chat_session_metadata(
            &gateway,
            &gateway.session_store,
            home.path(),
            &id,
            &old_core_string,
            "old",
            "old",
            None,
            None,
            None,
        );
        // Both futures contend on the same lifecycle lock. Completion may win
        // first, but cannot publish metadata after deletion has succeeded.
        let (deleted, ()) = tokio::join!(deleting, stale_persist);
        deleted.unwrap();
        crate::chat_session::persist_session_thread_link(
            &gateway,
            &gateway.session_store,
            home.path(),
            &id,
            &old_core_string,
            None,
            None,
        )
        .await;
        assert!(gateway.session_store.get(&id).await.is_none());
        assert!(
            gateway
                .session_manager()
                .list_session_ids()
                .await
                .is_empty()
        );
        // Explicitly accepted work after reset gets a fresh generation.
        let next = gateway
            .resolve_agent_session(config.clone(), Some(&id))
            .await
            .unwrap();
        assert_ne!(next.session_id, old_core);
        assert!(gateway.session_store.get(&id).await.is_some());
        gateway
            .retire_pruned_logical_session(&id, Some(&old_core_string))
            .await;
        gateway.retire_pruned_logical_session(&id, None).await;
        assert!(
            gateway
                .session_manager()
                .get_session(next.session_id)
                .await
                .is_ok()
        );
        assert!(
            gateway
                .logical_session_is_current(&id, &next.session_id.to_string())
                .await
        );
        // Model the interval between durable prune and its asynchronous core
        // retirement: recreation must not reuse the still-bound old generation.
        gateway.session_store.try_remove(&id).await.unwrap();
        let after_prune = gateway
            .resolve_agent_session(config.clone(), Some(&id))
            .await
            .unwrap();
        assert_ne!(after_prune.session_id, next.session_id);
        gateway
            .retire_pruned_logical_session(&id, Some(&next.session_id.to_string()))
            .await;
        assert!(
            gateway
                .session_manager()
                .get_session(after_prune.session_id)
                .await
                .is_ok()
        );
        gateway.restart_logical_session(&id).await.unwrap();
        crate::chat_session::persist_session_thread_link(
            &gateway,
            &gateway.session_store,
            home.path(),
            &id,
            &next.session_id.to_string(),
            None,
            None,
        )
        .await;
        assert!(
            gateway
                .session_store
                .get(&id)
                .await
                .unwrap()
                .core_thread_id
                .is_none()
        );
        assert!(
            gateway
                .session_manager()
                .list_session_ids()
                .await
                .is_empty()
        );
        gateway
            .retire_and_remove_logical_session(&id)
            .await
            .unwrap();
        let terminal_id = uuid::Uuid::now_v7().to_string();
        let old_generation = gateway
            .begin_terminal_generation(&terminal_id)
            .await
            .unwrap();
        gateway.restart_logical_session(&terminal_id).await.unwrap();
        crate::chat_session::persist_chat_session_metadata(
            &gateway,
            &gateway.session_store,
            home.path(),
            &terminal_id,
            &terminal_id,
            "terminal/test",
            "terminal",
            None,
            None,
            Some(&old_generation),
        )
        .await;
        assert!(
            gateway
                .session_store
                .get(&terminal_id)
                .await
                .unwrap()
                .core_thread_id
                .is_none()
        );
        gateway
            .retire_and_remove_logical_session(&terminal_id)
            .await
            .unwrap();
        let generation = gateway
            .begin_terminal_generation(&terminal_id)
            .await
            .unwrap();
        assert_ne!(generation, old_generation);
        crate::chat_session::persist_chat_session_metadata(
            &gateway,
            &gateway.session_store,
            home.path(),
            &terminal_id,
            &terminal_id,
            "terminal/old",
            "terminal",
            None,
            None,
            Some(&old_generation),
        )
        .await;
        assert!(
            gateway
                .session_store
                .get(&terminal_id)
                .await
                .unwrap()
                .core_thread_id
                .is_none()
        );
        crate::chat_session::persist_chat_session_metadata(
            &gateway,
            &gateway.session_store,
            home.path(),
            &terminal_id,
            &terminal_id,
            "terminal/current",
            "terminal",
            None,
            None,
            Some(&generation),
        )
        .await;
        assert_eq!(
            gateway
                .session_store
                .get(&terminal_id)
                .await
                .unwrap()
                .model
                .as_deref(),
            Some("terminal/current")
        );
        gateway
            .retire_and_remove_logical_session(&terminal_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn partial_output_timeout_is_an_error_and_stops_its_turn() {
        for persistent in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                // Drain the request, including potentially large instructions,
                // before serving a stream that deliberately never completes.
                let mut request = Vec::new();
                let mut chunk = [0; 8192];
                let header_end = loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(offset) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    {
                        break offset + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                }
                let events = [
                    serde_json::json!({"type":"response.created","response":{"id":"partial-response"}}),
                    serde_json::json!({"type":"response.output_item.added","item":{"type":"message","role":"assistant","id":"message-1","content":[]}}),
                    serde_json::json!({"type":"response.output_text.delta","delta":"partial answer"}),
                ].into_iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect::<String>();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{events}\r\n",
                    events.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                // Keep ownership of the connection until the cancelled client
                // drops it; do not send a completed event or terminal chunk.
                let _ = socket.read(&mut chunk).await;
            });
            let home = tempfile::tempdir().unwrap();
            for root in [home.path().to_path_buf(), home.path().join("skills")] {
                std::fs::create_dir_all(root.join("registry/github.com/savhub-ai/registry"))
                    .unwrap();
            }
            let mut config = ConfigBuilder::default()
                .savfox_home(home.path().to_path_buf())
                .build()
                .await
                .unwrap();
            config.cwd = home.path().to_path_buf();
            config.model = Some("test-model".to_owned());
            config.model_provider.base_url = Some(base_url);
            config.model_provider.wire_api = savfox_core::WireApi::Responses;
            config.model_provider.env_key = None;
            config.model_provider.requires_openai_auth = false;
            config.model_provider.supports_websockets = false;
            config.model_provider.stream_idle_timeout_ms = Some(600_000);
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(0);
            config
                .features
                .disable(savfox_core::features::Feature::RemoteModels);
            config
                .features
                .disable(savfox_core::features::Feature::ResponsesWebsockets);
            let store = Arc::new(SessionStore::from_home(home.path()));
            let gateway = Arc::new(GatewayChannel::new(GatewayBridgeArgs {
                config: Arc::new(config),
                session_store: store,
                cli_overrides: vec![],
                cloud_requirements: Default::default(),
                websocket_manager: GatewaySessionManager::new(),
                channel_registry: crate::channels::create_channel_registry(),
                channel_recovery_registry:
                    crate::channels::recovery::create_channel_recovery_registry(),
                channel_recovery_supervisors:
                    crate::channels::recovery::create_channel_recovery_supervisors(),
            }));
            let logical_id = persistent.then(|| uuid::Uuid::now_v7().to_string());
            let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
            let invoking = gateway.clone();
            let invocation = tokio::spawn(async move {
                invoking
                    .invoke_agent_text_in_session_with_metadata_impl(
                        "Respond with text",
                        "default",
                        logical_id.as_deref(),
                        move |delta| {
                            let _ = delta_tx.send(delta.to_owned());
                        },
                        None,
                        false,
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
            });
            let partial = tokio::time::timeout(Duration::from_secs(10), delta_rx.recv())
                .await
                .unwrap();
            assert_eq!(partial.as_deref(), Some("partial answer"));
            let ids = gateway.session_manager().list_session_ids().await;
            assert_eq!(ids.len(), 1);
            let session = gateway.session_manager().get_session(ids[0]).await.unwrap();
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(121)).await;
            let error = invocation.await.unwrap().unwrap_err();
            assert!(error.to_string().contains("timed out"), "{error:#}");
            assert!(!matches!(
                session.agent_status().await,
                AgentStatus::Running | AgentStatus::PendingInit
            ));
            assert_eq!(
                gateway.session_manager().list_session_ids().await.len(),
                usize::from(persistent)
            );
            tokio::time::resume();
            gateway
                .session_manager()
                .remove_and_close_all_sessions()
                .await
                .unwrap();
            server.abort();
        }
    }
    #[tokio::test]
    async fn invocation_cleanup_preserves_later_turn_approvals_on_the_same_session() {
        use crate::security::execution_policy::ApprovalClientCapabilities;

        let home = tempfile::tempdir().unwrap();
        let coordinator = ApprovalCoordinator::new(home.path().to_path_buf());
        let session_id = SessionId::new();
        let mut approvals = Vec::new();
        for turn_id in ["old-turn", "next-turn"] {
            approvals.push(
                coordinator
                    .register(
                        turn_id.to_owned(),
                        session_id,
                        ChannelApprovalScope {
                            channel: "telegram:room".to_owned(),
                            channel_config_id: None,
                            account_id: None,
                            peer_id: "owner".to_owned(),
                            logical_session_id: "conversation".to_owned(),
                        },
                        ChannelApprovalKind::Patch,
                        ApprovalClientCapabilities::interactive(),
                        ApprovalRequestMetadata {
                            agent_id: "test-agent".to_owned(),
                            policy_fingerprint: "policy".to_owned(),
                            cwd: home.path().to_path_buf(),
                            summary: "patch".to_owned(),
                            command: None,
                            reason: None,
                        },
                    )
                    .await
                    .unwrap(),
            );
        }
        remove_invocation_approvals(&coordinator, &[approvals[0].request_id.clone()]).await;
        assert!(!coordinator.is_pending(&approvals[0].request_id).await);
        assert!(coordinator.is_pending(&approvals[1].request_id).await);
    }
}
