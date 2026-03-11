use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use savfox_app_server_protocol::{
    ChatgptAuthTokensRefreshParams, ChatgptAuthTokensRefreshReason,
    ChatgptAuthTokensRefreshResponse, ClientInfo, ClientRequest, ConfigBatchWriteParams,
    ConfigReadParams, ConfigValueWriteParams, ConfigWarningNotification, ExperimentalApi,
    InitializeResponse, JSONRPCError, JSONRPCErrorError, JSONRPCNotification, JSONRPCRequest,
    JSONRPCResponse, RequestId, ServerNotification, ServerRequestPayload,
    experimental_required_message,
};
use savfox_core::auth::{
    ExternalAuthRefreshContext, ExternalAuthRefreshReason, ExternalAuthRefresher,
    ExternalAuthTokens,
};
use savfox_core::config::Config;
use savfox_core::config_loader::{CloudRequirementsLoader, LoaderOverrides};
use savfox_core::default_client::{
    SetOriginatorError, USER_AGENT_SUFFIX, get_savfox_user_agent,
    set_default_client_residency_requirement, set_default_originator,
};
use savfox_core::{AuthManager, SessionManager};
use savfox_feedback::SavfoxFeedback;
use savfox_protocol::SessionId;
use savfox_protocol::protocol::SessionSource;
use tokio::sync::broadcast;
use tokio::time::{Duration, timeout};
use toml::Value as TomlValue;

use crate::config_api::ConfigApi;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::outgoing_message::OutgoingMessageSender;
use crate::savfox_message_processor::{SavfoxMessageProcessor, SavfoxMessageProcessorArgs};

const EXTERNAL_AUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ExternalAuthRefreshBridge {
    outgoing: Arc<OutgoingMessageSender>,
}

impl ExternalAuthRefreshBridge {
    fn map_reason(reason: ExternalAuthRefreshReason) -> ChatgptAuthTokensRefreshReason {
        match reason {
            ExternalAuthRefreshReason::Unauthorized => ChatgptAuthTokensRefreshReason::Unauthorized,
        }
    }
}

#[async_trait]
impl ExternalAuthRefresher for ExternalAuthRefreshBridge {
    async fn refresh(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> std::io::Result<ExternalAuthTokens> {
        let params = ChatgptAuthTokensRefreshParams {
            reason: Self::map_reason(context.reason),
            previous_account_id: context.previous_account_id,
        };

        let (request_id, rx) = self
            .outgoing
            .send_request_with_id(ServerRequestPayload::ChatgptAuthTokensRefresh(params))
            .await;

        let result = match timeout(EXTERNAL_AUTH_REFRESH_TIMEOUT, rx).await {
            Ok(result) => result.map_err(|err| {
                std::io::Error::other(format!("auth refresh request canceled: {err}"))
            })?,
            Err(_) => {
                let _canceled = self.outgoing.cancel_request(&request_id).await;
                return Err(std::io::Error::other(format!(
                    "auth refresh request timed out after {}s",
                    EXTERNAL_AUTH_REFRESH_TIMEOUT.as_secs()
                )));
            }
        };

        let response: ChatgptAuthTokensRefreshResponse =
            serde_json::from_value(result).map_err(std::io::Error::other)?;

        Ok(ExternalAuthTokens {
            access_token: response.access_token,
            id_token: response.id_token,
        })
    }
}

pub(crate) struct MessageProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    savfox_message_processor: SavfoxMessageProcessor,
    config_api: ConfigApi,
    config: Arc<Config>,
    initialized: bool,
    experimental_api_enabled: Arc<AtomicBool>,
    config_warnings: Vec<ConfigWarningNotification>,
}

pub(crate) struct MessageProcessorArgs {
    pub(crate) outgoing: OutgoingMessageSender,
    pub(crate) savfox_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) config: Arc<Config>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) loader_overrides: LoaderOverrides,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: SavfoxFeedback,
    pub(crate) config_warnings: Vec<ConfigWarningNotification>,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(args: MessageProcessorArgs) -> Self {
        let MessageProcessorArgs {
            outgoing,
            savfox_linux_sandbox_exe,
            config,
            cli_overrides,
            loader_overrides,
            cloud_requirements,
            feedback,
            config_warnings,
        } = args;
        let outgoing = Arc::new(outgoing);
        let experimental_api_enabled = Arc::new(AtomicBool::new(false));
        let auth_manager = AuthManager::shared(
            config.savfox_home.clone(),
            false,
            config.cli_auth_credentials_store_mode,
        );
        auth_manager.set_forced_chatgpt_workspace_id(config.forced_chatgpt_workspace_id.clone());
        auth_manager.set_external_auth_refresher(Arc::new(ExternalAuthRefreshBridge {
            outgoing: outgoing.clone(),
        }));
        let session_manager = Arc::new(SessionManager::new(
            config.savfox_home.clone(),
            auth_manager.clone(),
            SessionSource::VSCode,
        ));
        let savfox_message_processor = SavfoxMessageProcessor::new(SavfoxMessageProcessorArgs {
            auth_manager,
            session_manager,
            outgoing: outgoing.clone(),
            savfox_linux_sandbox_exe,
            config: Arc::clone(&config),
            cli_overrides: cli_overrides.clone(),
            cloud_requirements: cloud_requirements.clone(),
            feedback,
        });
        let config_api = ConfigApi::new(
            config.savfox_home.clone(),
            cli_overrides,
            loader_overrides,
            cloud_requirements,
        );

        Self {
            outgoing,
            savfox_message_processor,
            config_api,
            config,
            initialized: false,
            experimental_api_enabled,
            config_warnings,
        }
    }

    pub(crate) async fn process_request(&mut self, request: JSONRPCRequest) {
        let request_id = request.id.clone();
        let request_json = match serde_json::to_value(&request) {
            Ok(request_json) => request_json,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let savfox_request = match serde_json::from_value::<ClientRequest>(request_json) {
            Ok(savfox_request) => savfox_request,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        match savfox_request {
            // Handle Initialize internally so SavfoxMessageProcessor does not have to concern
            // itself with the `initialized` bool.
            ClientRequest::Initialize { request_id, params } => {
                if self.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Already initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                } else {
                    let experimental_api_enabled = params
                        .capabilities
                        .as_ref()
                        .is_some_and(|cap| cap.experimental_api);
                    self.experimental_api_enabled
                        .store(experimental_api_enabled, Ordering::Relaxed);
                    let ClientInfo {
                        name,
                        title: _title,
                        version,
                    } = params.client_info;
                    if let Err(error) = set_default_originator(name.clone()) {
                        match error {
                            SetOriginatorError::InvalidHeaderValue => {
                                let error = JSONRPCErrorError {
                                    code: INVALID_REQUEST_ERROR_CODE,
                                    message: format!(
                                        "Invalid clientInfo.name: '{name}'. Must be a valid HTTP header value."
                                    ),
                                    data: None,
                                };
                                self.outgoing.send_error(request_id, error).await;
                                return;
                            }
                            SetOriginatorError::AlreadyInitialized => {
                                // No-op. This is expected to happen if the originator is already
                                // set via env var. TODO(owen): Once
                                // we remove support for SAVFOX_INTERNAL_ORIGINATOR_OVERRIDE,
                                // this will be an unexpected state and we can return a JSON-RPC
                                // error indicating internal server
                                // error.
                            }
                        }
                    }
                    set_default_client_residency_requirement(self.config.enforce_residency.value());
                    let user_agent_suffix = format!("{name}; {version}");
                    if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
                        *suffix = Some(user_agent_suffix);
                    }

                    let user_agent = get_savfox_user_agent();
                    let response = InitializeResponse { user_agent };
                    self.outgoing.send_response(request_id, response).await;

                    self.initialized = true;
                    if !self.config_warnings.is_empty() {
                        for notification in self.config_warnings.drain(..) {
                            self.outgoing
                                .send_server_notification(ServerNotification::ConfigWarning(
                                    notification,
                                ))
                                .await;
                        }
                    }

                    return;
                }
            }
            _ => {
                if !self.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Not initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            }
        }

        if let Some(reason) = savfox_request.experimental_reason()
            && !self.experimental_api_enabled.load(Ordering::Relaxed)
        {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: experimental_required_message(reason),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        match savfox_request {
            ClientRequest::ConfigRead { request_id, params } => {
                self.handle_config_read(request_id, params).await;
            }
            ClientRequest::ConfigValueWrite { request_id, params } => {
                self.handle_config_value_write(request_id, params).await;
            }
            ClientRequest::ConfigBatchWrite { request_id, params } => {
                self.handle_config_batch_write(request_id, params).await;
            }
            ClientRequest::ConfigRequirementsRead {
                request_id,
                params: _,
            } => {
                self.handle_config_requirements_read(request_id).await;
            }
            other => {
                self.savfox_message_processor.process_request(other).await;
            }
        }
    }

    pub(crate) async fn process_notification(&self, notification: JSONRPCNotification) {
        // Currently, we do not expect to receive any notifications from the
        // client, so we just log them.
        tracing::info!("<- notification: {:?}", notification);
    }

    pub(crate) fn session_created_receiver(&self) -> broadcast::Receiver<SessionId> {
        self.savfox_message_processor.session_created_receiver()
    }

    pub(crate) async fn try_attach_session_listener(&mut self, session_id: SessionId) {
        if !self.initialized {
            return;
        }
        self.savfox_message_processor
            .try_attach_session_listener(session_id)
            .await;
    }

    /// Handle a standalone JSON-RPC response originating from the peer.
    pub(crate) async fn process_response(&mut self, response: JSONRPCResponse) {
        tracing::info!("<- response: {:?}", response);
        let JSONRPCResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    /// Handle an error object received from the peer.
    pub(crate) async fn process_error(&mut self, err: JSONRPCError) {
        tracing::error!("<- error: {:?}", err);
        self.outgoing.notify_client_error(err.id, err.error).await;
    }

    async fn handle_config_read(&self, request_id: RequestId, params: ConfigReadParams) {
        match self.config_api.read(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_value_write(
        &self,
        request_id: RequestId,
        params: ConfigValueWriteParams,
    ) {
        match self.config_api.write_value(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_batch_write(
        &self,
        request_id: RequestId,
        params: ConfigBatchWriteParams,
    ) {
        match self.config_api.batch_write(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_requirements_read(&self, request_id: RequestId) {
        match self.config_api.config_requirements_read().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }
}
