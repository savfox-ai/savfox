use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParam, CallToolResult, ClientNotification, ClientRequest, ErrorCode, ErrorData,
    Implementation, InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, RequestId, ServerCapabilities,
};
use savfox_core::config::Config;
use savfox_core::default_client::{USER_AGENT_SUFFIX, get_savfox_user_agent};
use savfox_core::protocol::Submission;
use savfox_core::{AuthManager, SessionManager};
use savfox_protocol::SessionId;
use savfox_protocol::protocol::SessionSource;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task;

use crate::agent_tool_config::{
    SavfoxToolCallParam, SavfoxToolCallReplyParam, create_tool_for_savfox_tool_call_param,
    create_tool_for_savfox_tool_call_reply_param,
};
use crate::outgoing_message::OutgoingMessageSender;

pub(crate) struct MessageProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    initialized: bool,
    savfox_linux_sandbox_exe: Option<PathBuf>,
    session_manager: Arc<SessionManager>,
    running_requests_id_to_savfox_uuid: Arc<Mutex<HashMap<RequestId, SessionId>>>,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(
        outgoing: OutgoingMessageSender,
        savfox_linux_sandbox_exe: Option<PathBuf>,
        config: Arc<Config>,
    ) -> Self {
        let outgoing = Arc::new(outgoing);
        let auth_manager = AuthManager::shared(
            config.savfox_home.clone(),
            false,
            config.cli_auth_credentials_store_mode,
        );
        let session_manager = Arc::new(SessionManager::new(
            config.savfox_home.clone(),
            auth_manager,
            SessionSource::Mcp,
        ));
        Self {
            outgoing,
            initialized: false,
            savfox_linux_sandbox_exe,
            session_manager,
            running_requests_id_to_savfox_uuid: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn process_request(&mut self, request: JsonRpcRequest<ClientRequest>) {
        let request_id = request.id.clone();
        let client_request = request.request;

        match client_request {
            ClientRequest::InitializeRequest(params) => {
                self.handle_initialize(request_id, params.params).await;
            }
            ClientRequest::PingRequest(_params) => {
                self.handle_ping(request_id).await;
            }
            ClientRequest::ListResourcesRequest(params) => {
                self.handle_list_resources(params.params);
            }
            ClientRequest::ListResourceTemplatesRequest(params) => {
                self.handle_list_resource_templates(params.params);
            }
            ClientRequest::ReadResourceRequest(params) => {
                self.handle_read_resource(params.params);
            }
            ClientRequest::SubscribeRequest(params) => {
                self.handle_subscribe(params.params);
            }
            ClientRequest::UnsubscribeRequest(params) => {
                self.handle_unsubscribe(params.params);
            }
            ClientRequest::ListPromptsRequest(params) => {
                self.handle_list_prompts(params.params);
            }
            ClientRequest::GetPromptRequest(params) => {
                self.handle_get_prompt(params.params);
            }
            ClientRequest::ListToolsRequest(params) => {
                self.handle_list_tools(request_id, params.params).await;
            }
            ClientRequest::CallToolRequest(params) => {
                self.handle_call_tool(request_id, params.params).await;
            }
            ClientRequest::SetLevelRequest(params) => {
                self.handle_set_level(params.params);
            }
            ClientRequest::CompleteRequest(params) => {
                self.handle_complete(params.params);
            }
            ClientRequest::CustomRequest(custom) => {
                let method = custom.method.clone();
                self.outgoing
                    .send_error(
                        request_id,
                        ErrorData::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            format!("method not found: {method}"),
                            Some(json!({ "method": method })),
                        ),
                    )
                    .await;
            }
            _ => {
                self.outgoing
                    .send_error(
                        request_id,
                        ErrorData::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            "unsupported request".to_owned(),
                            None,
                        ),
                    )
                    .await;
            }
        }
    }

    pub(crate) async fn process_response(&mut self, response: JsonRpcResponse<serde_json::Value>) {
        tracing::info!("<- response: {:?}", response);
        let JsonRpcResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    pub(crate) async fn process_notification(
        &mut self,
        notification: JsonRpcNotification<ClientNotification>,
    ) {
        match notification.notification {
            ClientNotification::CancelledNotification(params) => {
                self.handle_cancelled_notification(params.params).await;
            }
            ClientNotification::ProgressNotification(params) => {
                self.handle_progress_notification(params.params);
            }
            ClientNotification::RootsListChangedNotification(_params) => {
                self.handle_roots_list_changed();
            }
            ClientNotification::InitializedNotification(_) => {
                self.handle_initialized_notification();
            }
            ClientNotification::CustomNotification(_) => {
                tracing::warn!("ignoring custom client notification");
            }
        }
    }

    pub(crate) fn process_error(&mut self, err: JsonRpcError) {
        tracing::error!("<- error: {:?}", err);
    }

    async fn handle_initialize(
        &mut self,
        id: RequestId,
        params: rmcp::model::InitializeRequestParam,
    ) {
        tracing::info!("initialize -> params: {:?}", params);

        if self.initialized {
            self.outgoing
                .send_error(
                    id,
                    ErrorData::invalid_request("initialize called more than once", None),
                )
                .await;
            return;
        }

        let client_info = params.client_info;
        let name = client_info.name;
        let version = client_info.version;
        let user_agent_suffix = format!("{name}; {version}");
        if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
            *suffix = Some(user_agent_suffix);
        }

        let server_info = Implementation::new("savfox-mcp-server", env!("CARGO_PKG_VERSION"))
            .with_title("Savfox");

        // Preserve Savfox's existing non-spec `serverInfo.user_agent` field.
        let mut server_info_value = match serde_json::to_value(&server_info) {
            Ok(value) => value,
            Err(err) => {
                self.outgoing
                    .send_error(
                        id,
                        ErrorData::internal_error(
                            format!("failed to serialize server info: {err}"),
                            None,
                        ),
                    )
                    .await;
                return;
            }
        };
        if let serde_json::Value::Object(ref mut obj) = server_info_value {
            obj.insert("user_agent".to_owned(), json!(get_savfox_user_agent()));
        }

        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build();
        let mut result_value = match serde_json::to_value(
            InitializeResult::new(capabilities)
                .with_server_info(server_info)
                .with_protocol_version(params.protocol_version.clone()),
        ) {
            Ok(value) => value,
            Err(err) => {
                self.outgoing
                    .send_error(
                        id,
                        ErrorData::internal_error(
                            format!("failed to serialize initialize response: {err}"),
                            None,
                        ),
                    )
                    .await;
                return;
            }
        };

        if let serde_json::Value::Object(ref mut obj) = result_value {
            obj.insert("serverInfo".to_owned(), server_info_value);
        }

        self.initialized = true;
        self.outgoing.send_response(id, result_value).await;
    }

    async fn handle_ping(&self, id: RequestId) {
        tracing::info!("ping");
        self.outgoing.send_response(id, json!({})).await;
    }

    fn handle_list_resources(&self, params: Option<rmcp::model::PaginatedRequestParam>) {
        tracing::info!("resources/list -> params: {:?}", params);
    }

    fn handle_list_resource_templates(&self, params: Option<rmcp::model::PaginatedRequestParam>) {
        tracing::info!("resources/templates/list -> params: {:?}", params);
    }

    fn handle_read_resource(&self, params: rmcp::model::ReadResourceRequestParam) {
        tracing::info!("resources/read -> params: {:?}", params);
    }

    fn handle_subscribe(&self, params: rmcp::model::SubscribeRequestParam) {
        tracing::info!("resources/subscribe -> params: {:?}", params);
    }

    fn handle_unsubscribe(&self, params: rmcp::model::UnsubscribeRequestParam) {
        tracing::info!("resources/unsubscribe -> params: {:?}", params);
    }

    fn handle_list_prompts(&self, params: Option<rmcp::model::PaginatedRequestParam>) {
        tracing::info!("prompts/list -> params: {:?}", params);
    }

    fn handle_get_prompt(&self, params: rmcp::model::GetPromptRequestParam) {
        tracing::info!("prompts/get -> params: {:?}", params);
    }

    async fn handle_list_tools(
        &self,
        id: RequestId,
        params: Option<rmcp::model::PaginatedRequestParam>,
    ) {
        tracing::trace!("tools/list -> {params:?}");
        let result = rmcp::model::ListToolsResult {
            meta: None,
            tools: vec![
                create_tool_for_savfox_tool_call_param(),
                create_tool_for_savfox_tool_call_reply_param(),
            ],
            next_cursor: None,
        };

        self.outgoing.send_response(id, result).await;
    }

    async fn handle_call_tool(&self, id: RequestId, params: CallToolRequestParam) {
        tracing::info!("tools/call -> params: {:?}", params);
        let CallToolRequestParam {
            name, arguments, ..
        } = params;

        match name.as_ref() {
            "savfox" => self.handle_tool_call_savfox(id, arguments).await,
            "savfox-reply" => {
                self.handle_tool_call_savfox_session_reply(id, arguments)
                    .await
            }
            _ => {
                let result = CallToolResult::error(vec![rmcp::model::Content::text(format!(
                    "Unknown tool '{name}'"
                ))]);
                self.outgoing.send_response(id, result).await;
            }
        }
    }

    async fn handle_tool_call_savfox(
        &self,
        id: RequestId,
        arguments: Option<rmcp::model::JsonObject>,
    ) {
        let arguments = arguments.map(serde_json::Value::Object);
        let (initial_prompt, config): (String, Config) = if let Some(json_val) = arguments { match serde_json::from_value::<SavfoxToolCallParam>(json_val) {
            Ok(tool_cfg) => match tool_cfg
                .into_config(self.savfox_linux_sandbox_exe.clone())
                .await
            {
                Ok(cfg) => cfg,
                Err(e) => {
                    let result = CallToolResult::error(vec![rmcp::model::Content::text(
                        format!("Failed to load Savfox configuration from overrides: {e}"),
                    )]);
                    self.outgoing.send_response(id, result).await;
                    return;
                }
            },
            Err(e) => {
                let result = CallToolResult::error(vec![rmcp::model::Content::text(format!(
                    "Failed to parse configuration for Savfox tool: {e}"
                ))]);
                self.outgoing.send_response(id, result).await;
                return;
            }
        } } else {
            let result = CallToolResult::error(vec![rmcp::model::Content::text(
                "Missing arguments for savfox tool-call; the `prompt` field is required.",
            )]);
            self.outgoing.send_response(id, result).await;
            return;
        };

        // Clone outgoing and server to move into async task.
        let outgoing = self.outgoing.clone();
        let session_manager = self.session_manager.clone();
        let running_requests_id_to_savfox_uuid = self.running_requests_id_to_savfox_uuid.clone();

        // Spawn an async task to handle the Savfox session so that we do not
        // block the synchronous message-processing loop.
        task::spawn(async move {
            // Run the Savfox session and stream events back to the client.
            crate::agent_tool_runner::run_savfox_tool_session(
                id,
                initial_prompt,
                config,
                outgoing,
                session_manager,
                running_requests_id_to_savfox_uuid,
            )
            .await;
        });
    }

    async fn handle_tool_call_savfox_session_reply(
        &self,
        request_id: RequestId,
        arguments: Option<rmcp::model::JsonObject>,
    ) {
        let arguments = arguments.map(serde_json::Value::Object);
        tracing::info!("tools/call -> params: {:?}", arguments);

        // parse arguments
        let savfox_tool_call_reply_param: SavfoxToolCallReplyParam = if let Some(json_val) = arguments { match serde_json::from_value::<SavfoxToolCallReplyParam>(json_val) {
            Ok(params) => params,
            Err(e) => {
                tracing::error!("Failed to parse Savfox tool call reply parameters: {e}");
                let result = CallToolResult::error(vec![rmcp::model::Content::text(format!(
                    "Failed to parse configuration for Savfox tool: {e}"
                ))]);
                self.outgoing.send_response(request_id, result).await;
                return;
            }
        } } else {
            tracing::error!(
                "Missing arguments for savfox-reply tool-call; the `session_id` and `prompt` fields are required."
            );
            let result = CallToolResult::error(vec![rmcp::model::Content::text(
                "Missing arguments for savfox-reply tool-call; the `session_id` and `prompt` fields are required.",
            )]);
            self.outgoing.send_response(request_id, result).await;
            return;
        };

        let session_id = match savfox_tool_call_reply_param.get_session_id() {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to parse session_id: {e}");
                let result = CallToolResult::error(vec![rmcp::model::Content::text(format!(
                    "Failed to parse session_id: {e}"
                ))]);
                self.outgoing.send_response(request_id, result).await;
                return;
            }
        };

        // Clone outgoing to move into async task.
        let outgoing = self.outgoing.clone();
        let running_requests_id_to_savfox_uuid = self.running_requests_id_to_savfox_uuid.clone();

        let savfox = if let Ok(c) = self.session_manager.get_session(session_id).await { c } else {
            tracing::warn!("Session not found for session_id: {session_id}");
            let result = crate::agent_tool_runner::create_call_tool_result_with_session_id(
                session_id,
                format!("Session not found for session_id: {session_id}"),
                Some(true),
            );
            outgoing.send_response(request_id, result).await;
            return;
        };

        // Spawn the long-running reply handler.
        let prompt = savfox_tool_call_reply_param.prompt.clone();
        tokio::spawn({
            let outgoing = outgoing.clone();
            let running_requests_id_to_savfox_uuid = running_requests_id_to_savfox_uuid.clone();

            async move {
                crate::agent_tool_runner::run_savfox_tool_session_reply(
                    session_id,
                    savfox,
                    outgoing,
                    request_id,
                    prompt,
                    running_requests_id_to_savfox_uuid,
                )
                .await;
            }
        });
    }

    fn handle_set_level(&self, params: rmcp::model::SetLevelRequestParam) {
        tracing::info!("logging/setLevel -> params: {:?}", params);
    }

    fn handle_complete(&self, params: rmcp::model::CompleteRequestParam) {
        tracing::info!("completion/complete -> params: {:?}", params);
    }

    // ---------------------------------------------------------------------
    // Notification handlers
    // ---------------------------------------------------------------------

    async fn handle_cancelled_notification(&self, params: rmcp::model::CancelledNotificationParam) {
        let request_id = params.request_id;
        // Create a stable string form early for logging and submission id.
        let request_id_string = request_id.to_string();

        // Obtain the session id while holding the first lock, then release.
        let session_id = {
            let map_guard = self.running_requests_id_to_savfox_uuid.lock().await;
            if let Some(id) = map_guard.get(&request_id) { *id } else {
                tracing::warn!("Session not found for request_id: {request_id_string}");
                return;
            }
        };
        tracing::info!("session_id: {session_id}");

        // Obtain the Savfox session from the server.
        let savfox_arc = if let Ok(c) = self.session_manager.get_session(session_id).await { c } else {
            tracing::warn!("Session not found for session_id: {session_id}");
            return;
        };

        // Submit interrupt to Savfox.
        if let Err(e) = savfox_arc
            .submit_with_id(Submission {
                id: request_id_string,
                op: savfox_core::protocol::Op::Interrupt,
            })
            .await
        {
            tracing::error!("Failed to submit interrupt to Savfox: {e}");
            return;
        }
        // unregister the id so we don't keep it in the map
        self.running_requests_id_to_savfox_uuid
            .lock()
            .await
            .remove(&request_id);
    }

    fn handle_progress_notification(&self, params: rmcp::model::ProgressNotificationParam) {
        tracing::info!("notifications/progress -> params: {:?}", params);
    }

    fn handle_roots_list_changed(&self) {
        tracing::info!("notifications/roots/list_changed");
    }

    fn handle_initialized_notification(&self) {
        tracing::info!("notifications/initialized");
    }
}
