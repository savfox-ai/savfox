use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Context;
use savfox_app_server_protocol::{
    AddConversationListenerParams, AppsListParams, ArchiveConversationParams,
    CancelLoginAccountParams, CancelLoginChatGptParams, ClientInfo, ClientNotification,
    CollaborationModeListParams, ConfigBatchWriteParams, ConfigReadParams, ConfigValueWriteParams,
    FeedbackUploadParams, ForkConversationParams, GetAccountParams, GetAuthStatusParams,
    InitializeCapabilities, InitializeParams, InterruptConversationParams, JSONRPCError,
    JSONRPCErrorError, JSONRPCNotification, JSONRPCRequest, JSONRPCResponse, JsonRpcMessage,
    ListConversationsParams, LoginAccountParams, LoginApiKeyParams, MockExperimentalMethodParams,
    ModelListParams, NewConversationParams, RemoveConversationListenerParams, RequestId,
    ResumeConversationParams, ReviewStartParams, SendUserMessageParams, SendUserTurnParams,
    ServerRequest, SessionArchiveParams, SessionForkParams, SessionListParams,
    SessionLoadedListParams, SessionReadParams, SessionResumeParams, SessionRollbackParams,
    SessionStartParams, SessionUnarchiveParams, SetDefaultModelParams, TurnInterruptParams,
    TurnStartParams,
};
use savfox_core::default_client::SAVFOX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct McpProcess {
    next_request_id: AtomicI64,
    /// Retain this child process until the client is dropped. The Tokio runtime
    /// will make a "best effort" to reap the process after it exits, but it is
    /// not a guarantee. See the `kill_on_drop` documentation for details.
    #[allow(dead_code)]
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pending_messages: VecDeque<JsonRpcMessage>,
}

pub const DEFAULT_CLIENT_NAME: &str = "savfox-app-server-tests";

impl McpProcess {
    pub async fn new(savfox_home: &Path) -> anyhow::Result<Self> {
        Self::new_with_env(savfox_home, &[]).await
    }

    /// Creates a new MCP process, allowing tests to override or remove
    /// specific environment variables for the child process only.
    ///
    /// Pass a tuple of (key, Some(value)) to set/override, or (key, None) to
    /// remove a variable from the child's environment.
    pub async fn new_with_env(
        savfox_home: &Path,
        env_overrides: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Self> {
        let program = savfox_utils_cargo_bin::cargo_bin("savfox-app-server")
            .context("should find binary for savfox-app-server")?;
        let mut cmd = Command::new(program);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env("SAVFOX_HOME", savfox_home);
        cmd.env("RUST_LOG", "debug");
        cmd.env_remove(SAVFOX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR);

        for (k, v) in env_overrides {
            match v {
                Some(val) => {
                    cmd.env(k, val);
                }
                None => {
                    cmd.env_remove(k);
                }
            }
        }

        let mut process = cmd
            .kill_on_drop(true)
            .spawn()
            .context("savfox-mcp-server proc should start")?;
        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow::format_err!("mcp should have stdin fd"))?;
        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| anyhow::format_err!("mcp should have stdout fd"))?;
        let stdout = BufReader::new(stdout);

        // Forward child's stderr to our stderr so failures are visible even
        // when stdout/stderr are captured by the test harness.
        if let Some(stderr) = process.stderr.take() {
            let mut stderr_reader = BufReader::new(stderr).lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = stderr_reader.next_line().await {
                    eprintln!("[mcp stderr] {line}");
                }
            });
        }
        Ok(Self {
            next_request_id: AtomicI64::new(0),
            process,
            stdin,
            stdout,
            pending_messages: VecDeque::new(),
        })
    }

    /// Performs the initialization handshake with the MCP server.
    pub async fn initialize(&mut self) -> anyhow::Result<()> {
        let initialized = self
            .initialize_with_client_info(ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            })
            .await?;
        let JsonRpcMessage::Response(_) = initialized else {
            unreachable!("expected JsonRpcMessage::Response for initialize, got {initialized:?}");
        };
        Ok(())
    }

    /// Sends initialize with the provided client info and returns the response/error message.
    pub async fn initialize_with_client_info(
        &mut self,
        client_info: ClientInfo,
    ) -> anyhow::Result<JsonRpcMessage> {
        self.initialize_with_capabilities(
            client_info,
            Some(InitializeCapabilities {
                experimental_api: true,
            }),
        )
        .await
    }

    pub async fn initialize_with_capabilities(
        &mut self,
        client_info: ClientInfo,
        capabilities: Option<InitializeCapabilities>,
    ) -> anyhow::Result<JsonRpcMessage> {
        self.initialize_with_params(InitializeParams {
            client_info,
            capabilities,
        })
        .await
    }

    async fn initialize_with_params(
        &mut self,
        params: InitializeParams,
    ) -> anyhow::Result<JsonRpcMessage> {
        let params = Some(serde_json::to_value(params)?);
        let request_id = self.send_request("initialize", params).await?;
        let message = self.read_jsonrpc_message().await?;
        match message {
            JsonRpcMessage::Response(response) => {
                if response.id != RequestId::Integer(request_id) {
                    anyhow::bail!(
                        "initialize response id mismatch: expected {}, got {:?}",
                        request_id,
                        response.id
                    );
                }

                // Send notifications/initialized to ack the response.
                self.send_notification(ClientNotification::Initialized)
                    .await?;

                Ok(JsonRpcMessage::Response(response))
            }
            JsonRpcMessage::Error(error) => {
                if error.id != RequestId::Integer(request_id) {
                    anyhow::bail!(
                        "initialize error id mismatch: expected {}, got {:?}",
                        request_id,
                        error.id
                    );
                }
                Ok(JsonRpcMessage::Error(error))
            }
            JsonRpcMessage::Notification(notification) => {
                anyhow::bail!("unexpected JsonRpcMessage::Notification: {notification:?}");
            }
            JsonRpcMessage::Request(request) => {
                anyhow::bail!("unexpected JsonRpcMessage::Request: {request:?}");
            }
        }
    }

    /// Send a `newConversation` JSON-RPC request.
    pub async fn send_new_conversation_request(
        &mut self,
        params: NewConversationParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("newConversation", params).await
    }

    /// Send an `archiveConversation` JSON-RPC request.
    pub async fn send_archive_conversation_request(
        &mut self,
        params: ArchiveConversationParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("archiveConversation", params).await
    }

    /// Send an `addConversationListener` JSON-RPC request.
    pub async fn send_add_conversation_listener_request(
        &mut self,
        params: AddConversationListenerParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("addConversationListener", params).await
    }

    /// Send a `sendUserMessage` JSON-RPC request with a single text item.
    pub async fn send_send_user_message_request(
        &mut self,
        params: SendUserMessageParams,
    ) -> anyhow::Result<i64> {
        // Wire format expects variants in camelCase; text item uses external tagging.
        let params = Some(serde_json::to_value(params)?);
        self.send_request("sendUserMessage", params).await
    }

    /// Send a `removeConversationListener` JSON-RPC request.
    pub async fn send_remove_session_listener_request(
        &mut self,
        params: RemoveConversationListenerParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("removeConversationListener", params)
            .await
    }

    /// Send a `sendUserTurn` JSON-RPC request.
    pub async fn send_send_user_turn_request(
        &mut self,
        params: SendUserTurnParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("sendUserTurn", params).await
    }

    /// Send a `interruptConversation` JSON-RPC request.
    pub async fn send_interrupt_conversation_request(
        &mut self,
        params: InterruptConversationParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("interruptConversation", params).await
    }

    /// Send a `getAuthStatus` JSON-RPC request.
    pub async fn send_get_auth_status_request(
        &mut self,
        params: GetAuthStatusParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("getAuthStatus", params).await
    }

    /// Send a `getUserSavedConfig` JSON-RPC request.
    pub async fn send_get_user_saved_config_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("getUserSavedConfig", None).await
    }

    /// Send a `getUserAgent` JSON-RPC request.
    pub async fn send_get_user_agent_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("getUserAgent", None).await
    }

    /// Send an `account/rateLimits/read` JSON-RPC request.
    pub async fn send_get_account_rate_limits_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("account/rateLimits/read", None).await
    }

    /// Send an `account/read` JSON-RPC request.
    pub async fn send_get_account_request(
        &mut self,
        params: GetAccountParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("account/read", params).await
    }

    /// Send an `account/login/start` JSON-RPC request with ChatGPT auth tokens.
    pub async fn send_chatgpt_auth_tokens_login_request(
        &mut self,
        id_token: String,
        access_token: String,
    ) -> anyhow::Result<i64> {
        let params = LoginAccountParams::ChatgptAuthTokens {
            id_token,
            access_token,
        };
        let params = Some(serde_json::to_value(params)?);
        self.send_request("account/login/start", params).await
    }

    /// Send a `feedback/upload` JSON-RPC request.
    pub async fn send_feedback_upload_request(
        &mut self,
        params: FeedbackUploadParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("feedback/upload", params).await
    }

    /// Send a `userInfo` JSON-RPC request.
    pub async fn send_user_info_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("userInfo", None).await
    }

    /// Send a `setDefaultModel` JSON-RPC request.
    pub async fn send_set_default_model_request(
        &mut self,
        params: SetDefaultModelParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("setDefaultModel", params).await
    }

    /// Send a `listConversations` JSON-RPC request.
    pub async fn send_list_conversations_request(
        &mut self,
        params: ListConversationsParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("listConversations", params).await
    }

    /// Send a `session/start` JSON-RPC request.
    pub async fn send_session_start_request(
        &mut self,
        params: SessionStartParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/start", params).await
    }

    /// Send a `session/resume` JSON-RPC request.
    pub async fn send_session_resume_request(
        &mut self,
        params: SessionResumeParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/resume", params).await
    }

    /// Send a `session/fork` JSON-RPC request.
    pub async fn send_session_fork_request(
        &mut self,
        params: SessionForkParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/fork", params).await
    }

    /// Send a `session/archive` JSON-RPC request.
    pub async fn send_session_archive_request(
        &mut self,
        params: SessionArchiveParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/archive", params).await
    }

    /// Send a `session/unarchive` JSON-RPC request.
    pub async fn send_session_unarchive_request(
        &mut self,
        params: SessionUnarchiveParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/unarchive", params).await
    }

    /// Send a `session/rollback` JSON-RPC request.
    pub async fn send_session_rollback_request(
        &mut self,
        params: SessionRollbackParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/rollback", params).await
    }

    /// Send a `session/list` JSON-RPC request.
    pub async fn send_session_list_request(
        &mut self,
        params: SessionListParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/list", params).await
    }

    /// Send a `session/loaded/list` JSON-RPC request.
    pub async fn send_session_loaded_list_request(
        &mut self,
        params: SessionLoadedListParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/loaded/list", params).await
    }

    /// Send a `session/read` JSON-RPC request.
    pub async fn send_session_read_request(
        &mut self,
        params: SessionReadParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("session/read", params).await
    }

    /// Send a `model/list` JSON-RPC request.
    pub async fn send_list_models_request(
        &mut self,
        params: ModelListParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("model/list", params).await
    }

    /// Send an `app/list` JSON-RPC request.
    pub async fn send_apps_list_request(&mut self, params: AppsListParams) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("app/list", params).await
    }

    /// Send a `collaborationMode/list` JSON-RPC request.
    pub async fn send_list_collaboration_modes_request(
        &mut self,
        params: CollaborationModeListParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("collaborationMode/list", params).await
    }

    /// Send a `mock/experimentalMethod` JSON-RPC request.
    pub async fn send_mock_experimental_method_request(
        &mut self,
        params: MockExperimentalMethodParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("mock/experimentalMethod", params).await
    }

    /// Send a `resumeConversation` JSON-RPC request.
    pub async fn send_resume_conversation_request(
        &mut self,
        params: ResumeConversationParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("resumeConversation", params).await
    }

    /// Send a `forkConversation` JSON-RPC request.
    pub async fn send_fork_conversation_request(
        &mut self,
        params: ForkConversationParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("forkConversation", params).await
    }

    /// Send a `loginApiKey` JSON-RPC request.
    pub async fn send_login_api_key_request(
        &mut self,
        params: LoginApiKeyParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("loginApiKey", params).await
    }

    /// Send a `loginChatGpt` JSON-RPC request.
    pub async fn send_login_chat_gpt_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("loginChatGpt", None).await
    }

    /// Send a `turn/start` JSON-RPC request (v2).
    pub async fn send_turn_start_request(
        &mut self,
        params: TurnStartParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("turn/start", params).await
    }

    /// Send a `turn/interrupt` JSON-RPC request (v2).
    pub async fn send_turn_interrupt_request(
        &mut self,
        params: TurnInterruptParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("turn/interrupt", params).await
    }

    /// Send a `review/start` JSON-RPC request (v2).
    pub async fn send_review_start_request(
        &mut self,
        params: ReviewStartParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("review/start", params).await
    }

    /// Send a `cancelLoginChatGpt` JSON-RPC request.
    pub async fn send_cancel_login_chat_gpt_request(
        &mut self,
        params: CancelLoginChatGptParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("cancelLoginChatGpt", params).await
    }

    /// Send a `logoutChatGpt` JSON-RPC request.
    pub async fn send_logout_chat_gpt_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("logoutChatGpt", None).await
    }

    pub async fn send_config_read_request(
        &mut self,
        params: ConfigReadParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("config/read", params).await
    }

    pub async fn send_config_value_write_request(
        &mut self,
        params: ConfigValueWriteParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("config/value/write", params).await
    }

    pub async fn send_config_batch_write_request(
        &mut self,
        params: ConfigBatchWriteParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("config/batchWrite", params).await
    }

    /// Send an `account/logout` JSON-RPC request.
    pub async fn send_logout_account_request(&mut self) -> anyhow::Result<i64> {
        self.send_request("account/logout", None).await
    }

    /// Send an `account/login/start` JSON-RPC request for API key login.
    pub async fn send_login_account_api_key_request(
        &mut self,
        api_key: &str,
    ) -> anyhow::Result<i64> {
        let params = serde_json::json!({
            "type": "apiKey",
            "apiKey": api_key,
        });
        self.send_request("account/login/start", Some(params)).await
    }

    /// Send an `account/login/start` JSON-RPC request for ChatGPT login.
    pub async fn send_login_account_chatgpt_request(&mut self) -> anyhow::Result<i64> {
        let params = serde_json::json!({
            "type": "chatgpt"
        });
        self.send_request("account/login/start", Some(params)).await
    }

    /// Send an `account/login/cancel` JSON-RPC request.
    pub async fn send_cancel_login_account_request(
        &mut self,
        params: CancelLoginAccountParams,
    ) -> anyhow::Result<i64> {
        let params = Some(serde_json::to_value(params)?);
        self.send_request("account/login/cancel", params).await
    }

    /// Send a `fuzzyFileSearch` JSON-RPC request.
    pub async fn send_fuzzy_file_search_request(
        &mut self,
        query: &str,
        roots: Vec<String>,
        cancellation_token: Option<String>,
    ) -> anyhow::Result<i64> {
        let mut params = serde_json::json!({
            "query": query,
            "roots": roots,
        });
        if let Some(token) = cancellation_token {
            params["cancellationToken"] = serde_json::json!(token);
        }
        self.send_request("fuzzyFileSearch", Some(params)).await
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<i64> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        let message = JsonRpcMessage::Request(JSONRPCRequest {
            id: RequestId::Integer(request_id),
            method: method.to_string(),
            params,
        });
        self.send_jsonrpc_message(message).await?;
        Ok(request_id)
    }

    pub async fn send_response(
        &mut self,
        id: RequestId,
        result: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.send_jsonrpc_message(JsonRpcMessage::Response(JSONRPCResponse { id, result }))
            .await
    }

    pub async fn send_error(
        &mut self,
        id: RequestId,
        error: JSONRPCErrorError,
    ) -> anyhow::Result<()> {
        self.send_jsonrpc_message(JsonRpcMessage::Error(JSONRPCError { id, error }))
            .await
    }

    pub async fn send_notification(
        &mut self,
        notification: ClientNotification,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(notification)?;
        self.send_jsonrpc_message(JsonRpcMessage::Notification(JSONRPCNotification {
            method: value
                .get("method")
                .and_then(|m| m.as_str())
                .ok_or_else(|| anyhow::format_err!("notification missing method field"))?
                .to_string(),
            params: value.get("params").cloned(),
        }))
        .await
    }

    async fn send_jsonrpc_message(&mut self, message: JsonRpcMessage) -> anyhow::Result<()> {
        eprintln!("writing message to stdin: {message:?}");
        let payload = serde_json::to_string(&message)?;
        self.stdin.write_all(payload.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_jsonrpc_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        let mut line = String::new();
        self.stdout.read_line(&mut line).await?;
        let message = serde_json::from_str::<JsonRpcMessage>(&line)?;
        eprintln!("read message from stdout: {message:?}");
        Ok(message)
    }

    pub async fn read_stream_until_request_message(&mut self) -> anyhow::Result<ServerRequest> {
        eprintln!("in read_stream_until_request_message()");

        let message = self
            .read_stream_until_message(|message| matches!(message, JsonRpcMessage::Request(_)))
            .await?;

        let JsonRpcMessage::Request(jsonrpc_request) = message else {
            unreachable!("expected JsonRpcMessage::Request, got {message:?}");
        };
        jsonrpc_request
            .try_into()
            .with_context(|| "failed to deserialize ServerRequest from JSONRPCRequest")
    }

    pub async fn read_stream_until_response_message(
        &mut self,
        request_id: RequestId,
    ) -> anyhow::Result<JSONRPCResponse> {
        eprintln!("in read_stream_until_response_message({request_id:?})");

        let message = self
            .read_stream_until_message(|message| {
                Self::message_request_id(message) == Some(&request_id)
            })
            .await?;

        let JsonRpcMessage::Response(response) = message else {
            unreachable!("expected JsonRpcMessage::Response, got {message:?}");
        };
        Ok(response)
    }

    pub async fn read_stream_until_error_message(
        &mut self,
        request_id: RequestId,
    ) -> anyhow::Result<JSONRPCError> {
        let message = self
            .read_stream_until_message(|message| {
                Self::message_request_id(message) == Some(&request_id)
            })
            .await?;

        let JsonRpcMessage::Error(err) = message else {
            unreachable!("expected JsonRpcMessage::Error, got {message:?}");
        };
        Ok(err)
    }

    pub async fn read_stream_until_notification_message(
        &mut self,
        method: &str,
    ) -> anyhow::Result<JSONRPCNotification> {
        eprintln!("in read_stream_until_notification_message({method})");

        let message = self
            .read_stream_until_message(|message| {
                matches!(
                    message,
                    JsonRpcMessage::Notification(notification) if notification.method == method
                )
            })
            .await?;

        let JsonRpcMessage::Notification(notification) = message else {
            unreachable!("expected JsonRpcMessage::Notification, got {message:?}");
        };
        Ok(notification)
    }

    pub async fn read_next_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        self.read_stream_until_message(|_| true).await
    }

    /// Clears any buffered messages so future reads only consider new stream items.
    ///
    /// We call this when e.g. we want to validate against the next turn and no longer care about
    /// messages buffered from the prior turn.
    pub fn clear_message_buffer(&mut self) {
        self.pending_messages.clear();
    }

    /// Reads the stream until a message matches `predicate`, buffering any non-matching messages
    /// for later reads.
    async fn read_stream_until_message<F>(&mut self, predicate: F) -> anyhow::Result<JsonRpcMessage>
    where
        F: Fn(&JsonRpcMessage) -> bool,
    {
        if let Some(message) = self.take_pending_message(&predicate) {
            return Ok(message);
        }

        loop {
            let message = self.read_jsonrpc_message().await?;
            if predicate(&message) {
                return Ok(message);
            }
            self.pending_messages.push_back(message);
        }
    }

    fn take_pending_message<F>(&mut self, predicate: &F) -> Option<JsonRpcMessage>
    where
        F: Fn(&JsonRpcMessage) -> bool,
    {
        if let Some(pos) = self.pending_messages.iter().position(predicate) {
            return self.pending_messages.remove(pos);
        }
        None
    }

    fn message_request_id(message: &JsonRpcMessage) -> Option<&RequestId> {
        match message {
            JsonRpcMessage::Request(request) => Some(&request.id),
            JsonRpcMessage::Response(response) => Some(&response.id),
            JsonRpcMessage::Error(err) => Some(&err.id),
            JsonRpcMessage::Notification(_) => None,
        }
    }
}
