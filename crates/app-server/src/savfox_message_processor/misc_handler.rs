use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use savfox_app_server_protocol::{
    CollaborationModeListParams, CollaborationModeListResponse, CommandExecParams,
    CommandExecResponse, FeedbackUploadParams, FeedbackUploadResponse, FuzzyFileSearchParams,
    FuzzyFileSearchResponse, JSONRPCErrorError, MockExperimentalMethodParams,
    MockExperimentalMethodResponse, ModelListParams, ModelListResponse, RequestId,
};
use savfox_core::SessionManager;
use savfox_core::config::Config;
use savfox_core::exec::ExecParams;
use savfox_core::exec_env::create_env;
use savfox_core::features::Feature;
use savfox_core::sandboxing::SandboxPermissions;
use savfox_core::windows_sandbox::WindowsSandboxLevelExt;
use savfox_protocol::SessionId;
use savfox_protocol::config_types::WindowsSandboxLevel;

use super::{INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE, SavfoxMessageProcessor};
use crate::fuzzy_file_search::run_fuzzy_file_search;
use crate::models::supported_models;
use crate::outgoing_message::OutgoingMessageSender;

impl SavfoxMessageProcessor {
    pub(crate) async fn exec_one_off_command(
        &self,
        request_id: RequestId,
        params: CommandExecParams,
    ) {
        tracing::debug!("ExecOneOffCommand params: {params:?}");

        if params.command.is_empty() {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "command must not be empty".to_owned(),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        let cwd = params.cwd.unwrap_or_else(|| self.config.cwd.clone());
        let env = create_env(&self.config.shell_environment_policy);
        let timeout_ms = params
            .timeout_ms
            .and_then(|timeout_ms| u64::try_from(timeout_ms).ok());
        let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
        let exec_params = ExecParams {
            command: params.command,
            cwd,
            expiration: timeout_ms.into(),
            env,
            sandbox_permissions: SandboxPermissions::UseDefault,
            windows_sandbox_level,
            justification: None,
            arg0: None,
        };

        let requested_policy = params.sandbox_policy.map(|policy| policy.to_core());
        let effective_policy = match requested_policy {
            Some(policy) => match self.config.sandbox_policy.can_set(&policy) {
                Ok(()) => policy,
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid sandbox policy: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            },
            None => self.config.sandbox_policy.get().clone(),
        };

        let savfox_linux_sandbox_exe = self.config.savfox_linux_sandbox_exe.clone();
        let outgoing = self.outgoing.clone();
        let req_id = request_id;
        let sandbox_cwd = self.config.cwd.clone();

        tokio::spawn(async move {
            match savfox_core::exec::process_exec_tool_call(
                exec_params,
                &effective_policy,
                sandbox_cwd.as_path(),
                &savfox_linux_sandbox_exe,
                None,
            )
            .await
            {
                Ok(output) => {
                    let response = CommandExecResponse {
                        exit_code: output.exit_code,
                        stdout: output.stdout.text,
                        stderr: output.stderr.text,
                    };
                    outgoing.send_response(req_id, response).await;
                }
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INTERNAL_ERROR_CODE,
                        message: format!("exec failed: {err}"),
                        data: None,
                    };
                    outgoing.send_error(req_id, error).await;
                }
            }
        });
    }

    pub(crate) async fn list_models(
        outgoing: Arc<OutgoingMessageSender>,
        session_manager: Arc<SessionManager>,
        config: Arc<Config>,
        request_id: RequestId,
        params: ModelListParams,
    ) {
        let ModelListParams { limit, cursor } = params;
        let mut config = (*config).clone();
        config.features.enable(Feature::RemoteModels);
        let models = supported_models(session_manager, &config).await;
        let total = models.len();

        if total == 0 {
            let response = ModelListResponse {
                data: Vec::new(),
                next_cursor: None,
            };
            outgoing.send_response(request_id, response).await;
            return;
        }

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => {
                if let Ok(idx) = cursor.parse::<usize>() {
                    idx
                } else {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid cursor: {cursor}"),
                        data: None,
                    };
                    outgoing.send_error(request_id, error).await;
                    return;
                }
            }
            None => 0,
        };

        if start > total {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("cursor {start} exceeds total models {total}"),
                data: None,
            };
            outgoing.send_error(request_id, error).await;
            return;
        }

        let end = start.saturating_add(effective_limit).min(total);
        let items = models[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };
        let response = ModelListResponse {
            data: items,
            next_cursor,
        };
        outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn list_collaboration_modes(
        outgoing: Arc<OutgoingMessageSender>,
        session_manager: Arc<SessionManager>,
        request_id: RequestId,
        params: CollaborationModeListParams,
    ) {
        let CollaborationModeListParams {} = params;
        let items = session_manager.list_collaboration_modes();
        let response = CollaborationModeListResponse { data: items };
        outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn mock_experimental_method(
        &self,
        request_id: RequestId,
        params: MockExperimentalMethodParams,
    ) {
        let MockExperimentalMethodParams { value } = params;
        let response = MockExperimentalMethodResponse { echoed: value };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn fuzzy_file_search(
        &mut self,
        request_id: RequestId,
        params: FuzzyFileSearchParams,
    ) {
        let FuzzyFileSearchParams {
            query,
            roots,
            cancellation_token,
        } = params;

        let cancel_flag = match cancellation_token.clone() {
            Some(token) => {
                let mut pending_fuzzy_searches = self.pending_fuzzy_searches.lock().await;
                // if a cancellation_token is provided and a pending_request exists for
                // that token, cancel it
                if let Some(existing) = pending_fuzzy_searches.get(&token) {
                    existing.store(true, Ordering::Relaxed);
                }
                let flag = Arc::new(AtomicBool::new(false));
                pending_fuzzy_searches.insert(token.clone(), flag.clone());
                flag
            }
            None => Arc::new(AtomicBool::new(false)),
        };

        let results = match query.as_str() {
            "" => vec![],
            _ => run_fuzzy_file_search(query, roots, cancel_flag.clone()).await,
        };

        if let Some(token) = cancellation_token {
            let mut pending_fuzzy_searches = self.pending_fuzzy_searches.lock().await;
            if let Some(current_flag) = pending_fuzzy_searches.get(&token)
                && Arc::ptr_eq(current_flag, &cancel_flag)
            {
                pending_fuzzy_searches.remove(&token);
            }
        }

        let response = FuzzyFileSearchResponse { files: results };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn upload_feedback(
        &self,
        request_id: RequestId,
        params: FeedbackUploadParams,
    ) {
        if !self.config.feedback_enabled {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "sending feedback is disabled by configuration".to_owned(),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        let FeedbackUploadParams {
            classification,
            reason,
            session_id,
            include_logs,
        } = params;

        let conversation_id = match session_id.as_deref() {
            Some(session_id) => match SessionId::from_string(session_id) {
                Ok(conversation_id) => Some(conversation_id),
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: format!("invalid session id: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            },
            None => None,
        };

        let snapshot = self.feedback.snapshot(conversation_id);
        let session_id = snapshot.session_id.clone();

        let validated_rollout_path = if include_logs {
            match conversation_id {
                Some(conv_id) => self.resolve_rollout_path(conv_id).await,
                None => None,
            }
        } else {
            None
        };
        let session_source = self.session_manager.session_source();

        let upload_result = tokio::task::spawn_blocking(move || {
            let rollout_path_ref = validated_rollout_path.as_deref();
            snapshot.upload_feedback(
                &classification,
                reason.as_deref(),
                include_logs,
                rollout_path_ref,
                Some(session_source),
            )
        })
        .await;

        let upload_result = match upload_result {
            Ok(result) => result,
            Err(join_err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to upload feedback: {join_err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        match upload_result {
            Ok(()) => {
                let response = FeedbackUploadResponse { session_id };
                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to upload feedback: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    async fn resolve_rollout_path(&self, conversation_id: SessionId) -> Option<PathBuf> {
        match self.session_manager.get_session(conversation_id).await {
            Ok(conv) => conv.rollout_path(),
            Err(_) => None,
        }
    }
}
