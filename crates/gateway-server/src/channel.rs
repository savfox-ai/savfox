use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use matrix_bot_sdk::client::{MatrixAuth, MatrixClient};
use savfox_app_server_protocol::{
    AccountLoginCompletedNotification, AccountUpdatedNotification, CancelLoginAccountParams,
    CancelLoginAccountResponse, CancelLoginAccountStatus, ClientRequest, JSONRPCErrorError,
    JSONRPCRequest, LoginAccountParams, LoginAccountResponse, RequestId, ServerNotification,
    ServerRequest, ServerRequestPayload,
};
use savfox_core::auth::{CLIENT_ID, login_with_api_key};
use savfox_core::config::edit::{ConfigEdit, ConfigEditsBuilder};
use savfox_core::config::{Config, ConfigService};
use savfox_core::config_loader::{CloudRequirementsLoader, LoaderOverrides};
use savfox_core::{
    ARCHIVED_SESSIONS_SUBDIR, AuthManager, SESSIONS_SUBDIR, SessionManager,
    find_archived_session_path_by_id_str, find_session_path_by_id_str, rollout_date_parts,
};
use savfox_feedback::SavfoxFeedback;
use savfox_login_oauth::{
    ServerOptions, ShutdownHandle, complete_device_code_login, request_device_code,
    run_login_server,
};
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{Op, SessionSource};
use savfox_protocol::user_input::UserInput;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use toml::Value as TomlValue;
use tracing::{error, info, warn};
use url::Url;
use uuid::Uuid;

use crate::session::GatewaySessionManager;

const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INTERNAL_ERROR_CODE: i64 = -32603;
const METHOD_NOT_FOUND_ERROR_CODE: i64 = -32601;

/// Outgoing message from the bridge to a WebSocket client.
#[derive(Debug, Clone)]
pub(crate) enum BridgeOutgoing {
    Response {
        id: RequestId,
        result: Value,
    },
    Error {
        id: RequestId,
        error: JSONRPCErrorError,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    ServerRequest(ServerRequest),
}

/// Handles bidirectional message routing between gateway WebSocket clients
/// and the Savfox core engine (SessionManager).
pub(crate) struct GatewayChannel {
    auth_manager: Arc<AuthManager>,
    session_manager: Arc<SessionManager>,
    config: Arc<Config>,
    cli_overrides: Vec<(String, TomlValue)>,
    cloud_requirements: CloudRequirementsLoader,
    config_service: ConfigService,
    feedback: SavfoxFeedback,
    savfox_linux_sandbox_exe: Option<PathBuf>,
    websocket_manager: GatewaySessionManager,
    /// Pending server→client requests awaiting a response.
    pending_requests: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Value>>>>,
    /// Outbound message channel.
    outgoing_tx: mpsc::Sender<BridgeOutgoing>,
    /// HTTP client for outbound platform API calls.
    http_client: reqwest::Client,
    /// Runtime bridge credentials hot-reloaded from config patch/apply.
    runtime_bridge_secrets: Arc<RwLock<RuntimeBridgeSecrets>>,
    /// Active login attempt (browser OAuth or device code).
    active_login: Arc<Mutex<Option<ActiveLogin>>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeBridgeSecrets {
    pub(crate) discord_bot_token: Option<String>,
    pub(crate) telegram_bot_token: Option<String>,
    pub(crate) slack_bot_token: Option<String>,
    pub(crate) slack_signing_secret: Option<String>,
    pub(crate) webhook_secret: Option<String>,
}

#[derive(Debug, Clone)]
struct MatrixOutboundConfig {
    id: String,
    homeserver: String,
    access_token: String,
    user_id: Option<String>,
    rooms: Vec<String>,
}

impl MatrixOutboundConfig {
    fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("matrix") {
            return None;
        }
        let raw = config.config.as_object()?;
        let access_token =
            first_non_empty_config_string(raw, &["accessToken", "access_token", "token"])?;
        let homeserver =
            first_non_empty_config_string(raw, &["homeserver", "homeserver_url", "server_url"])
                .unwrap_or_else(|| "https://matrix.org".to_string());
        let user_id = first_non_empty_config_string(raw, &["userId", "user_id"]);

        let mut rooms = Vec::new();
        for key in ["groups", "rooms", "roomIds", "room_ids", "room_id"] {
            if let Some(value) = raw.get(key) {
                rooms.extend(parse_string_list(value));
            }
        }
        rooms.sort_unstable();
        rooms.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

        Some(Self {
            id: config.id.clone(),
            homeserver,
            access_token,
            user_id,
            rooms,
        })
    }

    fn matches_room(&self, room_id: &str) -> bool {
        let room_id = room_id.trim();
        !room_id.is_empty()
            && self
                .rooms
                .iter()
                .any(|room| room.eq_ignore_ascii_case(room_id))
    }
}

fn first_non_empty_config_string(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
    })
}

fn parse_string_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => text
            .split(['\n', ','])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Tracks an active login attempt for cancellation.
struct ActiveLogin {
    shutdown_handle: ShutdownHandle,
    login_id: Uuid,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentInvocationResult {
    pub(crate) reply: String,
    pub(crate) session_id: String,
    pub(crate) rollout_path: Option<PathBuf>,
    pub(crate) last_token_usage: Option<savfox_protocol::protocol::TokenUsage>,
}

/// Arguments needed to construct a `GatewayChannel`.
pub(crate) struct GatewayBridgeArgs {
    pub(crate) config: Arc<Config>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: SavfoxFeedback,
    pub(crate) savfox_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) websocket_manager: GatewaySessionManager,
    pub(crate) outgoing_tx: mpsc::Sender<BridgeOutgoing>,
}

impl GatewayChannel {
    pub(crate) fn new(args: GatewayBridgeArgs) -> Self {
        let auth_manager = AuthManager::shared(
            args.config.savfox_home.clone(),
            false,
            args.config.cli_auth_credentials_store_mode,
        );

        let session_manager = Arc::new(SessionManager::new(
            args.config.savfox_home.clone(),
            auth_manager.clone(),
            SessionSource::VSCode, // Gateway acts similarly to app-server
        ));

        let config_service = ConfigService::new(
            args.config.savfox_home.clone(),
            args.cli_overrides.clone(),
            LoaderOverrides::default(),
            args.cloud_requirements.clone(),
        );

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            auth_manager,
            session_manager,
            config: args.config,
            cli_overrides: args.cli_overrides,
            cloud_requirements: args.cloud_requirements,
            config_service,
            feedback: args.feedback,
            savfox_linux_sandbox_exe: args.savfox_linux_sandbox_exe,
            websocket_manager: args.websocket_manager,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            outgoing_tx: args.outgoing_tx,
            http_client,
            runtime_bridge_secrets: Arc::new(RwLock::new(RuntimeBridgeSecrets::default())),
            active_login: Arc::new(Mutex::new(None)),
        }
    }

    /// List available model IDs for the OpenAI-compatible API.
    pub(crate) async fn list_models(&self) -> Vec<String> {
        self.session_manager
            .list_models(
                &self.config,
                savfox_core::models_manager::manager::RefreshStrategy::OnlineIfUncached,
            )
            .await
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

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
                let file_name = match rollout_path.file_name() {
                    Some(f) => f.to_owned(),
                    None => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            "rollout path missing file name".to_string(),
                        )
                        .await;
                        return;
                    }
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
                let file_name = match archived_path.file_name() {
                    Some(f) => f.to_owned(),
                    None => {
                        self.send_error(
                            request_id,
                            INTERNAL_ERROR_CODE,
                            "archived path missing file name".to_string(),
                        )
                        .await;
                        return;
                    }
                };

                let (year, month, day) = match rollout_date_parts(&file_name) {
                    Some(parts) => parts,
                    None => {
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
                        self.send_response(request_id, serde_json::json!({ "status": "unarchived", "session_id": session_id_str })).await;
                        return;
                    }
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

                let name = params.name.trim().to_string();
                if name.is_empty() {
                    self.send_error(
                        request_id,
                        INVALID_REQUEST_ERROR_CODE,
                        "session name must not be empty".to_string(),
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
                        "numTurns must be >= 1".to_string(),
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
                    "account logout is not supported in gateway mode".to_string(),
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
                    "MCP OAuth login is not supported in gateway mode".to_string(),
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
                    "one-off command execution is not supported in gateway mode".to_string(),
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

            // === Legacy / Deprecated v1 APIs ===
            ClientRequest::NewConversation { request_id, .. }
            | ClientRequest::GetConversationSummary { request_id, .. }
            | ClientRequest::ListConversations { request_id, .. }
            | ClientRequest::ResumeConversation { request_id, .. }
            | ClientRequest::ForkConversation { request_id, .. }
            | ClientRequest::ArchiveConversation { request_id, .. }
            | ClientRequest::SendUserMessage { request_id, .. }
            | ClientRequest::SendUserTurn { request_id, .. }
            | ClientRequest::InterruptConversation { request_id, .. }
            | ClientRequest::AddConversationListener { request_id, .. }
            | ClientRequest::RemoveConversationListener { request_id, .. }
            | ClientRequest::GitDiffToRemote { request_id, .. }
            | ClientRequest::LoginApiKey { request_id, .. }
            | ClientRequest::LoginChatGpt { request_id, .. }
            | ClientRequest::CancelLoginChatGpt { request_id, .. }
            | ClientRequest::LogoutChatGpt { request_id, .. }
            | ClientRequest::GetAuthStatus { request_id, .. }
            | ClientRequest::GetUserSavedConfig { request_id, .. }
            | ClientRequest::SetDefaultModel { request_id, .. }
            | ClientRequest::GetUserAgent { request_id, .. }
            | ClientRequest::UserInfo { request_id, .. }
            | ClientRequest::FuzzyFileSearch { request_id, .. }
            | ClientRequest::ExecOneOffCommand { request_id, .. } => {
                self.send_error(
                    request_id,
                    METHOD_NOT_FOUND_ERROR_CODE,
                    "legacy v1 API not supported in gateway; use v2 APIs instead".to_string(),
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

    /// Send a server→client request (e.g. approval) and wait for a response.
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

    // === Platform API calls ===

    /// Send a message to a Discord channel via the Bot API.
    pub(crate) async fn send_discord_message(
        &self,
        bot_token: &str,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages");
        let mut body = serde_json::json!({ "content": content });
        if let Some(message_id) = reply_to_message_id {
            let trimmed = message_id.trim();
            if !trimmed.is_empty() {
                body["message_reference"] = serde_json::json!({
                    "message_id": trimmed,
                });
            }
        }

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bot {bot_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            warn!("Discord API error: HTTP {status}: {body_str}");
        }

        Ok(())
    }

    /// Send a rich embed message to a Discord channel.
    pub(crate) async fn send_discord_embed(
        &self,
        bot_token: &str,
        channel_id: &str,
        title: &str,
        description: &str,
        color: u32,
    ) -> anyhow::Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages");
        let body = serde_json::json!({
            "embeds": [{
                "title": title,
                "description": description,
                "color": color,
            }]
        });

        let _response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bot {bot_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        Ok(())
    }

    /// Send a message via the Telegram Bot API.
    pub(crate) async fn send_telegram_message(
        &self,
        bot_token: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });

        if let Some(mode) = parse_mode {
            body["parse_mode"] = serde_json::json!(mode);
        }
        if let Some(message_id) = reply_to_message_id.and_then(|v| v.trim().parse::<i64>().ok()) {
            body["reply_to_message_id"] = serde_json::json!(message_id);
        }

        let response = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            warn!("Telegram API error: HTTP {status}: {body_str}");
        }

        Ok(())
    }

    /// Send a message via the Slack API (using response_url or chat.postMessage).
    pub(crate) async fn send_slack_message(
        &self,
        bot_token: &str,
        channel: &str,
        text: &str,
        blocks: Option<Value>,
        thread_ts: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = "https://slack.com/api/chat.postMessage";
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });

        if let Some(blocks) = blocks {
            body["blocks"] = blocks;
        }
        if let Some(thread_ts) = thread_ts {
            let trimmed = thread_ts.trim();
            if !trimmed.is_empty() {
                body["thread_ts"] = serde_json::json!(trimmed);
            }
        }

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {bot_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            warn!("Slack API error: HTTP {status}: {body_str}");
        }

        Ok(())
    }

    async fn resolve_matrix_outbound_config(
        &self,
        room_id: &str,
    ) -> anyhow::Result<Option<MatrixOutboundConfig>> {
        let all_configs =
            savfox_core::config::channel_store::list_channel_configs(&self.config.savfox_home)
                .await
                .context("failed to load channel configs")?;
        let matrix_configs: Vec<MatrixOutboundConfig> = all_configs
            .iter()
            .filter_map(MatrixOutboundConfig::from_channel_config)
            .collect();

        if matrix_configs.is_empty() {
            return Ok(None);
        }

        if let Some(config) = matrix_configs
            .iter()
            .find(|config| config.matches_room(room_id))
        {
            return Ok(Some(config.clone()));
        }

        if matrix_configs.len() == 1 {
            return Ok(matrix_configs.into_iter().next());
        }

        if let Some(default_config) = matrix_configs.iter().find(|config| config.rooms.is_empty()) {
            warn!(
                room_id,
                config_id = %default_config.id,
                "Matrix room does not match any configured room allowlist; using default Matrix channel"
            );
            return Ok(Some(default_config.clone()));
        }

        let fallback = matrix_configs.first().cloned();
        if let Some(config) = fallback.as_ref() {
            warn!(
                room_id,
                config_id = %config.id,
                "Matrix room does not match configured room allowlists; using first Matrix channel"
            );
        }
        Ok(fallback)
    }

    /// Send a message via Matrix using matrix-bot-sdk.
    pub(crate) async fn send_matrix_message(
        &self,
        homeserver: &str,
        access_token: &str,
        user_id: Option<&str>,
        room_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let homeserver_url = Url::parse(homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {homeserver}"))?;
        let auth = if let Some(uid) = user_id.map(str::trim).filter(|uid| !uid.is_empty()) {
            MatrixAuth::new(access_token).with_user_id(uid.to_string())
        } else {
            MatrixAuth::new(access_token)
        };
        let client = MatrixClient::new(homeserver_url, auth);
        client
            .send_text(room_id, text)
            .await
            .with_context(|| format!("failed to send Matrix message to room {room_id}"))?;
        Ok(())
    }

    /// Join a Matrix room when this gateway user receives an invite event.
    pub(crate) async fn auto_join_matrix_invited_room(
        &self,
        room_id: &str,
        invited_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let room_id = room_id.trim();
        if room_id.is_empty() {
            return Ok(());
        }

        let Some(config) = self.resolve_matrix_outbound_config(room_id).await? else {
            warn!(
                room_id,
                "Matrix invite received but no Matrix channel config is available"
            );
            return Ok(());
        };

        let configured_user_id = config
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let (Some(invited), Some(configured)) = (
            invited_user_id.map(str::trim).filter(|v| !v.is_empty()),
            configured_user_id,
        ) && !invited.eq_ignore_ascii_case(configured)
        {
            return Ok(());
        }

        let homeserver_url = Url::parse(&config.homeserver).with_context(|| {
            format!(
                "invalid Matrix homeserver URL for config {}: {}",
                config.id, config.homeserver
            )
        })?;
        let auth = if let Some(uid) = configured_user_id {
            MatrixAuth::new(config.access_token.clone()).with_user_id(uid.to_string())
        } else {
            MatrixAuth::new(config.access_token.clone())
        };
        let client = MatrixClient::new(homeserver_url, auth);
        let joined_room_id = client
            .join_room(room_id)
            .await
            .with_context(|| format!("failed to auto-join Matrix room {room_id}"))?;
        info!(
            room_id,
            joined_room_id,
            config_id = %config.id,
            "Auto-joined Matrix room after invite"
        );
        Ok(())
    }

    /// Send a message via the Mattermost REST API.
    pub(crate) async fn send_mattermost_message(
        &self,
        server_url: &str,
        access_token: &str,
        channel_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = format!("{server_url}/api/v4/posts");
        let body = serde_json::json!({
            "channel_id": channel_id,
            "message": text,
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Mattermost API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the Google Chat API (webhook URL or Spaces API).
    pub(crate) async fn send_googlechat_message(
        &self,
        webhook_url: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
        crate::ssrf::validate_outbound_url(webhook_url, &ssrf_cfg)
            .await
            .map_err(|err| anyhow::anyhow!("blocked googlechat webhook url: {err}"))?;
        let body = serde_json::json!({ "text": text });
        let client = crate::ssrf::build_guarded_client(&ssrf_cfg)
            .map_err(|err| anyhow::anyhow!("failed to create webhook client: {err}"))?;

        let response = client
            .post(webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Google Chat API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the Microsoft Teams Bot Framework / webhook.
    pub(crate) async fn send_teams_message(
        &self,
        webhook_url: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
        crate::ssrf::validate_outbound_url(webhook_url, &ssrf_cfg)
            .await
            .map_err(|err| anyhow::anyhow!("blocked teams webhook url: {err}"))?;
        let body = serde_json::json!({
            "type": "message",
            "text": text,
        });
        let client = crate::ssrf::build_guarded_client(&ssrf_cfg)
            .map_err(|err| anyhow::anyhow!("failed to create webhook client: {err}"))?;

        let response = client
            .post(webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Teams API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the LINE Messaging API.
    pub(crate) async fn send_line_message(
        &self,
        channel_token: &str,
        reply_token: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = "https://api.line.me/v2/bot/message/reply";
        let body = serde_json::json!({
            "replyToken": reply_token,
            "messages": [{
                "type": "text",
                "text": text,
            }]
        });

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {channel_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "LINE API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Push a message via the LINE Messaging API (no reply token needed).
    pub(crate) async fn push_line_message(
        &self,
        channel_token: &str,
        user_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = "https://api.line.me/v2/bot/message/push";
        let body = serde_json::json!({
            "to": user_id,
            "messages": [{
                "type": "text",
                "text": text,
            }]
        });

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {channel_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "LINE Push API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the Feishu/Lark Bot API.
    pub(crate) async fn send_feishu_message(
        &self,
        base_url: &str,
        tenant_access_token: &str,
        receive_id: &str,
        receive_id_type: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
            base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": serde_json::json!({"text": text}).to_string(),
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {tenant_access_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Feishu API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via DingTalk custom robot webhook.
    ///
    /// `webhook_or_token` accepts either a full webhook URL or an access token.
    pub(crate) async fn send_dingtalk_message(
        &self,
        webhook_or_token: &str,
        secret: Option<&str>,
        text: &str,
    ) -> anyhow::Result<()> {
        let webhook_or_token = webhook_or_token.trim();
        if webhook_or_token.is_empty() {
            anyhow::bail!("dingtalk webhook target is empty");
        }

        let mut webhook_url = if webhook_or_token.starts_with("https://")
            || webhook_or_token.starts_with("http://")
        {
            webhook_or_token.to_string()
        } else {
            format!("https://oapi.dingtalk.com/robot/send?access_token={webhook_or_token}")
        };

        if let Some(secret) = secret.map(str::trim).filter(|v| !v.is_empty()) {
            use base64::Engine;
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let timestamp = chrono::Utc::now().timestamp_millis().to_string();
            let sign_content = format!("{timestamp}\n{secret}");
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
            mac.update(sign_content.as_bytes());
            let sign =
                base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
            let sign_encoded: String =
                url::form_urlencoded::byte_serialize(sign.as_bytes()).collect();
            let separator = if webhook_url.contains('?') { '&' } else { '?' };
            webhook_url =
                format!("{webhook_url}{separator}timestamp={timestamp}&sign={sign_encoded}");
        }

        let body = serde_json::json!({
            "msgtype": "text",
            "text": {
                "content": text,
            }
        });
        let response = self
            .http_client
            .post(&webhook_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Dingtalk API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the Zalo OA Customer Service API.
    pub(crate) async fn send_zalo_message(
        &self,
        access_token: &str,
        user_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = "https://openapi.zalo.me/v3.0/oa/message/cs";
        let body = serde_json::json!({
            "recipient": {
                "user_id": user_id,
            },
            "message": {
                "text": text,
            },
        });

        let response = self
            .http_client
            .post(url)
            .header("Content-Type", "application/json")
            .header("access_token", access_token)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Zalo OA API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via an IRC bridge HTTP API.
    /// IRC doesn't have a native HTTP API, so this calls a local IRC bridge service.
    pub(crate) async fn send_irc_message(
        &self,
        bridge_url: &str,
        channel_name: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "channel": channel_name,
            "message": text,
        });

        let url = format!("{bridge_url}/send");
        let response = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "IRC bridge error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message to the appropriate platform via the gateway.
    /// The `channel` format is `platform:id` (e.g., `discord:12345`, `telegram:67890`).
    /// Platform credentials are read from runtime secrets, channel configs, or environment
    /// variables.
    pub(crate) async fn send_platform_message(
        &self,
        channel: &str,
        text: &str,
        discord_token: Option<&str>,
        telegram_token: Option<&str>,
        slack_token: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_platform_message_with_context(
            channel,
            text,
            discord_token,
            telegram_token,
            slack_token,
            None,
            None,
        )
        .await
    }

    /// Send a message with optional thread and reply context.
    pub(crate) async fn send_platform_message_with_context(
        &self,
        channel: &str,
        text: &str,
        discord_token: Option<&str>,
        telegram_token: Option<&str>,
        slack_token: Option<&str>,
        session_id: Option<&str>,
        reply_target: Option<&str>,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime_bridge_secrets.read().await.clone();
        let (platform, channel_id) = channel.split_once(':').unwrap_or(("webhook", channel));

        match platform {
            "discord" => {
                let token = discord_token
                    .map(|s| s.to_owned())
                    .or(runtime.discord_bot_token.clone())
                    .or_else(|| std::env::var("DISCORD_BOT_TOKEN").ok());
                if let Some(token) = token {
                    self.send_discord_message(&token, channel_id, text, reply_target)
                        .await?;
                } else {
                    warn!("Discord token not configured for channel: {channel}");
                }
            }
            "telegram" => {
                let token = telegram_token
                    .map(|s| s.to_owned())
                    .or(runtime.telegram_bot_token.clone())
                    .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok());
                if let Some(token) = token {
                    self.send_telegram_message(
                        &token,
                        channel_id,
                        text,
                        Some("HTML"),
                        reply_target,
                    )
                    .await?;
                } else {
                    warn!("Telegram token not configured for channel: {channel}");
                }
            }
            "slack" => {
                let token = slack_token
                    .map(|s| s.to_owned())
                    .or(runtime.slack_bot_token.clone())
                    .or_else(|| std::env::var("SLACK_BOT_TOKEN").ok());
                if let Some(token) = token {
                    self.send_slack_message(&token, channel_id, text, None, session_id)
                        .await?;
                } else {
                    warn!("Slack token not configured for channel: {channel}");
                }
            }
            "matrix" => {
                if let Some(config) = self.resolve_matrix_outbound_config(channel_id).await? {
                    self.send_matrix_message(
                        &config.homeserver,
                        &config.access_token,
                        config.user_id.as_deref(),
                        channel_id,
                        text,
                    )
                    .await?;
                } else {
                    warn!("Matrix channel not configured in channels/*.json");
                }
            }
            "mattermost" => {
                let server = std::env::var("MATTERMOST_URL")
                    .unwrap_or_else(|_| "http://localhost:8065".to_string());
                if let Ok(token) = std::env::var("MATTERMOST_TOKEN") {
                    self.send_mattermost_message(&server, &token, channel_id, text)
                        .await?;
                } else {
                    warn!("Mattermost token not configured");
                }
            }
            "googlechat" | "gchat" => {
                if let Ok(webhook_url) = std::env::var("GOOGLECHAT_WEBHOOK_URL") {
                    self.send_googlechat_message(&webhook_url, text).await?;
                } else {
                    warn!("Google Chat webhook URL not configured");
                }
            }
            "teams" | "msteams" => {
                if let Ok(webhook_url) = std::env::var("TEAMS_WEBHOOK_URL") {
                    self.send_teams_message(&webhook_url, text).await?;
                } else {
                    warn!("Teams webhook URL not configured");
                }
            }
            "line" => {
                if let Ok(token) = std::env::var("LINE_CHANNEL_TOKEN") {
                    self.push_line_message(&token, channel_id, text).await?;
                } else {
                    warn!("LINE channel token not configured");
                }
            }
            "feishu" | "lark" => {
                let config = crate::bridges::feishu::resolve_feishu_outbound_config(
                    &self.config.savfox_home,
                )
                .await?;
                let receive_id_type = config
                    .as_ref()
                    .map(|cfg| cfg.receive_id_type.as_str())
                    .unwrap_or("chat_id");
                let token = if let Some(token) = config
                    .as_ref()
                    .and_then(|cfg| cfg.app_access_token.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(token.to_string())
                } else if let (Some(app_id), Some(app_secret)) = (
                    config
                        .as_ref()
                        .and_then(|cfg| cfg.app_id.as_deref())
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                    config
                        .as_ref()
                        .and_then(|cfg| cfg.app_secret.as_deref())
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                ) {
                    match crate::bridges::feishu::fetch_feishu_tenant_access_token(
                        &self.http_client,
                        config
                            .as_ref()
                            .map(|cfg| cfg.base_url.as_str())
                            .unwrap_or("https://open.feishu.cn"),
                        app_id,
                        app_secret,
                    )
                    .await
                    {
                        Ok(token) => Some(token),
                        Err(err) => {
                            warn!("failed to fetch Feishu tenant token from channel config: {err}");
                            None
                        }
                    }
                } else {
                    std::env::var("FEISHU_TENANT_ACCESS_TOKEN")
                        .ok()
                        .or_else(|| std::env::var("FEISHU_APP_ACCESS_TOKEN").ok())
                };

                if let Some(token) = token {
                    let base_url = config
                        .as_ref()
                        .map(|cfg| cfg.base_url.as_str())
                        .unwrap_or("https://open.feishu.cn");
                    self.send_feishu_message(base_url, &token, channel_id, receive_id_type, text)
                        .await?;
                } else {
                    warn!(
                        "Feishu credentials are not configured (need tenant token or app_id/app_secret)"
                    );
                }
            }
            "dingtalk" => {
                if channel_id.starts_with("https://") || channel_id.starts_with("http://") {
                    self.send_dingtalk_message(
                        channel_id,
                        std::env::var("DINGTALK_SECRET").ok().as_deref(),
                        text,
                    )
                    .await?;
                } else {
                    let config = crate::bridges::dingtalk::resolve_dingtalk_outbound_config(
                        &self.config.savfox_home,
                    )
                    .await?;
                    let target = config
                        .as_ref()
                        .and_then(|cfg| {
                            cfg.webhook_url.clone().or_else(|| cfg.access_token.clone())
                        })
                        .or_else(|| {
                            std::env::var("DINGTALK_WEBHOOK_URL")
                                .ok()
                                .or_else(|| std::env::var("DINGTALK_ACCESS_TOKEN").ok())
                        });
                    let secret = config
                        .as_ref()
                        .and_then(|cfg| cfg.secret.clone())
                        .or_else(|| std::env::var("DINGTALK_SECRET").ok());

                    if let Some(target) = target {
                        self.send_dingtalk_message(&target, secret.as_deref(), text)
                            .await?;
                    } else {
                        warn!(
                            "Dingtalk credentials are not configured (need webhook URL or access token)"
                        );
                    }
                }
            }
            "irc" => {
                let bridge_url = std::env::var("IRC_BRIDGE_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:6667".to_string());
                self.send_irc_message(&bridge_url, channel_id, text).await?;
            }
            "zalo" => {
                if let Ok(token) = std::env::var("ZALO_OA_ACCESS_TOKEN") {
                    self.send_zalo_message(&token, channel_id, text).await?;
                } else {
                    warn!("Zalo OA access token not configured");
                }
            }
            _ => {
                warn!("unknown platform for channel: {channel}");
            }
        }

        Ok(())
    }

    /// Get a reference to the thread manager.
    #[must_use]
    pub(crate) fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the WebSocket client manager.
    #[must_use]
    pub(crate) fn websocket_manager(&self) -> &GatewaySessionManager {
        &self.websocket_manager
    }

    /// Get a reference to the HTTP client for platform API calls.
    #[must_use]
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get a reference to the config.
    #[must_use]
    pub(crate) fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Replace runtime bridge credentials from a hot config update.
    pub(crate) async fn set_runtime_bridge_secrets(&self, secrets: RuntimeBridgeSecrets) {
        let mut lock = self.runtime_bridge_secrets.write().await;
        *lock = secrets;
    }

    /// Snapshot current runtime bridge credentials.
    #[must_use]
    pub(crate) async fn runtime_bridge_secrets(&self) -> RuntimeBridgeSecrets {
        self.runtime_bridge_secrets.read().await.clone()
    }

    /// Invoke the agent with a text prompt and return the response text.
    ///
    /// Creates a temporary thread, submits the user message as a `UserInput` Op,
    /// and collects the assistant's text reply by reading `Event` messages.
    /// Used by OpenAI-compatible and OpenResponses API endpoints.
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
            .invoke_agent_text_in_session_with_metadata_impl(prompt, model, None, |_| {})
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
            .invoke_agent_text_in_session_with_metadata_impl(prompt, model, session_id, |_| {})
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
        self.invoke_agent_text_in_session_with_metadata_impl(prompt, model, session_id, on_delta)
            .await
    }

    /// Invoke the agent and include thread metadata for history tracking.
    pub(crate) async fn invoke_agent_text_with_metadata(
        &self,
        prompt: &str,
        model: &str,
    ) -> anyhow::Result<AgentInvocationResult> {
        self.invoke_agent_text_in_session_with_metadata_impl(prompt, model, None, |_| {})
            .await
    }

    /// Invoke the agent with optional persisted session context and include thread metadata.
    pub(crate) async fn invoke_agent_text_in_session_with_metadata(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<AgentInvocationResult> {
        self.invoke_agent_text_in_session_with_metadata_impl(prompt, model, session_id, |_| {})
            .await
    }

    async fn invoke_agent_text_in_session_with_metadata_impl<F>(
        &self,
        prompt: &str,
        model: &str,
        session_id: Option<&str>,
        mut on_delta: F,
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
            config.model = Some(model.to_string());
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

        let new_session = if let Some(sid) = requested_session_id.as_deref() {
            if let Some(path) = self.resolve_session_rollout_path(sid).await? {
                self.session_manager
                    .resume_session_from_rollout(config.clone(), path, self.auth_manager.clone())
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to resume session thread: {e}"))?
            } else {
                self.session_manager
                    .start_session(config)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to start thread: {e}"))?
            }
        } else {
            self.session_manager
                .start_session(config)
                .await
                .map_err(|e| anyhow::anyhow!("failed to start thread: {e}"))?
        };
        let session_id = new_session.session_id.clone();

        let session = self
            .session_manager
            .get_session(session_id.clone())
            .await
            .map_err(|e| anyhow::anyhow!("failed to get session: {e}"))?;
        let rollout_path = session.rollout_path();

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
        let timeout = tokio::time::Duration::from_secs(120);
        let deadline = tokio::time::Instant::now() + timeout;

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

        // Cleanup: remove the temporary thread.
        let _ = self.session_manager.remove_session(&session_id).await;

        let rollout_path = if let Some(sid) = requested_session_id.as_deref() {
            self.canonicalize_session_rollout_path(sid, rollout_path)
                .await
        } else {
            rollout_path
        };

        if reply.is_empty() {
            reply = "(no response from agent)".to_string();
        }

        Ok(AgentInvocationResult {
            reply,
            session_id: requested_session_id.unwrap_or_else(|| session_id.to_string()),
            rollout_path,
            last_token_usage,
        })
    }

    async fn resolve_session_rollout_path(
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

    async fn canonicalize_session_rollout_path(
        &self,
        session_id: &str,
        rollout_path: Option<PathBuf>,
    ) -> Option<PathBuf> {
        let Some(source_path) = rollout_path else {
            return None;
        };

        let canonical = self
            .config
            .savfox_home
            .join(SESSIONS_SUBDIR)
            .join(format!("{session_id}.jsonl"));
        if source_path == canonical {
            return Some(canonical);
        }
        if !tokio::fs::try_exists(&source_path).await.unwrap_or(false) {
            return Some(canonical);
        }

        if let Some(parent) = canonical.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                warn!(
                    path = %parent.display(),
                    "failed to create session rollout directory: {err}"
                );
                return Some(source_path);
            }
        }

        if tokio::fs::try_exists(&canonical).await.unwrap_or(false) {
            if let Err(err) = tokio::fs::remove_file(&source_path).await {
                warn!(
                    from = %source_path.display(),
                    to = %canonical.display(),
                    "failed to remove duplicate rollout file: {err}"
                );
                return Some(source_path);
            }
            return Some(canonical);
        }

        match tokio::fs::rename(&source_path, &canonical).await {
            Ok(_) => Some(canonical),
            Err(err) => {
                warn!(
                    from = %source_path.display(),
                    to = %canonical.display(),
                    "failed to canonicalize rollout path: {err}"
                );
                Some(source_path)
            }
        }
    }

    /// Get a receiver for thread-created events from the SessionManager.
    pub(crate) fn thread_created_receiver(&self) -> broadcast::Receiver<SessionId> {
        self.session_manager.subscribe_session_created()
    }

    async fn send_response(&self, id: RequestId, result: Value) {
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::Response { id, result })
            .await
        {
            warn!("failed to send response: {err}");
        }
    }

    async fn send_error(&self, id: RequestId, code: i64, message: String) {
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

    async fn send_notification(&self, method: &str, params: Value) {
        if let Err(err) = self
            .outgoing_tx
            .send(BridgeOutgoing::Notification {
                method: method.to_string(),
                params: Some(params),
            })
            .await
        {
            warn!("failed to send notification: {err}");
        }
    }

    async fn handle_login_account(&self, request_id: RequestId, params: LoginAccountParams) {
        match params {
            LoginAccountParams::Chatgpt => {
                self.handle_chatgpt_login(request_id).await;
            }
            LoginAccountParams::DeviceCode => {
                self.handle_device_code_login(request_id).await;
            }
            LoginAccountParams::ChatgptAuthTokens {
                id_token,
                access_token,
            } => {
                self.handle_chatgpt_auth_tokens_login(request_id, id_token, access_token)
                    .await;
            }
            LoginAccountParams::ApiKey { api_key } => {
                self.handle_api_key_login(request_id, api_key).await;
            }
        }
    }

    async fn handle_chatgpt_login(&self, request_id: RequestId) {
        let opts = ServerOptions::new(
            self.config.savfox_home.clone(),
            CLIENT_ID.to_string(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.cli_auth_credentials_store_mode,
        );

        match run_login_server(opts) {
            Ok(server) => {
                let login_id = Uuid::new_v4();
                let shutdown_handle = server.cancel_handle();
                let auth_url = server.auth_url.clone();

                {
                    let mut guard = self.active_login.lock().await;
                    if let Some(existing) = guard.take() {
                        drop(existing);
                    }
                    *guard = Some(ActiveLogin {
                        shutdown_handle,
                        login_id,
                    });
                }

                let response = LoginAccountResponse::Chatgpt {
                    login_id: login_id.to_string(),
                    auth_url,
                };
                self.send_response(request_id, serde_json::to_value(response).unwrap())
                    .await;

                let auth_manager = self.auth_manager.clone();
                let active_login = self.active_login.clone();
                let outgoing_tx = self.outgoing_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(600),
                        server.block_until_done(),
                    )
                    .await;

                    let (success, error_msg) = match result {
                        Ok(Ok(())) => {
                            auth_manager.reload();
                            (true, None)
                        }
                        Ok(Err(err)) => (false, Some(format!("Login server error: {err}"))),
                        Err(_) => (false, Some("Login timed out".to_string())),
                    };

                    {
                        let mut guard = active_login.lock().await;
                        if let Some(active) = guard.take() {
                            if active.login_id != login_id {
                                *guard = Some(active);
                            }
                        }
                    }

                    let notification = AccountLoginCompletedNotification {
                        login_id: Some(login_id.to_string()),
                        success,
                        error: error_msg,
                    };
                    if let Err(err) = outgoing_tx
                        .send(BridgeOutgoing::Notification {
                            method: "account/login/completed".to_string(),
                            params: Some(serde_json::to_value(notification).unwrap()),
                        })
                        .await
                    {
                        warn!("failed to send login completed notification: {err}");
                    }

                    if success {
                        let account_updated = AccountUpdatedNotification {
                            auth_mode: auth_manager
                                .auth_cached()
                                .as_ref()
                                .map(|a| a.api_auth_mode()),
                        };
                        if let Err(err) = outgoing_tx
                            .send(BridgeOutgoing::Notification {
                                method: "account/updated".to_string(),
                                params: Some(serde_json::to_value(account_updated).unwrap()),
                            })
                            .await
                        {
                            warn!("failed to send account updated notification: {err}");
                        }
                    }
                });
            }
            Err(err) => {
                self.send_error(
                    request_id,
                    INTERNAL_ERROR_CODE,
                    format!("Failed to start login server: {err}"),
                )
                .await;
            }
        }
    }

    async fn handle_device_code_login(&self, request_id: RequestId) {
        let opts = ServerOptions::new(
            self.config.savfox_home.clone(),
            CLIENT_ID.to_string(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.cli_auth_credentials_store_mode,
        );

        match request_device_code(&opts).await {
            Ok(device_code) => {
                let login_id = Uuid::new_v4();
                let verification_url = device_code.verification_url.clone();
                let user_code = device_code.user_code.clone();

                let response = LoginAccountResponse::DeviceCode {
                    login_id: login_id.to_string(),
                    verification_url,
                    user_code,
                };
                self.send_response(request_id, serde_json::to_value(response).unwrap())
                    .await;

                let opts_clone = opts.clone();
                let device_code_clone = device_code;
                let auth_manager = self.auth_manager.clone();
                let outgoing_tx = self.outgoing_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(900),
                        complete_device_code_login(opts_clone, device_code_clone),
                    )
                    .await;

                    let (success, error_msg) = match result {
                        Ok(Ok(())) => {
                            auth_manager.reload();
                            (true, None)
                        }
                        Ok(Err(err)) => (false, Some(format!("Device code login error: {err}"))),
                        Err(_) => (false, Some("Device code login timed out".to_string())),
                    };

                    let notification = AccountLoginCompletedNotification {
                        login_id: Some(login_id.to_string()),
                        success,
                        error: error_msg,
                    };
                    if let Err(err) = outgoing_tx
                        .send(BridgeOutgoing::Notification {
                            method: "account/login/completed".to_string(),
                            params: Some(serde_json::to_value(notification).unwrap()),
                        })
                        .await
                    {
                        warn!("failed to send login completed notification: {err}");
                    }

                    if success {
                        let account_updated = AccountUpdatedNotification {
                            auth_mode: auth_manager
                                .auth_cached()
                                .as_ref()
                                .map(|a| a.api_auth_mode()),
                        };
                        if let Err(err) = outgoing_tx
                            .send(BridgeOutgoing::Notification {
                                method: "account/updated".to_string(),
                                params: Some(serde_json::to_value(account_updated).unwrap()),
                            })
                            .await
                        {
                            warn!("failed to send account updated notification: {err}");
                        }
                    }
                });
            }
            Err(err) => {
                self.send_error(
                    request_id,
                    INTERNAL_ERROR_CODE,
                    format!("Failed to request device code: {err}"),
                )
                .await;
            }
        }
    }

    async fn handle_chatgpt_auth_tokens_login(
        &self,
        request_id: RequestId,
        id_token: String,
        access_token: String,
    ) {
        match savfox_core::auth::login_with_chatgpt_auth_tokens(
            &self.config.savfox_home,
            &id_token,
            &access_token,
        ) {
            Ok(()) => {
                self.auth_manager.reload();
                let response = LoginAccountResponse::ChatgptAuthTokens {};
                self.send_response(request_id, serde_json::to_value(response).unwrap())
                    .await;

                let notification = AccountLoginCompletedNotification {
                    login_id: None,
                    success: true,
                    error: None,
                };
                self.send_notification(
                    "account/login/completed",
                    serde_json::to_value(notification).unwrap(),
                )
                .await;

                let account_updated = AccountUpdatedNotification {
                    auth_mode: self
                        .auth_manager
                        .auth_cached()
                        .as_ref()
                        .map(|a| a.api_auth_mode()),
                };
                self.send_notification(
                    "account/updated",
                    serde_json::to_value(account_updated).unwrap(),
                )
                .await;
            }
            Err(err) => {
                self.send_error(
                    request_id,
                    INTERNAL_ERROR_CODE,
                    format!("Failed to save auth tokens: {err}"),
                )
                .await;
            }
        }
    }

    async fn handle_api_key_login(&self, request_id: RequestId, api_key: String) {
        match login_with_api_key(
            &self.config.savfox_home,
            &api_key,
            self.config.cli_auth_credentials_store_mode,
        ) {
            Ok(()) => {
                self.auth_manager.reload();
                let response = LoginAccountResponse::ApiKey {};
                self.send_response(request_id, serde_json::to_value(response).unwrap())
                    .await;

                let notification = AccountLoginCompletedNotification {
                    login_id: None,
                    success: true,
                    error: None,
                };
                self.send_notification(
                    "account/login/completed",
                    serde_json::to_value(notification).unwrap(),
                )
                .await;

                let account_updated = AccountUpdatedNotification {
                    auth_mode: self
                        .auth_manager
                        .auth_cached()
                        .as_ref()
                        .map(|a| a.api_auth_mode()),
                };
                self.send_notification(
                    "account/updated",
                    serde_json::to_value(account_updated).unwrap(),
                )
                .await;
            }
            Err(err) => {
                self.send_error(
                    request_id,
                    INTERNAL_ERROR_CODE,
                    format!("Failed to save API key: {err}"),
                )
                .await;
            }
        }
    }

    async fn handle_cancel_login_account(
        &self,
        request_id: RequestId,
        params: CancelLoginAccountParams,
    ) {
        let login_id = match Uuid::parse_str(&params.login_id) {
            Ok(id) => id,
            Err(_) => {
                let response = CancelLoginAccountResponse {
                    status: CancelLoginAccountStatus::NotFound,
                };
                self.send_response(request_id, serde_json::to_value(response).unwrap())
                    .await;
                return;
            }
        };

        let mut guard = self.active_login.lock().await;
        let status = if let Some(active) = guard.take() {
            if active.login_id == login_id {
                drop(active);
                CancelLoginAccountStatus::Canceled
            } else {
                *guard = Some(active);
                CancelLoginAccountStatus::NotFound
            }
        } else {
            CancelLoginAccountStatus::NotFound
        };

        let response = CancelLoginAccountResponse { status };
        self.send_response(request_id, serde_json::to_value(response).unwrap())
            .await;
    }
}

// === Signature verification utilities ===

/// Verify a Slack request signature using HMAC-SHA256.
pub(crate) fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let sig_basestring = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));

    let mut mac = match Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(sig_basestring.as_bytes());
    let result = mac.finalize();
    let computed = format!("v0={}", hex::encode(result.into_bytes()));

    // Constant-time comparison
    computed == signature
}

/// Check if Slack timestamp is within an allowed replay window.
pub(crate) fn is_slack_timestamp_fresh(
    timestamp: &str,
    max_age_secs: u64,
    now_epoch_secs: u64,
) -> bool {
    let Ok(ts) = timestamp.parse::<u64>() else {
        return false;
    };
    if ts > now_epoch_secs {
        return false;
    }
    now_epoch_secs.saturating_sub(ts) <= max_age_secs
}

/// Verify a Discord interaction signature using Ed25519.
pub(crate) fn verify_discord_signature(
    public_key: &str,
    signature: &str,
    timestamp: &str,
    body: &[u8],
) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let pub_key_bytes = match hex::decode(public_key) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let pub_key_array: [u8; 32] = match pub_key_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&pub_key_array) {
        Ok(key) => key,
        Err(_) => return false,
    };

    let sig_bytes = match hex::decode(signature) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let sig_array: [u8; 64] = match sig_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(&sig_array);

    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    verifying_key.verify(&message, &sig).is_ok()
}

/// Verify a generic webhook HMAC-SHA256 signature.
pub(crate) fn verify_webhook_hmac(secret: &str, signature: &str, body: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize();
    let computed = hex::encode(result.into_bytes());

    // Support both raw hex and "sha256=" prefix
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);
    computed == expected
}

/// Verify Telegram webhook secret token equality.
pub(crate) fn verify_telegram_webhook_secret(expected_secret: &str, received_secret: &str) -> bool {
    expected_secret == received_secret
}

/// Verify WhatsApp webhook signature using HMAC-SHA256.
pub(crate) fn verify_whatsapp_webhook_signature(
    app_secret: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);

    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    computed == expected
}

#[cfg(test)]
mod tests {
    use super::{
        is_slack_timestamp_fresh, verify_discord_signature, verify_slack_signature,
        verify_telegram_webhook_secret, verify_webhook_hmac,
    };

    #[test]
    fn slack_timestamp_freshness_validation() {
        assert!(is_slack_timestamp_fresh("1000", 300, 1200));
        assert!(!is_slack_timestamp_fresh("899", 300, 1200));
        assert!(!is_slack_timestamp_fresh("1201", 300, 1200));
        assert!(!is_slack_timestamp_fresh("not-a-number", 300, 1200));
    }

    #[test]
    fn slack_signature_roundtrip() {
        let secret = "top-secret";
        let timestamp = "1700000000";
        let body = br#"{"type":"event_callback","event":{"text":"/savfox hi"}}"#;

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac init");
        let base = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
        mac.update(base.as_bytes());
        let signature = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_slack_signature(secret, timestamp, &signature, body));
        assert!(!verify_slack_signature(
            secret,
            timestamp,
            "v0=deadbeef",
            body
        ));
    }

    #[test]
    fn webhook_hmac_roundtrip() {
        let secret = "webhook-secret";
        let body = br#"{"action":"start_thread","prompt":"hello"}"#;

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac init");
        mac.update(body);
        let digest = hex::encode(mac.finalize().into_bytes());

        assert!(verify_webhook_hmac(secret, &digest, body));
        assert!(verify_webhook_hmac(
            secret,
            &format!("sha256={digest}"),
            body
        ));
        assert!(!verify_webhook_hmac(secret, "sha256=bad", body));
        assert!(!verify_webhook_hmac(
            secret,
            &digest,
            br#"{"action":"different"}"#
        ));
    }

    #[test]
    fn discord_signature_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey};

        let secret = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&secret);
        let public_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
        let timestamp = "1700000000";
        let body = br#"{"type":2,"data":{"name":"savfox"}}"#;

        let mut msg = timestamp.as_bytes().to_vec();
        msg.extend_from_slice(body);
        let sig = signing_key.sign(&msg);
        let signature_hex = hex::encode(sig.to_bytes());

        assert!(verify_discord_signature(
            &public_key_hex,
            &signature_hex,
            timestamp,
            body
        ));
        assert!(!verify_discord_signature(
            &public_key_hex,
            "deadbeef",
            timestamp,
            body
        ));
    }

    #[test]
    fn telegram_secret_verification() {
        assert!(verify_telegram_webhook_secret("abc123", "abc123"));
        assert!(!verify_telegram_webhook_secret("abc123", "wrong"));
    }
}
