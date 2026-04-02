use std::ffi::OsStr;
use std::fs::{FileTimes, OpenOptions};
use std::path::Path;
use std::time::{Duration, SystemTime};

use savfox_app_server_protocol::{
    JSONRPCErrorError, RequestId, ServerNotification, SessionArchiveParams, SessionArchiveResponse,
    SessionForkParams, SessionForkResponse, SessionListParams, SessionListResponse,
    SessionLoadedListParams, SessionLoadedListResponse, SessionReadParams, SessionReadResponse,
    SessionResumeParams, SessionResumeResponse, SessionRollbackParams, SessionSetNameParams,
    SessionSetNameResponse, SessionSortKey, SessionStartParams, SessionStartResponse,
    SessionStartedNotification, SessionUnarchiveParams, SessionUnarchiveResponse,
    build_turns_from_event_msgs,
};
use savfox_core::features::Feature;
use savfox_core::protocol::{EventMsg, Op, SessionConfiguredEvent};
use savfox_core::state_db::get_state_db;
use savfox_core::{
    InitialHistory, NewSession, RolloutRecorder, SessionSortKey as CoreSessionSortKey,
    find_archived_session_path_by_id_str, find_session_path_by_id_str, read_session_meta_line,
    rollout_date_parts,
};
use savfox_protocol::SessionId;
use savfox_protocol::dynamic_tools::DynamicToolSpec as CoreDynamicToolSpec;
use savfox_protocol::protocol::SessionMetaLine;
use tokio::sync::{broadcast, oneshot};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::{
    INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE, SESSION_LIST_DEFAULT_LIMIT,
    SESSION_LIST_MAX_LIMIT, SavfoxMessageProcessor, build_ephemeral_session, derive_config_for_cwd,
    derive_config_from_params, read_event_msgs_from_rollout, read_summary_from_rollout,
    summary_to_session, validate_dynamic_tools,
};
use crate::bespoke_event_handling::apply_bespoke_event_handling;
use crate::filters::{compute_source_filters, source_kind_matches};
use crate::outgoing_message::OutgoingNotification;

impl SavfoxMessageProcessor {
    pub(crate) async fn session_start(
        &mut self,
        request_id: RequestId,
        params: SessionStartParams,
    ) {
        let SessionStartParams {
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            config,
            base_instructions,
            developer_instructions,
            dynamic_tools,
            mock_experimental_field: _mock_experimental_field,
            experimental_raw_events,
            personality,
            ephemeral,
        } = params;
        let mut typesafe_overrides = self.build_session_config_overrides(
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            base_instructions,
            developer_instructions,
            personality,
        );
        typesafe_overrides.ephemeral = ephemeral;

        let config = match derive_config_from_params(
            &self.cli_overrides,
            config,
            typesafe_overrides,
            &self.cloud_requirements,
        )
        .await
        {
            Ok(config) => config,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("error deriving config: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let dynamic_tools = dynamic_tools.unwrap_or_default();
        let core_dynamic_tools = if dynamic_tools.is_empty() {
            Vec::new()
        } else {
            let snapshot = savfox_core::mcp::collect_mcp_snapshot(&config).await;
            let mcp_tool_names = snapshot
                .tools
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            if let Err(message) = validate_dynamic_tools(&dynamic_tools, &mcp_tool_names) {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message,
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
            dynamic_tools
                .into_iter()
                .map(|tool| CoreDynamicToolSpec {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                })
                .collect()
        };

        match self
            .session_manager
            .start_session_with_tools(config, core_dynamic_tools)
            .await
        {
            Ok(new_conv) => {
                let NewSession {
                    session_id,
                    session,
                    session_configured,
                    
                } = new_conv;
                let config_snapshot = session.config_snapshot().await;
                let fallback_provider = self.config.model_provider_id.as_str();

                // A bit hacky, but the summary contains a lot of useful information for the session
                // that unfortunately does not get returned from session_manager.start_session().
                let session = match session_configured.rollout_path.as_ref() {
                    Some(rollout_path) => {
                        match read_summary_from_rollout(rollout_path.as_path(), fallback_provider)
                            .await
                        {
                            Ok(summary) => summary_to_session(summary),
                            Err(err) => {
                                self.send_internal_error(
                                    request_id,
                                    format!(
                                        "failed to load rollout `{}` for session {session_id}: {err}",
                                        rollout_path.display()
                                    ),
                                )
                                .await;
                                return;
                            }
                        }
                    }
                    None => build_ephemeral_session(session_id, &config_snapshot),
                };

                let response = SessionStartResponse {
                    session: session.clone(),
                    model: config_snapshot.model,
                    model_provider: config_snapshot.model_provider_id,
                    cwd: config_snapshot.cwd,
                    approval_policy: config_snapshot.approval_policy.into(),
                    sandbox: config_snapshot.sandbox_policy.into(),
                    reasoning_effort: config_snapshot.reasoning_effort,
                };

                // Auto-attach a session listener when starting a session.
                // Use the same behavior as the v1 API, with opt-in support for raw item events.
                if let Err(err) = self
                    .attach_conversation_listener(session_id, experimental_raw_events)
                    .await
                {
                    tracing::warn!(
                        "failed to attach listener for session {}: {}",
                        session_id,
                        err.message
                    );
                }

                self.outgoing.send_response(request_id, response).await;

                let notif = SessionStartedNotification { session };
                self.outgoing
                    .send_server_notification(ServerNotification::SessionStarted(notif))
                    .await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("error creating session: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    pub(crate) async fn session_archive(
        &mut self,
        request_id: RequestId,
        params: SessionArchiveParams,
    ) {
        // TODO(jif) mostly rewrite this using sqlite after phase 1
        let session_id = match SessionId::from_string(&params.session_id) {
            Ok(id) => id,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("invalid session id: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let rollout_path =
            match find_session_path_by_id_str(&self.config.savfox_home, &session_id.to_string())
                .await
            {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("no rollout found for session id {session_id}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("failed to locate session id {session_id}: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            };

        match self.archive_session_common(session_id, &rollout_path).await {
            Ok(()) => {
                let response = SessionArchiveResponse {};
                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => {
                self.outgoing.send_error(request_id, err).await;
            }
        }
    }

    pub(crate) async fn session_set_name(
        &self,
        request_id: RequestId,
        params: SessionSetNameParams,
    ) {
        let SessionSetNameParams { session_id, name } = params;
        let Some(name) = savfox_core::util::normalize_session_name(&name) else {
            self.send_invalid_request_error(
                request_id,
                "session name must not be empty".to_owned(),
            )
            .await;
            return;
        };

        let (_, session) = match self.load_session(&session_id).await {
            Ok(v) => v,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        if let Err(err) = session.submit(Op::SetSessionName { name }).await {
            self.send_internal_error(request_id, format!("failed to set session name: {err}"))
                .await;
            return;
        }

        self.outgoing
            .send_response(request_id, SessionSetNameResponse {})
            .await;
    }

    pub(crate) async fn session_unarchive(
        &mut self,
        request_id: RequestId,
        params: SessionUnarchiveParams,
    ) {
        // TODO(jif) mostly rewrite this using sqlite after phase 1
        let session_id = match SessionId::from_string(&params.session_id) {
            Ok(id) => id,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("invalid session id: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let archived_path = match find_archived_session_path_by_id_str(
            &self.config.savfox_home,
            &session_id.to_string(),
        )
        .await
        {
            Ok(Some(path)) => path,
            Ok(None) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("no archived rollout found for session id {session_id}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("failed to locate archived session id {session_id}: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let rollout_path_display = archived_path.display().to_string();
        let fallback_provider = self.config.model_provider_id.clone();
        let state_db_ctx = get_state_db(&self.config, None).await;
        let archived_folder = self
            .config
            .savfox_home
            .join(savfox_core::ARCHIVED_SESSIONS_SUBDIR);

        let result: Result<savfox_app_server_protocol::Session, JSONRPCErrorError> = async {
            let canonical_archived_dir = tokio::fs::canonicalize(&archived_folder).await.map_err(
                |err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!(
                        "failed to unarchive session: unable to resolve archived directory: {err}"
                    ),
                    data: None,
                },
            )?;
            let canonical_rollout_path = tokio::fs::canonicalize(&archived_path).await;
            let canonical_rollout_path = if let Ok(path) = canonical_rollout_path
                && path.starts_with(&canonical_archived_dir)
            {
                path
            } else {
                return Err(JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!(
                        "rollout path `{rollout_path_display}` must be in archived directory"
                    ),
                    data: None,
                });
            };

            let required_suffix = format!("{session_id}.jsonl");
            let Some(file_name) = canonical_rollout_path.file_name().map(OsStr::to_owned) else {
                return Err(JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("rollout path `{rollout_path_display}` missing file name"),
                    data: None,
                });
            };
            if !file_name
                .to_string_lossy()
                .ends_with(required_suffix.as_str())
            {
                return Err(JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!(
                        "rollout path `{rollout_path_display}` does not match session id {session_id}"
                    ),
                    data: None,
                });
            }

            let Some((year, month, day)) = rollout_date_parts(&file_name) else {
                return Err(JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!(
                        "rollout path `{rollout_path_display}` missing filename timestamp"
                    ),
                    data: None,
                });
            };

            let sessions_folder = self.config.savfox_home.join(savfox_core::SESSIONS_SUBDIR);
            let dest_dir = sessions_folder.join(year).join(month).join(day);
            let restored_path = dest_dir.join(&file_name);
            tokio::fs::create_dir_all(&dest_dir)
                .await
                .map_err(|err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to unarchive session: {err}"),
                    data: None,
                })?;
            tokio::fs::rename(&canonical_rollout_path, &restored_path)
                .await
                .map_err(|err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to unarchive session: {err}"),
                    data: None,
                })?;
            tokio::task::spawn_blocking({
                let restored_path = restored_path.clone();
                move || -> std::io::Result<()> {
                    let times = FileTimes::new().set_modified(SystemTime::now());
                    OpenOptions::new()
                        .append(true)
                        .open(&restored_path)?
                        .set_times(times)?;
                    Ok(())
                }
            })
            .await
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to update unarchived session timestamp: {err}"),
                data: None,
            })?
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to update unarchived session timestamp: {err}"),
                data: None,
            })?;
            if let Some(ctx) = state_db_ctx {
                let _ = ctx
                    .mark_unarchived(session_id, restored_path.as_path())
                    .await;
            }
            let summary =
                read_summary_from_rollout(restored_path.as_path(), fallback_provider.as_str())
                    .await
                    .map_err(|err| JSONRPCErrorError {
                        code: INTERNAL_ERROR_CODE,
                        message: format!("failed to read unarchived session: {err}"),
                        data: None,
                    })?;
            Ok(summary_to_session(summary))
        }
        .await;

        match result {
            Ok(session) => {
                let response = SessionUnarchiveResponse { session };
                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => {
                self.outgoing.send_error(request_id, err).await;
            }
        }
    }

    pub(crate) async fn session_rollback(
        &mut self,
        request_id: RequestId,
        params: SessionRollbackParams,
    ) {
        let SessionRollbackParams {
            session_id,
            num_turns,
        } = params;

        if num_turns == 0 {
            self.send_invalid_request_error(request_id, "numTurns must be >= 1".to_owned())
                .await;
            return;
        }

        let (session_id, session) = match self.load_session(&session_id).await {
            Ok(v) => v,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        {
            let mut map = self.pending_rollbacks.lock().await;
            if map.contains_key(&session_id) {
                self.send_invalid_request_error(
                    request_id,
                    "rollback already in progress for this session".to_owned(),
                )
                .await;
                return;
            }

            map.insert(session_id, request_id.clone());
        }

        if let Err(err) = session.submit(Op::SessionRollback { num_turns }).await {
            // No SessionRollback event will arrive if an error occurs.
            // Clean up and reply immediately.
            let mut map = self.pending_rollbacks.lock().await;
            map.remove(&session_id);

            self.send_internal_error(request_id, format!("failed to start rollback: {err}"))
                .await;
        }
    }

    pub(crate) async fn session_list(&self, request_id: RequestId, params: SessionListParams) {
        let SessionListParams {
            cursor,
            limit,
            sort_key,
            model_providers,
            source_kinds,
            archived,
        } = params;

        let requested_page_size = limit
            .map(|value| value as usize)
            .unwrap_or(SESSION_LIST_DEFAULT_LIMIT)
            .clamp(1, SESSION_LIST_MAX_LIMIT);
        let core_sort_key = match sort_key.unwrap_or(SessionSortKey::CreatedAt) {
            SessionSortKey::CreatedAt => CoreSessionSortKey::CreatedAt,
            SessionSortKey::UpdatedAt => CoreSessionSortKey::UpdatedAt,
        };
        let (summaries, next_cursor) = match self
            .list_sessions_common(
                requested_page_size,
                cursor,
                model_providers,
                source_kinds,
                core_sort_key,
                archived.unwrap_or(false),
            )
            .await
        {
            Ok(r) => r,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let data = summaries.into_iter().map(summary_to_session).collect();
        let response = SessionListResponse { data, next_cursor };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn session_loaded_list(
        &self,
        request_id: RequestId,
        params: SessionLoadedListParams,
    ) {
        let SessionLoadedListParams { cursor, limit } = params;
        let mut data = self
            .session_manager
            .list_session_ids()
            .await
            .into_iter()
            .map(|session_id| session_id.to_string())
            .collect::<Vec<_>>();

        if data.is_empty() {
            let response = SessionLoadedListResponse {
                data,
                next_cursor: None,
            };
            self.outgoing.send_response(request_id, response).await;
            return;
        }

        data.sort();
        let total = data.len();
        let start = match cursor {
            Some(cursor) => {
                let cursor = if let Ok(id) = SessionId::from_string(&cursor) { id.to_string() } else {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid cursor: {cursor}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                };
                match data.binary_search(&cursor) {
                    Ok(idx) => idx + 1,
                    Err(idx) => idx,
                }
            }
            None => 0,
        };

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let end = start.saturating_add(effective_limit).min(total);
        let page = data[start..end].to_vec();
        let next_cursor = page.last().filter(|_| end < total).cloned();

        let response = SessionLoadedListResponse {
            data: page,
            next_cursor,
        };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn session_read(&mut self, request_id: RequestId, params: SessionReadParams) {
        let SessionReadParams {
            session_id,
            include_turns,
        } = params;

        let session_uuid = match SessionId::from_string(&session_id) {
            Ok(id) => id,
            Err(err) => {
                self.send_invalid_request_error(request_id, format!("invalid session id: {err}"))
                    .await;
                return;
            }
        };

        let rollout_path =
            match find_session_path_by_id_str(&self.config.savfox_home, &session_uuid.to_string())
                .await
            {
                Ok(Some(path)) => Some(path),
                Ok(None) => None,
                Err(err) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("failed to locate session id {session_uuid}: {err}"),
                    )
                    .await;
                    return;
                }
            };

        let mut session = if let Some(rollout_path) = rollout_path.as_ref() {
            let fallback_provider = self.config.model_provider_id.as_str();
            match read_summary_from_rollout(rollout_path, fallback_provider).await {
                Ok(summary) => summary_to_session(summary),
                Err(err) => {
                    self.send_internal_error(
                        request_id,
                        format!(
                            "failed to load rollout `{}` for session {session_uuid}: {err}",
                            rollout_path.display()
                        ),
                    )
                    .await;
                    return;
                }
            }
        } else {
            let Ok(session) = self.session_manager.get_session(session_uuid).await else {
                self.send_invalid_request_error(
                    request_id,
                    format!("session not loaded: {session_uuid}"),
                )
                .await;
                return;
            };
            let config_snapshot = session.config_snapshot().await;
            if include_turns {
                self.send_invalid_request_error(
                    request_id,
                    "ephemeral sessions do not support includeTurns".to_owned(),
                )
                .await;
                return;
            }
            build_ephemeral_session(session_uuid, &config_snapshot)
        };

        if include_turns && let Some(rollout_path) = rollout_path.as_ref() {
            match read_event_msgs_from_rollout(rollout_path).await {
                Ok(events) => {
                    session.turns = build_turns_from_event_msgs(&events);
                }
                Err(err) => {
                    self.send_internal_error(
                        request_id,
                        format!(
                            "failed to load rollout `{}` for session {session_uuid}: {err}",
                            rollout_path.display()
                        ),
                    )
                    .await;
                    return;
                }
            }
        }

        let response = SessionReadResponse { session };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) fn session_created_receiver(&self) -> broadcast::Receiver<SessionId> {
        self.session_manager.subscribe_session_created()
    }

    /// Best-effort: attach a listener for session_id if missing.
    pub(crate) async fn try_attach_session_listener(&mut self, session_id: SessionId) {
        if self
            .listener_session_ids_by_subscription
            .values()
            .any(|entry| *entry == session_id)
        {
            return;
        }

        if let Err(err) = self.attach_conversation_listener(session_id, false).await {
            warn!(
                "failed to attach listener for session {session_id}: {message}",
                message = err.message
            );
        }
    }

    pub(crate) async fn session_resume(
        &mut self,
        request_id: RequestId,
        params: SessionResumeParams,
    ) {
        let SessionResumeParams {
            session_id,
            history,
            path,
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            config: request_overrides,
            base_instructions,
            developer_instructions,
            personality,
        } = params;

        let session_history = if let Some(history) = history {
            if history.is_empty() {
                self.send_invalid_request_error(
                    request_id,
                    "history must not be empty".to_owned(),
                )
                .await;
                return;
            }
            InitialHistory::Forked(
                history
                    .into_iter()
                    .map(savfox_protocol::protocol::RolloutItem::ResponseItem)
                    .collect(),
            )
        } else if let Some(path) = path {
            match RolloutRecorder::get_rollout_history(&path).await {
                Ok(initial_history) => initial_history,
                Err(err) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("failed to load rollout `{}`: {err}", path.display()),
                    )
                    .await;
                    return;
                }
            }
        } else {
            let existing_session_id = match SessionId::from_string(&session_id) {
                Ok(id) => id,
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid session id: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            };

            let path = match find_session_path_by_id_str(
                &self.config.savfox_home,
                &existing_session_id.to_string(),
            )
            .await
            {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("no rollout found for session id {existing_session_id}"),
                    )
                    .await;
                    return;
                }
                Err(err) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("failed to locate session id {existing_session_id}: {err}"),
                    )
                    .await;
                    return;
                }
            };

            match RolloutRecorder::get_rollout_history(&path).await {
                Ok(initial_history) => initial_history,
                Err(err) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("failed to load rollout `{}`: {err}", path.display()),
                    )
                    .await;
                    return;
                }
            }
        };

        let history_cwd = session_history.session_cwd();
        let typesafe_overrides = self.build_session_config_overrides(
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            base_instructions,
            developer_instructions,
            personality,
        );

        // Derive a Config using the same logic as new conversation, honoring overrides if provided.
        let config = match derive_config_for_cwd(
            &self.cli_overrides,
            request_overrides,
            typesafe_overrides,
            history_cwd,
            &self.cloud_requirements,
        )
        .await
        {
            Ok(config) => config,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("error deriving config: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let fallback_model_provider = config.model_provider_id.clone();

        match self
            .session_manager
            .resume_session_with_history(config, session_history, self.auth_manager.clone())
            .await
        {
            Ok(NewSession {
                session_id,
                session_configured,
                ..
            }) => {
                let SessionConfiguredEvent {
                    rollout_path,
                    initial_messages,
                    ..
                } = session_configured;
                let Some(rollout_path) = rollout_path else {
                    self.send_internal_error(
                        request_id,
                        format!("rollout path missing for session {session_id}"),
                    )
                    .await;
                    return;
                };
                // Auto-attach a session listener when resuming a session.
                if let Err(err) = self.attach_conversation_listener(session_id, false).await {
                    tracing::warn!(
                        "failed to attach listener for session {}: {}",
                        session_id,
                        err.message
                    );
                }

                let mut session = match read_summary_from_rollout(
                    rollout_path.as_path(),
                    fallback_model_provider.as_str(),
                )
                .await
                {
                    Ok(summary) => summary_to_session(summary),
                    Err(err) => {
                        self.send_internal_error(
                            request_id,
                            format!(
                                "failed to load rollout `{}` for session {session_id}: {err}",
                                rollout_path.display()
                            ),
                        )
                        .await;
                        return;
                    }
                };
                session.turns = initial_messages
                    .as_deref()
                    .map_or_else(Vec::new, build_turns_from_event_msgs);

                let response = SessionResumeResponse {
                    session,
                    model: session_configured.model,
                    model_provider: session_configured.model_provider_id,
                    cwd: session_configured.cwd,
                    approval_policy: session_configured.approval_policy.into(),
                    sandbox: session_configured.sandbox_policy.into(),
                    reasoning_effort: session_configured.reasoning_effort,
                };

                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("error resuming session: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    pub(crate) async fn session_fork(&mut self, request_id: RequestId, params: SessionForkParams) {
        let SessionForkParams {
            session_id,
            path,
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            config: cli_overrides,
            base_instructions,
            developer_instructions,
        } = params;

        let rollout_path = if let Some(path) = path {
            path
        } else {
            let existing_session_id = match SessionId::from_string(&session_id) {
                Ok(id) => id,
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid session id: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            };

            match find_session_path_by_id_str(
                &self.config.savfox_home,
                &existing_session_id.to_string(),
            )
            .await
            {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("no rollout found for session id {existing_session_id}"),
                    )
                    .await;
                    return;
                }
                Err(err) => {
                    self.send_invalid_request_error(
                        request_id,
                        format!("failed to locate session id {existing_session_id}: {err}"),
                    )
                    .await;
                    return;
                }
            }
        };

        let history_cwd = match read_session_meta_line(&rollout_path).await {
            Ok(meta_line) => Some(meta_line.meta.cwd),
            Err(err) => {
                let rollout_path = rollout_path.display();
                warn!("failed to read session metadata from rollout {rollout_path}: {err}");
                None
            }
        };

        // Persist windows sandbox feature.
        let mut cli_overrides = cli_overrides.unwrap_or_default();
        if cfg!(windows) && self.config.features.enabled(Feature::WindowsSandbox) {
            cli_overrides.insert(
                "features.experimental_windows_sandbox".to_owned(),
                serde_json::json!(true),
            );
        }
        let request_overrides = if cli_overrides.is_empty() {
            None
        } else {
            Some(cli_overrides)
        };
        let typesafe_overrides = self.build_session_config_overrides(
            model,
            model_provider,
            cwd,
            approval_policy,
            sandbox,
            base_instructions,
            developer_instructions,
            None,
        );
        // Derive a Config using the same logic as new conversation, honoring overrides if provided.
        let config = match derive_config_for_cwd(
            &self.cli_overrides,
            request_overrides,
            typesafe_overrides,
            history_cwd,
            &self.cloud_requirements,
        )
        .await
        {
            Ok(config) => config,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("error deriving config: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let fallback_model_provider = config.model_provider_id.clone();

        let NewSession {
            session_id,
            session_configured,
            ..
        } = match self
            .session_manager
            .fork_session(usize::MAX, config, rollout_path.clone())
            .await
        {
            Ok(session) => session,
            Err(err) => {
                let (code, message) = match err {
                    savfox_core::error::SavfoxError::Io(_)
                    | savfox_core::error::SavfoxError::Json(_) => (
                        INVALID_REQUEST_ERROR_CODE,
                        format!("failed to load rollout `{}`: {err}", rollout_path.display()),
                    ),
                    savfox_core::error::SavfoxError::InvalidRequest(message) => {
                        (INVALID_REQUEST_ERROR_CODE, message)
                    }
                    _ => (INTERNAL_ERROR_CODE, format!("error forking session: {err}")),
                };
                let error = JSONRPCErrorError {
                    code,
                    message,
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let SessionConfiguredEvent {
            rollout_path,
            initial_messages,
            ..
        } = session_configured;
        let Some(rollout_path) = rollout_path else {
            self.send_internal_error(
                request_id,
                format!("rollout path missing for session {session_id}"),
            )
            .await;
            return;
        };
        // Auto-attach a conversation listener when forking a session.
        if let Err(err) = self.attach_conversation_listener(session_id, false).await {
            tracing::warn!(
                "failed to attach listener for session {}: {}",
                session_id,
                err.message
            );
        }

        let mut session = match read_summary_from_rollout(
            rollout_path.as_path(),
            fallback_model_provider.as_str(),
        )
        .await
        {
            Ok(summary) => summary_to_session(summary),
            Err(err) => {
                self.send_internal_error(
                    request_id,
                    format!(
                        "failed to load rollout `{}` for session {session_id}: {err}",
                        rollout_path.display()
                    ),
                )
                .await;
                return;
            }
        };
        session.turns = initial_messages
            .as_deref()
            .map_or_else(Vec::new, build_turns_from_event_msgs);

        let response = SessionForkResponse {
            session: session.clone(),
            model: session_configured.model,
            model_provider: session_configured.model_provider_id,
            cwd: session_configured.cwd,
            approval_policy: session_configured.approval_policy.into(),
            sandbox: session_configured.sandbox_policy.into(),
            reasoning_effort: session_configured.reasoning_effort,
        };

        self.outgoing.send_response(request_id, response).await;

        let notif = SessionStartedNotification { session };
        self.outgoing
            .send_server_notification(ServerNotification::SessionStarted(notif))
            .await;
    }

    async fn list_sessions_common(
        &self,
        requested_page_size: usize,
        cursor: Option<String>,
        model_providers: Option<Vec<String>>,
        source_kinds: Option<Vec<savfox_app_server_protocol::SessionSourceKind>>,
        sort_key: CoreSessionSortKey,
        archived: bool,
    ) -> Result<
        (
            Vec<savfox_app_server_protocol::ConversationSummary>,
            Option<String>,
        ),
        JSONRPCErrorError,
    > {
        let mut cursor_obj: Option<savfox_core::Cursor> = match cursor.as_ref() {
            Some(cursor_str) => {
                Some(
                    savfox_core::parse_cursor(cursor_str).ok_or_else(|| JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid cursor: {cursor_str}"),
                        data: None,
                    })?,
                )
            }
            None => None,
        };
        let mut last_cursor = cursor_obj.clone();
        let mut remaining = requested_page_size;
        let mut items = Vec::with_capacity(requested_page_size);
        let mut next_cursor: Option<String> = None;

        let model_provider_filter = match model_providers {
            Some(providers) => {
                if providers.is_empty() {
                    None
                } else {
                    Some(providers)
                }
            }
            None => Some(vec![self.config.model_provider_id.clone()]),
        };
        let fallback_provider = self.config.model_provider_id.clone();
        let (allowed_sources_vec, source_kind_filter) = compute_source_filters(source_kinds);
        let allowed_sources = allowed_sources_vec.as_slice();

        while remaining > 0 {
            let page_size = remaining.min(SESSION_LIST_MAX_LIMIT);
            let page = if archived {
                RolloutRecorder::list_archived_sessions(
                    &self.config.savfox_home,
                    page_size,
                    cursor_obj.as_ref(),
                    sort_key,
                    allowed_sources,
                    model_provider_filter.as_deref(),
                    fallback_provider.as_str(),
                )
                .await
                .map_err(|err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to list sessions: {err}"),
                    data: None,
                })?
            } else {
                RolloutRecorder::list_sessions(
                    &self.config.savfox_home,
                    page_size,
                    cursor_obj.as_ref(),
                    sort_key,
                    allowed_sources,
                    model_provider_filter.as_deref(),
                    fallback_provider.as_str(),
                )
                .await
                .map_err(|err| JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to list sessions: {err}"),
                    data: None,
                })?
            };

            let mut filtered = page
                .items
                .into_iter()
                .filter_map(|it| {
                    let updated_at = it.updated_at.clone();
                    let session_meta_line = it.head.first().and_then(|first| {
                        serde_json::from_value::<SessionMetaLine>(first.clone()).ok()
                    })?;
                    super::extract_conversation_summary(
                        it.path,
                        &it.head,
                        &session_meta_line.meta,
                        session_meta_line.git.as_ref(),
                        fallback_provider.as_str(),
                        updated_at,
                    )
                })
                .filter(|summary| {
                    source_kind_filter
                        .as_ref()
                        .is_none_or(|filter| source_kind_matches(&summary.source, filter))
                })
                .collect::<Vec<_>>();
            if filtered.len() > remaining {
                filtered.truncate(remaining);
            }
            items.extend(filtered);
            remaining = requested_page_size.saturating_sub(items.len());

            // Encode RolloutCursor into the JSON-RPC string form returned to clients.
            let next_cursor_value = page.next_cursor.clone();
            next_cursor = next_cursor_value
                .as_ref()
                .and_then(|cursor| serde_json::to_value(cursor).ok())
                .and_then(|value| value.as_str().map(str::to_owned));
            if remaining == 0 {
                break;
            }

            match next_cursor_value {
                Some(cursor_val) if remaining > 0 => {
                    // Break if our pagination would reuse the same cursor again; this avoids
                    // an infinite loop when filtering drops everything on the page.
                    if last_cursor.as_ref() == Some(&cursor_val) {
                        next_cursor = None;
                        break;
                    }
                    last_cursor = Some(cursor_val.clone());
                    cursor_obj = Some(cursor_val);
                }
                _ => break,
            }
        }

        Ok((items, next_cursor))
    }

    async fn archive_session_common(
        &mut self,
        session_id: SessionId,
        rollout_path: &Path,
    ) -> Result<(), JSONRPCErrorError> {
        // Verify rollout_path is under sessions dir.
        let rollout_folder = self.config.savfox_home.join(savfox_core::SESSIONS_SUBDIR);

        let canonical_sessions_dir = match tokio::fs::canonicalize(&rollout_folder).await {
            Ok(path) => path,
            Err(err) => {
                return Err(JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!(
                        "failed to archive session: unable to resolve sessions directory: {err}"
                    ),
                    data: None,
                });
            }
        };
        let canonical_rollout_path = tokio::fs::canonicalize(rollout_path).await;
        let canonical_rollout_path = if let Ok(path) = canonical_rollout_path
            && path.starts_with(&canonical_sessions_dir)
        {
            path
        } else {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` must be in sessions directory",
                    rollout_path.display()
                ),
                data: None,
            });
        };

        // Verify file name matches session id.
        let required_suffix = format!("{session_id}.jsonl");
        let Some(file_name) = canonical_rollout_path.file_name().map(OsStr::to_owned) else {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` missing file name",
                    rollout_path.display()
                ),
                data: None,
            });
        };
        if !file_name
            .to_string_lossy()
            .ends_with(required_suffix.as_str())
        {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "rollout path `{}` does not match session id {session_id}",
                    rollout_path.display()
                ),
                data: None,
            });
        }

        let mut state_db_ctx = None;

        // If the session is active, request shutdown and wait briefly.
        if let Some(conversation) = self.session_manager.remove_session(&session_id).await {
            if let Some(ctx) = conversation.state_db() {
                state_db_ctx = Some(ctx);
            }
            info!("session {session_id} was active; shutting down");
            // Request shutdown.
            match conversation.submit(Op::Shutdown).await {
                Ok(_) => {
                    // Poll agent status rather than consuming events so attached listeners do not
                    // block shutdown.
                    let wait_for_shutdown = async {
                        loop {
                            if matches!(
                                conversation.agent_status().await,
                                savfox_protocol::protocol::AgentStatus::Shutdown
                            ) {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    };
                    if tokio::time::timeout(Duration::from_secs(10), wait_for_shutdown)
                        .await
                        .is_err()
                    {
                        warn!("session {session_id} shutdown timed out; proceeding with archive");
                    }
                }
                Err(err) => {
                    error!("failed to submit Shutdown to session {session_id}: {err}");
                }
            }
        }

        if state_db_ctx.is_none() {
            state_db_ctx = get_state_db(&self.config, None).await;
        }

        // Move the rollout file to archived.
        let result: std::io::Result<()> = async move {
            let archive_folder = self
                .config
                .savfox_home
                .join(savfox_core::ARCHIVED_SESSIONS_SUBDIR);
            tokio::fs::create_dir_all(&archive_folder).await?;
            let archived_path = archive_folder.join(&file_name);
            tokio::fs::rename(&canonical_rollout_path, &archived_path).await?;
            if let Some(ctx) = state_db_ctx {
                let _ = ctx
                    .mark_archived(session_id, archived_path.as_path(), chrono::Utc::now())
                    .await;
            }
            Ok(())
        }
        .await;

        result.map_err(|err| JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message: format!("failed to archive session: {err}"),
            data: None,
        })
    }

    pub(crate) async fn attach_conversation_listener(
        &mut self,
        conversation_id: SessionId,
        experimental_raw_events: bool,
    ) -> Result<Uuid, JSONRPCErrorError> {
        let conversation = match self.session_manager.get_session(conversation_id).await {
            Ok(conv) => conv,
            Err(_) => {
                return Err(JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("session not found: {conversation_id}"),
                    data: None,
                });
            }
        };

        let subscription_id = Uuid::new_v4();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        self.conversation_listeners
            .insert(subscription_id, cancel_tx);
        self.listener_session_ids_by_subscription
            .insert(subscription_id, conversation_id);

        let outgoing_for_task = self.outgoing.clone();
        let pending_interrupts = self.pending_interrupts.clone();
        let pending_rollbacks = self.pending_rollbacks.clone();
        let turn_summary_store = self.turn_summary_store.clone();
        let fallback_model_provider = self.config.model_provider_id.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => {
                        // User has unsubscribed, so exit this task.
                        break;
                    }
                    event = conversation.next_event() => {
                        let event = match event {
                            Ok(event) => event,
                            Err(err) => {
                                tracing::warn!("session.next_event() failed with: {err}");
                                break;
                            }
                        };

                        if let EventMsg::RawResponseItem(_) = &event.msg
                            && !experimental_raw_events {
                                continue;
                            }

                        // For now, we send a notification for every event,
                        // JSON-serializing the `Event` as-is, but these should
                        // be migrated to be variants of `ServerNotification`
                        // instead.
                        let event_formatted = match &event.msg {
                            EventMsg::TurnStarted(_) => "task_started",
                            EventMsg::TurnComplete(_) => "task_complete",
                            _ => &event.msg.to_string(),
                        };
                        let mut params = match serde_json::to_value(event.clone()) {
                            Ok(serde_json::Value::Object(map)) => map,
                            Ok(_) => {
                                error!("event did not serialize to an object");
                                continue;
                            }
                            Err(err) => {
                                error!("failed to serialize event: {err}");
                                continue;
                            }
                        };
                        params.insert(
                            "conversationId".to_owned(),
                            conversation_id.to_string().into(),
                        );

                        outgoing_for_task
                            .send_notification(OutgoingNotification {
                                method: format!("savfox/event/{event_formatted}"),
                                params: Some(params.into()),
                            })
                            .await;

                        apply_bespoke_event_handling(
                            event.clone(),
                            conversation_id,
                            conversation.clone(),
                            outgoing_for_task.clone(),
                            pending_interrupts.clone(),
                            pending_rollbacks.clone(),
                            turn_summary_store.clone(),
                            fallback_model_provider.clone(),
                        )
                        .await;
                    }
                }
            }
        });
        Ok(subscription_id)
    }
}
