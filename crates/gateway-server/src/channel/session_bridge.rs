use std::path::PathBuf;

use savfox_core::config::Config;
use savfox_core::{
    SESSIONS_SUBDIR, find_archived_session_path_by_id_str, find_session_path_by_id_str,
};
use savfox_protocol::SessionId;
use tracing::{debug, warn};

use super::{AgentInvocationResult, GatewayChannel, ResolvedAgentSession};
use crate::security::approval_coordinator::{
    ApprovalNotification, ApprovalRequestMetadata, ChannelApprovalKind, ChannelApprovalScope,
    PendingChannelApproval,
};
use crate::security::execution_policy::{ExecutionMode, ResolvedExecutionSecurity};

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
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        self.invoke_agent_text_in_session_with_metadata_impl(
            prompt, model, session_id, on_delta, None, false, None, None, None,
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

        if let Some(invocation) = self
            .invoke_terminal_delegate_agent(
                prompt,
                model,
                model,
                requested_session_id.as_deref(),
                &[],
                None,
            )
            .await?
        {
            on_delta(&invocation.result.reply);
            return Ok(invocation.result);
        }

        let resolved_session = if concurrent_fork {
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
                &self.session_store,
                &self.config.savfox_home,
                logical_id,
                &session_id.to_string(),
                rollout_path.as_deref(),
            )
            .await;
        }

        // Submit the user message.
        let user_input = UserInput::Text {
            text: prompt.to_owned(),
            text_elements: Vec::new(),
        };
        session
            .submit(Op::UserInput {
                items: vec![user_input],
                final_output_json_schema: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("failed to submit message: {e}"))?;

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
                            if reply.is_empty() && fallback_agent_reply.is_empty() {
                                self.approval_coordinator()
                                    .remove_for_core_session(session_id)
                                    .await;
                                return Err(anyhow::anyhow!("agent error: {}", err.message));
                            }
                            break;
                        }
                        _ => {
                            // Skip other events (tool calls, reasoning, etc.).
                        }
                    }
                }
                Ok(Err(e)) => {
                    if reply.is_empty() && fallback_agent_reply.is_empty() {
                        self.approval_coordinator()
                            .remove_for_core_session(session_id)
                            .await;
                        return Err(anyhow::anyhow!("thread error: {e}"));
                    }
                    break;
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
                    if reply.is_empty() && fallback_agent_reply.is_empty() {
                        warn!(session_id = %session_id, "agent invocation timed out");
                        self.approval_coordinator()
                            .remove_for_core_session(session_id)
                            .await;
                        return Err(anyhow::anyhow!("agent invocation timed out"));
                    }
                    break;
                }
            }
        }

        if let Some(pending) = pending_approval.take() {
            if let Some(request_id) = pending.request_id.as_deref() {
                self.approval_coordinator().remove(request_id).await;
            }
            let _ = session.submit(pending.abort_op()).await;
        }
        self.approval_coordinator()
            .remove_for_core_session(session_id)
            .await;

        if !saw_delta && !fallback_agent_reply.is_empty() {
            reply.push_str(&fallback_agent_reply);
            on_delta(&fallback_agent_reply);
        }

        if resolved_session.cleanup_after_turn {
            let _ = self.session_manager.remove_session(&session_id).await;
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
        })
    }

    pub(crate) async fn resolve_agent_session(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
    ) -> anyhow::Result<ResolvedAgentSession> {
        self.resolve_agent_session_inner(config, logical_session_id, false)
            .await
    }

    /// Like `resolve_agent_session` but forks a new session when the existing
    /// one is still processing a previous invocation.  This allows concurrent
    /// messages to be handled independently without waiting.
    pub(crate) async fn resolve_agent_session_concurrent(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
    ) -> anyhow::Result<ResolvedAgentSession> {
        self.resolve_agent_session_inner(config, logical_session_id, true)
            .await
    }

    /// Drop the active in-memory core session for a logical session after its
    /// effective security policy changes. The rollout remains available and
    /// will be resumed with the newly resolved config on the next invocation.
    pub(crate) async fn invalidate_logical_session_security(&self, logical_session_id: &str) {
        if let Some(core_session_id) = self
            .resolve_active_logical_session_thread(logical_session_id)
            .await
        {
            self.approval_coordinator()
                .remove_for_core_session(core_session_id)
                .await;
            let _ = self.session_manager.remove_session(&core_session_id).await;
        }

        self.logical_session_threads
            .lock()
            .await
            .remove(logical_session_id);
    }

    async fn resolve_agent_session_inner(
        &self,
        config: Config,
        logical_session_id: Option<&str>,
        concurrent_fork: bool,
    ) -> anyhow::Result<ResolvedAgentSession> {
        use savfox_protocol::protocol::AgentStatus;

        let logical_session_id = logical_session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        if let Some(logical_session_id) = logical_session_id.as_deref() {
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
                let resumed = self
                    .session_manager
                    .resume_session_from_rollout(config, path, self.auth_manager.clone())
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to resume session thread: {err}"))?;
                self.bind_logical_session_thread(logical_session_id, &resumed.session_id)
                    .await;
                return Ok(ResolvedAgentSession {
                    session_id: resumed.session_id,
                    cleanup_after_turn: false,
                });
            }

            let new_session = self
                .session_manager
                .start_session(config)
                .await
                .map_err(|err| anyhow::anyhow!("failed to start thread: {err}"))?;
            self.bind_logical_session_thread(logical_session_id, &new_session.session_id)
                .await;
            return Ok(ResolvedAgentSession {
                session_id: new_session.session_id,
                cleanup_after_turn: false,
            });
        }

        let new_session = self
            .session_manager
            .start_session(config)
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
            self.session_manager
                .fork_session(usize::MAX, config, path)
                .await
                .map_err(|err| anyhow::anyhow!("failed to fork concurrent session: {err}"))?
        } else {
            self.session_manager
                .start_session(config)
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

    /// Unbind the logical session thread mapping so the next message creates a
    /// fresh agent session.
    pub(crate) async fn unbind_logical_session_thread(&self, logical_session_id: &str) {
        let mut bindings = self.logical_session_threads.lock().await;
        bindings.remove(logical_session_id);
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
