use std::path::PathBuf;

use savfox_core::config::Config;
use savfox_core::{
    SESSIONS_SUBDIR, find_archived_session_path_by_id_str, find_session_path_by_id_str,
};
use savfox_protocol::SessionId;
use tracing::{debug, warn};

use super::{AgentInvocationResult, GatewayChannel, ResolvedAgentSession};

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
            prompt, model, session_id, on_delta, None, false,
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
        on_delta: F,
        on_approval: Box<dyn FnMut(&str) + Send>,
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
        )
        .await
    }

    async fn invoke_agent_text_in_session_with_metadata_impl<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        mut on_delta: F,
        mut on_approval: Option<Box<dyn FnMut(&str) + Send>>,
        concurrent_fork: bool,
    ) -> anyhow::Result<AgentInvocationResult>
    where
        F: FnMut(&str) + Send,
    {
        use savfox_protocol::protocol::{EventMsg, Op};
        use savfox_protocol::user_input::UserInput;

        // Strip "[user]:" prefix if present (some clients add this prefix)
        let prompt = prompt
            .strip_prefix("[user]:")
            .map(|s| s.trim())
            .unwrap_or(prompt.trim());

        let mut config = (*self.config).clone();
        let model = model.trim();
        // Only override the config model when a real model slug is provided.
        // The callers often pass "default" to mean "use the default agent",
        // which should NOT replace the configured model name.
        if !model.is_empty() && model != "default" {
            config.model = Some(model.to_owned());
        }

        let requested_session_id = session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
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
            .invoke_terminal_delegate_agent(prompt, model, model, requested_session_id.as_deref())
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
        let mut pending_approval_summary: Option<String> = None;
        let normal_timeout = tokio::time::Duration::from_secs(120);
        let approval_timeout = tokio::time::Duration::from_secs(300);
        let mut deadline = tokio::time::Instant::now() + normal_timeout;

        loop {
            match tokio::time::timeout_at(deadline, session.next_event()).await {
                Ok(Ok(event)) => {
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
                            pending_approval_summary =
                                Some(format!("exec approval for turn {}", req.turn_id));
                            if let Some(ref mut notify) = on_approval {
                                let cmd = req.command.join(" ");
                                let reason = req
                                    .reason
                                    .as_deref()
                                    .map(|r| format!("\nReason: {r}"))
                                    .unwrap_or_default();
                                let msg = format!(
                                    "[Approval Required]\nCommand: {cmd}{reason}\n\nReply + to approve, - to deny."
                                );
                                notify(&msg);
                                deadline = tokio::time::Instant::now() + approval_timeout;
                            }
                        }
                        EventMsg::ApplyPatchApprovalRequest(req) => {
                            pending_approval_summary =
                                Some(format!("patch approval for turn {}", req.turn_id));
                            if let Some(ref mut notify) = on_approval {
                                let files: Vec<_> = req
                                    .changes
                                    .keys()
                                    .map(|p| p.display().to_string())
                                    .collect();
                                let reason = req
                                    .reason
                                    .as_deref()
                                    .map(|r| format!("\nReason: {r}"))
                                    .unwrap_or_default();
                                let msg = format!(
                                    "[Approval Required]\nFile changes: {}{reason}\n\nReply + to approve, - to deny.",
                                    files.join(", ")
                                );
                                notify(&msg);
                                deadline = tokio::time::Instant::now() + approval_timeout;
                            }
                        }
                        EventMsg::TurnComplete(_) => {
                            break;
                        }
                        EventMsg::Error(err) => {
                            if reply.is_empty() && fallback_agent_reply.is_empty() {
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
                        return Err(anyhow::anyhow!("thread error: {e}"));
                    }
                    break;
                }
                Err(_) => {
                    // Timeout.
                    if reply.is_empty() && fallback_agent_reply.is_empty() {
                        if let Some(summary) = pending_approval_summary.as_deref() {
                            warn!(
                                session_id = %session_id,
                                pending_approval = %summary,
                                "agent invocation timed out while waiting for approval"
                            );
                            return Err(anyhow::anyhow!(
                                "agent invocation timed out while waiting for {summary}"
                            ));
                        }
                        warn!(session_id = %session_id, "agent invocation timed out");
                        return Err(anyhow::anyhow!("agent invocation timed out"));
                    }
                    break;
                }
            }
        }

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
            && let Some(thread_id) = entry.thread_id.as_deref()
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

            if let Some(thread_id) = entry.thread_id.as_deref()
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
