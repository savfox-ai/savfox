use savfox_app_server_protocol::{
    ClientRequest, JSONRPCErrorError, JSONRPCRequest, RequestId, ServerNotification,
    ServerRequestPayload,
};
use savfox_core::config::edit::{ConfigEdit, ConfigEditsBuilder};
use savfox_core::{
    ARCHIVED_SESSIONS_SUBDIR, SESSIONS_SUBDIR, find_archived_session_path_by_id_str,
    find_session_path_by_id_str, rollout_date_parts,
};
use savfox_protocol::SessionId;
use savfox_protocol::protocol::Op;
use savfox_protocol::user_input::UserInput;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use super::{
    BridgeOutgoing, GatewayChannel, INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE,
    METHOD_NOT_FOUND_ERROR_CODE,
};

impl GatewayChannel {
    /// Process an incoming JSON-RPC request from a WebSocket client.
    pub(crate) async fn process_request(&self, session_id: &str, request: JSONRPCRequest) {
        let request_id = request.id.clone();

        let request_json = match serde_json::to_value(&request) {
            Ok(v) => v,
            Err(err) => {
                self.send_error(
                    request_id,
                    INVALID_REQUEST_ERROR_CODE,
                    format!("invalid request: {err}"),
                )
                .await;
                return;
            }
        };

        let client_request = match serde_json::from_value::<ClientRequest>(request_json) {
            Ok(r) => r,
            Err(err) => {
                self.send_error(
                    request_id,
                    INVALID_REQUEST_ERROR_CODE,
                    format!("unknown method or invalid params: {err}"),
                )
                .await;
                return;
            }
        };

        info!(
            session_id = %session_id,
            method = %request.method,
            "processing gateway request"
        );

        self.dispatch_client_request(session_id, client_request)
            .await;
    }

    /// Dispatch a parsed `ClientRequest` to the appropriate handler.
    ///
    /// Routes each variant to the SessionManager or responds directly.
    /// This mirrors the pattern in `app-server`'s `SavfoxMessageProcessor`.
    async fn dispatch_client_request(&self, _session_id: &str, request: ClientRequest) {
        match request {
            // === Session lifecycle ===
            ClientRequest::SessionStart {
                request_id,
                params: _,
            } => {
                info!("gateway: session/start");
                let config = (*self.config).clone();
                match self.session_manager.start_session(config).await {
                    Ok(new_session) => {
                        let result = serde_json::json!({
                            "session_id": new_session.session_id.to_string(),
                        });
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(request_id, INTERNAL_ERROR_CODE, format!("{err}"))
                            .await;
                    }
                }
            }

            ClientRequest::SessionList {
                request_id,
                params: _,
            } => {
                let session_ids = self.session_manager.list_session_ids().await;
                let ids: Vec<String> = session_ids.iter().map(|id| id.to_string()).collect();
                let result = serde_json::json!({ "session_ids": ids });
                self.send_response(request_id, result).await;
            }

            ClientRequest::SessionLoadedList {
                request_id,
                params: _,
            } => {
                let session_ids = self.session_manager.list_session_ids().await;
                let ids: Vec<String> = session_ids.iter().map(|id| id.to_string()).collect();
                let result = serde_json::json!({ "session_ids": ids });
                self.send_response(request_id, result).await;
            }

            ClientRequest::SessionArchive { request_id, params } => {
                info!("gateway: session/archive");
                let session_id_str = params.session_id;
                let session_id = match SessionId::from_string(&session_id_str) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // Find the rollout file in sessions/.
                let rollout_path = match find_session_path_by_id_str(
                    &self.config.savfox_home,
                    &session_id.to_string(),
                )
                .await
                {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("no rollout found for session {session_id}"),
                        )
                        .await;
                        return;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to locate session {session_id}: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // If session is active, remove it and shut it down.
                if let Some(session) = self.session_manager.remove_session(&session_id).await {
                    let _ = session.submit(Op::Shutdown).await;
                }

                // Move rollout file from sessions/ to archived_sessions/.
                let file_name = if let Some(f) = rollout_path.file_name() {
                    f.to_owned()
                } else {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        "rollout path missing file name".to_owned(),
                    )
                    .await;
                    return;
                };
                let archive_folder = self.config.savfox_home.join(ARCHIVED_SESSIONS_SUBDIR);
                if let Err(err) = tokio::fs::create_dir_all(&archive_folder).await {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        format!("failed to create archive dir: {err}"),
                    )
                    .await;
                    return;
                }
                let archived_path = archive_folder.join(&file_name);
                if let Err(err) = tokio::fs::rename(&rollout_path, &archived_path).await {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        format!("failed to archive thread: {err}"),
                    )
                    .await;
                    return;
                }

                self.send_response(request_id, serde_json::json!({ "status": "archived" }))
                    .await;
            }

            ClientRequest::SessionUnarchive { request_id, params } => {
                info!("gateway: session/unarchive");
                let session_id_str = params.session_id;
                let session_id = match SessionId::from_string(&session_id_str) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // Find the rollout file in archived_sessions/.
                let archived_path = match find_archived_session_path_by_id_str(
                    &self.config.savfox_home,
                    &session_id.to_string(),
                )
                .await
                {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("no archived rollout found for session {session_id}"),
                        )
                        .await;
                        return;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to locate archived session {session_id}: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // Move back to sessions/ directory.
                let file_name = if let Some(f) = archived_path.file_name() {
                    f.to_owned()
                } else {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        "archived path missing file name".to_owned(),
                    )
                    .await;
                    return;
                };

                let (year, month, day) = if let Some(parts) = rollout_date_parts(&file_name) {
                    parts
                } else {
                    // Fall back to a flat directory.
                    let sessions_folder = self.config.savfox_home.join(SESSIONS_SUBDIR);
                    let restored_path = sessions_folder.join(&file_name);
                    if let Err(err) = tokio::fs::create_dir_all(&sessions_folder).await {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to create sessions dir: {err}"),
                        )
                        .await;
                        return;
                    }
                    if let Err(err) = tokio::fs::rename(&archived_path, &restored_path).await {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to unarchive thread: {err}"),
                        )
                        .await;
                        return;
                    }
                    self.send_response(
                        request_id,
                        serde_json::json!({ "status": "unarchived", "session_id": session_id_str }),
                    )
                    .await;
                    return;
                };

                let sessions_folder = self.config.savfox_home.join(SESSIONS_SUBDIR);
                let dest_dir = sessions_folder.join(&year).join(&month).join(&day);
                if let Err(err) = tokio::fs::create_dir_all(&dest_dir).await {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        format!("failed to create sessions subdir: {err}"),
                    )
                    .await;
                    return;
                }
                let restored_path = dest_dir.join(&file_name);
                if let Err(err) = tokio::fs::rename(&archived_path, &restored_path).await {
                    self.send_error(
                        request_id,
                        INTERNAL_ERROR_CODE,
                        format!("failed to unarchive thread: {err}"),
                    )
                    .await;
                    return;
                }

                self.send_response(
                    request_id,
                    serde_json::json!({ "status": "unarchived", "session_id": session_id_str }),
                )
                .await;
            }

            ClientRequest::SessionSetName { request_id, params } => {
                info!("gateway: session/setName");
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                let name = params.name.trim().to_owned();
                if name.is_empty() {
                    self.send_error(
                        request_id,
                        INVALID_REQUEST_ERROR_CODE,
                        "session name must not be empty".to_owned(),
                    )
                    .await;
                    return;
                }

                match self.session_manager.get_session(session_id).await {
                    Ok(session) => {
                        if let Err(err) = session
                            .submit(Op::SetSessionName { name: name.clone() })
                            .await
                        {
                            self.send_error(
                                request_id,
                                INTERNAL_ERROR_CODE,
                                format!("failed to set session name: {err}"),
                            )
                            .await;
                            return;
                        }
                        self.send_response(request_id, serde_json::json!({ "status": "ok" }))
                            .await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("session not found: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::SessionRollback { request_id, params } => {
                info!("gateway: session/rollback");
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                if params.num_turns == 0 {
                    self.send_error(
                        request_id,
                        INVALID_REQUEST_ERROR_CODE,
                        "numTurns must be >= 1".to_owned(),
                    )
                    .await;
                    return;
                }

                match self.session_manager.get_session(session_id).await {
                    Ok(session) => {
                        if let Err(err) = session
                            .submit(Op::SessionRollback {
                                num_turns: params.num_turns,
                            })
                            .await
                        {
                            self.send_error(
                                request_id,
                                INTERNAL_ERROR_CODE,
                                format!("failed to rollback: {err}"),
                            )
                            .await;
                            return;
                        }
                        self.send_response(request_id, serde_json::json!({ "status": "ok" }))
                            .await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("session not found: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::SessionRead { request_id, params } => {
                info!("gateway: session/read");
                let session_id_str = params.session_id.clone();
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // Try to find rollout on disk.
                let rollout_path = match find_session_path_by_id_str(
                    &self.config.savfox_home,
                    &session_id.to_string(),
                )
                .await
                {
                    Ok(Some(p)) => Some(p),
                    _ => None,
                };

                let mut result = serde_json::json!({
                    "session_id": session_id_str,
                    "rolloutPath": rollout_path.as_ref().map(|p| p.display().to_string()),
                });

                // If active, include agent status.
                if let Ok(session) = self.session_manager.get_session(session_id).await {
                    let status = session.agent_status().await;
                    result["agentStatus"] = serde_json::json!(format!("{status:?}"));
                }

                self.send_response(request_id, result).await;
            }

            ClientRequest::SessionResume { request_id, params } => {
                info!("gateway: session/resume");

                // Determine rollout path from params.
                let rollout_path = if let Some(path) = params.path {
                    path
                } else {
                    let session_id_str = params.session_id;
                    match find_session_path_by_id_str(&self.config.savfox_home, &session_id_str)
                        .await
                    {
                        Ok(Some(p)) => p,
                        Ok(None) => {
                            self.send_error(
                                request_id,
                                INVALID_REQUEST_ERROR_CODE,
                                format!("no rollout found for session {session_id_str}"),
                            )
                            .await;
                            return;
                        }
                        Err(err) => {
                            self.send_error(
                                request_id,
                                INTERNAL_ERROR_CODE,
                                format!("failed to locate session: {err}"),
                            )
                            .await;
                            return;
                        }
                    }
                };

                let config = (*self.config).clone();
                match self
                    .session_manager
                    .resume_session_from_rollout(config, rollout_path, self.auth_manager.clone())
                    .await
                {
                    Ok(new_thread) => {
                        let result = serde_json::json!({
                            "session_id": new_thread.session_id.to_string(),
                        });
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to resume thread: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::SessionFork { request_id, params } => {
                info!("gateway: thread/fork");

                // Determine rollout path.
                let rollout_path = if let Some(path) = params.path {
                    path
                } else {
                    let session_id_str = params.session_id;
                    match find_session_path_by_id_str(&self.config.savfox_home, &session_id_str)
                        .await
                    {
                        Ok(Some(p)) => p,
                        Ok(None) => {
                            self.send_error(
                                request_id,
                                INVALID_REQUEST_ERROR_CODE,
                                format!("no rollout found for thread {session_id_str}"),
                            )
                            .await;
                            return;
                        }
                        Err(err) => {
                            self.send_error(
                                request_id,
                                INTERNAL_ERROR_CODE,
                                format!("failed to locate thread: {err}"),
                            )
                            .await;
                            return;
                        }
                    }
                };

                let config = (*self.config).clone();
                // Fork at the end (keep all history) by default.
                match self
                    .session_manager
                    .fork_session(usize::MAX, config, rollout_path)
                    .await
                {
                    Ok(new_thread) => {
                        let result = serde_json::json!({
                            "session_id": new_thread.session_id.to_string(),
                        });
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to fork thread: {err}"),
                        )
                        .await;
                    }
                }
            }

            // === Turn lifecycle ===
            ClientRequest::TurnStart { request_id, params } => {
                info!("gateway: turn/start");
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid thread id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                match self.session_manager.get_session(session_id).await {
                    Ok(session) => {
                        // Convert v2 UserInput items to core UserInput items.
                        let items: Vec<UserInput> = params
                            .input
                            .into_iter()
                            .map(|item| item.into_core())
                            .collect();
                        let op = Op::UserInput {
                            items,
                            final_output_json_schema: params.output_schema,
                        };
                        match session.submit(op).await {
                            Ok(turn_id) => {
                                self.send_response(
                                    request_id,
                                    serde_json::json!({
                                        "status": "started",
                                        "turnId": turn_id,
                                    }),
                                )
                                .await;
                            }
                            Err(err) => {
                                self.send_error(
                                    request_id,
                                    INTERNAL_ERROR_CODE,
                                    format!("failed to start turn: {err}"),
                                )
                                .await;
                            }
                        }
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("thread not found: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::TurnInterrupt { request_id, params } => {
                info!("gateway: turn/interrupt");
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                match self.session_manager.get_session(session_id).await {
                    Ok(session) => {
                        if let Err(err) = session.submit(Op::Interrupt).await {
                            self.send_error(
                                request_id,
                                INTERNAL_ERROR_CODE,
                                format!("failed to interrupt turn: {err}"),
                            )
                            .await;
                            return;
                        }
                        self.send_response(
                            request_id,
                            serde_json::json!({ "status": "interrupted" }),
                        )
                        .await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("session not found: {err}"),
                        )
                        .await;
                    }
                }
            }

            // === Model / Skills / Config ===
            ClientRequest::ModelList {
                request_id,
                params: _,
            } => {
                let models = self
                    .session_manager
                    .list_models(
                        &self.config,
                        savfox_core::models_manager::manager::RefreshStrategy::OnlineIfUncached,
                    )
                    .await;
                let result = serde_json::to_value(&models).unwrap_or(Value::Null);
                self.send_response(request_id, result).await;
            }

            ClientRequest::SkillsList { request_id, params } => {
                let cwds = if params.cwds.is_empty() {
                    vec![self.config.cwd.clone()]
                } else {
                    params.cwds
                };

                let skills_manager = self.session_manager.skills_manager();
                let mut entries = Vec::new();

                for cwd in cwds {
                    let outcome = skills_manager
                        .skills_for_cwd(&cwd, params.force_reload)
                        .await;
                    let skills: Vec<Value> = outcome
                        .skills
                        .iter()
                        .map(|skill| {
                            let enabled = !outcome.disabled_paths.contains(&skill.path);
                            serde_json::json!({
                                "name": skill.name,
                                "description": skill.description,
                                "shortDescription": skill.short_description,
                                "path": skill.path,
                                "scope": skill.scope,
                                "enabled": enabled,
                            })
                        })
                        .collect();
                    let errors: Vec<Value> = outcome
                        .errors
                        .iter()
                        .map(|err| {
                            serde_json::json!({
                                "path": err.path,
                                "message": err.message,
                            })
                        })
                        .collect();

                    entries.push(serde_json::json!({
                        "cwd": cwd,
                        "skills": skills,
                        "errors": errors,
                    }));
                }

                let result = serde_json::json!({ "data": entries });
                self.send_response(request_id, result).await;
            }

            ClientRequest::AppsList {
                request_id,
                params: _,
            } => {
                let result = serde_json::json!({ "apps": [] });
                self.send_response(request_id, result).await;
            }

            ClientRequest::SkillsConfigWrite { request_id, params } => {
                let edits = vec![ConfigEdit::SetSkillConfig {
                    path: params.path.clone(),
                    enabled: params.enabled,
                }];

                match ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_edits(edits)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        self.session_manager.skills_manager().clear_cache();
                        let result = serde_json::json!({ "effectiveEnabled": params.enabled });
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("failed to update skill settings: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::CollaborationModeList {
                request_id,
                params: _,
            } => {
                let modes = self.session_manager.list_collaboration_modes();
                let result = serde_json::to_value(&modes).unwrap_or(Value::Null);
                self.send_response(request_id, result).await;
            }

            ClientRequest::ReviewStart { request_id, params } => {
                info!("gateway: review/start");
                let session_id = match SessionId::from_string(&params.session_id) {
                    Ok(id) => id,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid session id: {err}"),
                        )
                        .await;
                        return;
                    }
                };

                // Convert the v2 ReviewTarget to core ReviewTarget via serde roundtrip.
                let target_json = match serde_json::to_value(&params.target) {
                    Ok(v) => v,
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("invalid review target: {err}"),
                        )
                        .await;
                        return;
                    }
                };
                let core_target: savfox_protocol::protocol::ReviewTarget =
                    match serde_json::from_value(target_json) {
                        Ok(v) => v,
                        Err(err) => {
                            self.send_error(
                                request_id,
                                INVALID_REQUEST_ERROR_CODE,
                                format!("failed to map review target: {err}"),
                            )
                            .await;
                            return;
                        }
                    };

                match self.session_manager.get_session(session_id).await {
                    Ok(thread) => {
                        let review_request = savfox_protocol::protocol::ReviewRequest {
                            target: core_target,
                            user_facing_hint: None,
                        };
                        match thread.submit(Op::Review { review_request }).await {
                            Ok(turn_id) => {
                                self.send_response(
                                    request_id,
                                    serde_json::json!({
                                        "status": "ok",
                                        "turnId": turn_id,
                                    }),
                                )
                                .await;
                            }
                            Err(err) => {
                                self.send_error(
                                    request_id,
                                    INTERNAL_ERROR_CODE,
                                    format!("failed to start review: {err}"),
                                )
                                .await;
                            }
                        }
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INVALID_REQUEST_ERROR_CODE,
                            format!("thread not found: {err}"),
                        )
                        .await;
                    }
                }
            }

            // === Config ===
            ClientRequest::ConfigRead { request_id, params } => {
                info!("gateway: config/read");
                match self.config_service.read(params).await {
                    Ok(response) => {
                        let result = serde_json::to_value(&response).unwrap_or(Value::Null);
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("config read failed: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::ConfigValueWrite { request_id, params } => {
                info!("gateway: config/valueWrite");
                match self.config_service.write_value(params).await {
                    Ok(response) => {
                        let result = serde_json::to_value(&response).unwrap_or(Value::Null);
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("config write failed: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::ConfigBatchWrite { request_id, params } => {
                info!("gateway: config/batchWrite");
                match self.config_service.batch_write(params).await {
                    Ok(response) => {
                        let result = serde_json::to_value(&response).unwrap_or(Value::Null);
                        self.send_response(request_id, result).await;
                    }
                    Err(err) => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            format!("config batch write failed: {err}"),
                        )
                        .await;
                    }
                }
            }

            ClientRequest::ConfigRequirementsRead {
                request_id,
                params: _,
            } => {
                info!("gateway: config/requirementsRead");
                match self.cloud_requirements.get().await {
                    Some(req) => {
                        // Build a JSON representation of the requirements manually,
                        // since ConfigRequirementsToml doesn't derive Serialize.
                        let result = serde_json::json!({
                            "allowed_approval_policies": req.allowed_approval_policies.as_ref().map(|v| v.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>()),
                            "allowed_sandbox_modes": req.allowed_sandbox_modes.as_ref().map(|v| v.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>()),
                            "enforce_residency": req.enforce_residency.as_ref().map(|r| format!("{r:?}")),
                        });
                        self.send_response(
                            request_id,
                            serde_json::json!({ "requirements": result }),
                        )
                        .await;
                    }
                    None => {
                        self.send_response(request_id, serde_json::json!({ "requirements": null }))
                            .await;
                    }
                }
            }

            // === Auth / Account ===
            ClientRequest::Initialize {
                request_id,
                params: _,
            } => {
                let result = serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "capabilities": {},
                });
                self.send_response(request_id, result).await;
            }

            ClientRequest::LoginAccount { request_id, params } => {
                self.handle_login_account(request_id, params).await;
            }

            ClientRequest::CancelLoginAccount { request_id, params } => {
                self.handle_cancel_login_account(request_id, params).await;
            }

            ClientRequest::LogoutAccount {
                request_id,
                params: _,
            } => {
                self.send_error(
                    request_id,
                    METHOD_NOT_FOUND_ERROR_CODE,
                    "account logout is not supported in gateway mode".to_owned(),
                )
                .await;
            }

            ClientRequest::GetAccount {
                request_id,
                params: _,
            } => {
                // Return gateway-level account info.
                let result = serde_json::json!({
                    "status": "gateway",
                    "version": env!("CARGO_PKG_VERSION"),
                    "mode": "gateway",
                });
                self.send_response(request_id, result).await;
            }

            ClientRequest::GetAccountRateLimits {
                request_id,
                params: _,
            } => {
                // Gateway doesn't expose per-account rate limits from providers.
                let result = serde_json::json!({ "rateLimits": {} });
                self.send_response(request_id, result).await;
            }

            // === MCP ===
            ClientRequest::McpServerOauthLogin {
                request_id,
                params: _,
            } => {
                self.send_error(
                    request_id,
                    METHOD_NOT_FOUND_ERROR_CODE,
                    "MCP OAuth login is not supported in gateway mode".to_owned(),
                )
                .await;
            }

            ClientRequest::McpServerRefresh {
                request_id,
                params: _,
            } => {
                info!("gateway: mcp/refresh");
                // MCP server refresh is not fully supported in gateway mode.
                // The gateway doesn't maintain MCP server connections like the app-server.
                self.send_response(request_id, serde_json::json!({ "status": "not_applicable", "reason": "gateway does not manage MCP server connections" })).await;
            }

            ClientRequest::McpServerStatusList {
                request_id,
                params: _,
            } => {
                // MCP server status listing not yet available in gateway context.
                let result = serde_json::json!({ "statuses": [] });
                self.send_response(request_id, result).await;
            }

            // === Feedback ===
            ClientRequest::FeedbackUpload { request_id, params } => {
                info!(
                    "gateway: feedback/upload (classification={})",
                    params.classification
                );
                // Attempt to upload feedback using the feedback client.
                let session_id = params.session_id.clone();
                let classification = params.classification.clone();
                let reason = params.reason.clone();
                let result = serde_json::json!({
                    "status": "received",
                    "classification": classification,
                    "reason": reason,
                    "session_id": session_id,
                });
                self.send_response(request_id, result).await;
            }

            // === One-off command exec ===
            ClientRequest::OneOffCommandExec {
                request_id,
                params: _,
            } => {
                self.send_error(
                    request_id,
                    METHOD_NOT_FOUND_ERROR_CODE,
                    "one-off command execution is not supported in gateway mode".to_owned(),
                )
                .await;
            }

            // === Experimental / Mock ===
            ClientRequest::MockExperimentalMethod {
                request_id,
                params: _,
            } => {
                let result = serde_json::json!({ "status": "ok" });
                self.send_response(request_id, result).await;
            }

            ClientRequest::FuzzyFileSearch { request_id, .. } => {
                self.send_error(
                    request_id,
                    METHOD_NOT_FOUND_ERROR_CODE,
                    "fuzzy file search is not supported in gateway mode".to_owned(),
                )
                .await;
            }
        }
    }

    /// Process a JSON-RPC response from a client (answering a server request).
    pub(crate) async fn process_client_response(&self, id: RequestId, result: Value) {
        let mut pending = self.pending_requests.lock().await;
        if let Some(tx) = pending.remove(&id) {
            if let Err(err) = tx.send(result) {
                warn!("failed to deliver client response for {id:?}: {err:?}");
            }
        } else {
            warn!("received response for unknown request {id:?}");
        }
    }

    /// Send a server->client request (e.g. approval) and wait for a response.
    pub(crate) async fn send_server_request(
        &self,
        payload: ServerRequestPayload,
    ) -> oneshot::Receiver<Value> {
        let id = RequestId::String(uuid::Uuid::now_v7().to_string());
        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending_requests.lock().await;
            pending.insert(id.clone(), tx);
        }

        let request = payload.request_with_id(id);
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::ServerRequest(request))
            .await
        {
            warn!("failed to send server request: {err}");
        }

        rx
    }

    /// Broadcast a notification to all subscribed clients for a thread.
    pub(crate) async fn broadcast_thread_event(
        &self,
        session_id: &str,
        notification: &ServerNotification,
    ) {
        let event = match serde_json::to_value(notification) {
            Ok(v) => v,
            Err(err) => {
                error!("failed to serialize notification: {err}");
                return;
            }
        };

        let method = event
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();

        let params = event.get("params").cloned().unwrap_or(Value::Null);

        self.websocket_manager
            .broadcast_to_session(session_id, &method, params)
            .await;
    }

    pub(in crate::channel) async fn send_response(&self, id: RequestId, result: Value) {
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::Response { id, result })
            .await
        {
            warn!("failed to send response: {err}");
        }
    }

    pub(in crate::channel) async fn send_error(&self, id: RequestId, code: i64, message: String) {
        let error = JSONRPCErrorError {
            code,
            message,
            data: None,
        };
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::Error { id, error })
            .await
        {
            warn!("failed to send error: {err}");
        }
    }

    pub(in crate::channel) async fn send_notification(&self, method: &str, params: Value) {
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::Notification {
                method: method.to_owned(),
                params: Some(params),
            })
            .await
        {
            warn!("failed to send notification: {err}");
        }
    }
}
