use std::time::Duration;

use savfox_app_server_protocol::{
    Account, AccountLoginCompletedNotification, AccountUpdatedNotification, AuthMode,
    CancelLoginAccountParams, CancelLoginAccountResponse, CancelLoginAccountStatus,
    GetAccountParams, GetAccountRateLimitsResponse, GetAccountResponse, JSONRPCErrorError,
    LoginAccountParams, LoginAccountResponse, LogoutAccountResponse, RequestId, ServerNotification,
};
use savfox_core::SavfoxAuth;
use savfox_core::auth::{CLIENT_ID, login_with_api_key, login_with_chatgpt_auth_tokens};
use savfox_core::token_data::parse_id_token;
use savfox_login_oauth::{
    ServerOptions as LoginServerOptions, complete_device_code_login, request_device_code,
    run_login_server,
};
use savfox_protocol::config_types::ForcedLoginMethod;
use savfox_protocol::protocol::RateLimitSnapshot as CoreRateLimitSnapshot;
use uuid::Uuid;

use super::{
    ActiveLogin, CancelLoginError, INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE,
    LOGIN_CHATGPT_TIMEOUT, SavfoxMessageProcessor,
};

impl SavfoxMessageProcessor {
    pub(crate) async fn login_v2(&mut self, request_id: RequestId, params: LoginAccountParams) {
        match params {
            LoginAccountParams::ApiKey { api_key } => {
                self.login_api_key_v2(request_id, api_key).await;
            }
            LoginAccountParams::Chatgpt => {
                self.login_chatgpt_v2(request_id).await;
            }
            LoginAccountParams::DeviceCode => {
                self.login_device_code_v2(request_id).await;
            }
            LoginAccountParams::ChatgptAuthTokens {
                id_token,
                access_token,
            } => {
                self.login_chatgpt_auth_tokens(request_id, id_token, access_token)
                    .await;
            }
        }
    }

    fn external_auth_active_error(&self) -> JSONRPCErrorError {
        JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message: "External auth is active. Use account/login/start (chatgptAuthTokens) to update it or account/logout to clear it."
                .to_string(),
            data: None,
        }
    }

    async fn login_api_key_common(
        &mut self,
        api_key: &str,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_external_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if matches!(
            self.config.forced_login_method,
            Some(ForcedLoginMethod::Chatgpt)
        ) {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "API key login is disabled. Use ChatGPT login instead.".to_string(),
                data: None,
            });
        }

        // Cancel any active login attempt.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                drop(active);
            }
        }

        match login_with_api_key(
            &self.config.savfox_home,
            api_key,
            self.config.cli_auth_credentials_store_mode,
        ) {
            Ok(()) => {
                self.auth_manager.reload();
                Ok(())
            }
            Err(err) => Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to save api key: {err}"),
                data: None,
            }),
        }
    }

    async fn login_api_key_v2(&mut self, request_id: RequestId, api_key: String) {
        match self.login_api_key_common(&api_key).await {
            Ok(()) => {
                let response = savfox_app_server_protocol::LoginAccountResponse::ApiKey {};
                self.outgoing.send_response(request_id, response).await;

                let payload_login_completed = AccountLoginCompletedNotification {
                    login_id: None,
                    success: true,
                    error: None,
                };
                self.outgoing
                    .send_server_notification(ServerNotification::AccountLoginCompleted(
                        payload_login_completed,
                    ))
                    .await;

                let payload_v2 = AccountUpdatedNotification {
                    auth_mode: self
                        .auth_manager
                        .auth_cached()
                        .as_ref()
                        .map(SavfoxAuth::api_auth_mode),
                };
                self.outgoing
                    .send_server_notification(ServerNotification::AccountUpdated(payload_v2))
                    .await;
            }
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    // Build options for a ChatGPT login attempt; performs validation.
    async fn login_chatgpt_common(
        &self,
    ) -> std::result::Result<LoginServerOptions, JSONRPCErrorError> {
        let config = self.config.as_ref();

        if self.auth_manager.is_external_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
            return Err(JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "ChatGPT login is disabled. Use API key login instead.".to_string(),
                data: None,
            });
        }

        Ok(LoginServerOptions {
            open_browser: false,
            ..LoginServerOptions::new(
                config.savfox_home.clone(),
                CLIENT_ID.to_string(),
                config.forced_chatgpt_workspace_id.clone(),
                config.cli_auth_credentials_store_mode,
            )
        })
    }

    async fn login_chatgpt_v2(&mut self, request_id: RequestId) {
        match self.login_chatgpt_common().await {
            Ok(opts) => match run_login_server(opts) {
                Ok(server) => {
                    let login_id = Uuid::new_v4();
                    let shutdown_handle = server.cancel_handle();

                    // Replace active login if present.
                    {
                        let mut guard = self.active_login.lock().await;
                        if let Some(existing) = guard.take() {
                            drop(existing);
                        }
                        *guard = Some(ActiveLogin {
                            shutdown_handle: shutdown_handle.clone(),
                            login_id,
                        });
                    }

                    // Spawn background task to monitor completion.
                    let outgoing_clone = self.outgoing.clone();
                    let active_login = self.active_login.clone();
                    let auth_manager = self.auth_manager.clone();
                    let auth_url = server.auth_url.clone();
                    tokio::spawn(async move {
                        let (success, error_msg) = match tokio::time::timeout(
                            LOGIN_CHATGPT_TIMEOUT,
                            server.block_until_done(),
                        )
                        .await
                        {
                            Ok(Ok(())) => (true, None),
                            Ok(Err(err)) => (false, Some(format!("Login server error: {err}"))),
                            Err(_elapsed) => {
                                shutdown_handle.shutdown();
                                (false, Some("Login timed out".to_string()))
                            }
                        };

                        let payload_v2 = AccountLoginCompletedNotification {
                            login_id: Some(login_id.to_string()),
                            success,
                            error: error_msg,
                        };
                        outgoing_clone
                            .send_server_notification(ServerNotification::AccountLoginCompleted(
                                payload_v2,
                            ))
                            .await;

                        if success {
                            auth_manager.reload();

                            // Notify clients with the actual current auth mode.
                            let current_auth_method = auth_manager
                                .auth_cached()
                                .as_ref()
                                .map(SavfoxAuth::api_auth_mode);
                            let payload_v2 = AccountUpdatedNotification {
                                auth_mode: current_auth_method,
                            };
                            outgoing_clone
                                .send_server_notification(ServerNotification::AccountUpdated(
                                    payload_v2,
                                ))
                                .await;
                        }

                        // Clear the active login if it matches this attempt. It may have been
                        // replaced or cancelled.
                        let mut guard = active_login.lock().await;
                        if guard.as_ref().map(|l| l.login_id) == Some(login_id) {
                            *guard = None;
                        }
                    });

                    let response = savfox_app_server_protocol::LoginAccountResponse::Chatgpt {
                        login_id: login_id.to_string(),
                        auth_url,
                    };
                    self.outgoing.send_response(request_id, response).await;
                }
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INTERNAL_ERROR_CODE,
                        message: format!("failed to start login server: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                }
            },
            Err(err) => {
                self.outgoing.send_error(request_id, err).await;
            }
        }
    }

    async fn login_device_code_v2(&mut self, request_id: RequestId) {
        match self.login_chatgpt_common().await {
            Ok(opts) => match request_device_code(&opts).await {
                Ok(device_code) => {
                    let login_id = Uuid::new_v4();
                    let response = LoginAccountResponse::DeviceCode {
                        login_id: login_id.to_string(),
                        verification_url: device_code.verification_url.clone(),
                        user_code: device_code.user_code.clone(),
                    };
                    self.outgoing.send_response(request_id, response).await;

                    let auth_manager = self.auth_manager.clone();
                    let outgoing_clone = self.outgoing.clone();
                    tokio::spawn(async move {
                        let result = tokio::time::timeout(
                            Duration::from_secs(900),
                            complete_device_code_login(opts, device_code),
                        )
                        .await;

                        let (success, error_msg) = match result {
                            Ok(Ok(())) => {
                                auth_manager.reload();
                                (true, None)
                            }
                            Ok(Err(err)) => {
                                (false, Some(format!("Device code login error: {err}")))
                            }
                            Err(_) => (false, Some("Device code login timed out".to_string())),
                        };

                        let payload_login_completed = AccountLoginCompletedNotification {
                            login_id: Some(login_id.to_string()),
                            success,
                            error: error_msg,
                        };
                        outgoing_clone
                            .send_server_notification(ServerNotification::AccountLoginCompleted(
                                payload_login_completed,
                            ))
                            .await;

                        if success {
                            let current_auth_method = auth_manager
                                .auth_cached()
                                .as_ref()
                                .map(SavfoxAuth::api_auth_mode);
                            let payload_updated = AccountUpdatedNotification {
                                auth_mode: current_auth_method,
                            };
                            outgoing_clone
                                .send_server_notification(ServerNotification::AccountUpdated(
                                    payload_updated,
                                ))
                                .await;
                        }
                    });
                }
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INTERNAL_ERROR_CODE,
                        message: format!("failed to request device code: {err}"),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                }
            },
            Err(err) => {
                self.outgoing.send_error(request_id, err).await;
            }
        }
    }

    async fn cancel_login_chatgpt_common(
        &mut self,
        login_id: Uuid,
    ) -> std::result::Result<(), CancelLoginError> {
        let mut guard = self.active_login.lock().await;
        if guard.as_ref().map(|l| l.login_id) == Some(login_id) {
            if let Some(active) = guard.take() {
                drop(active);
            }
            Ok(())
        } else {
            Err(CancelLoginError::NotFound)
        }
    }

    pub(crate) async fn cancel_login_v2(
        &mut self,
        request_id: RequestId,
        params: CancelLoginAccountParams,
    ) {
        let login_id = params.login_id;
        match Uuid::parse_str(&login_id) {
            Ok(uuid) => {
                let status = match self.cancel_login_chatgpt_common(uuid).await {
                    Ok(()) => CancelLoginAccountStatus::Canceled,
                    Err(CancelLoginError::NotFound) => CancelLoginAccountStatus::NotFound,
                };
                let response = CancelLoginAccountResponse { status };
                self.outgoing.send_response(request_id, response).await;
            }
            Err(_) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("invalid login id: {login_id}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    async fn login_chatgpt_auth_tokens(
        &mut self,
        request_id: RequestId,
        id_token: String,
        access_token: String,
    ) {
        if matches!(
            self.config.forced_login_method,
            Some(ForcedLoginMethod::Api)
        ) {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: "External ChatGPT auth is disabled. Use API key login instead."
                    .to_string(),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        // Cancel any active login attempt to avoid persisting managed auth state.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                drop(active);
            }
        }

        let id_token_info = match parse_id_token(&id_token) {
            Ok(info) => info,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("invalid id token: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        if let Some(expected_workspace) = self.config.forced_chatgpt_workspace_id.as_deref()
            && id_token_info.chatgpt_account_id.as_deref() != Some(expected_workspace)
        {
            let account_id = id_token_info.chatgpt_account_id;
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!(
                    "External auth must use workspace {expected_workspace}, but received {account_id:?}."
                ),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        if let Err(err) =
            login_with_chatgpt_auth_tokens(&self.config.savfox_home, &id_token, &access_token)
        {
            let error = JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to set external auth: {err}"),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }
        self.auth_manager.reload();

        self.outgoing
            .send_response(request_id, LoginAccountResponse::ChatgptAuthTokens {})
            .await;

        let payload_login_completed = AccountLoginCompletedNotification {
            login_id: None,
            success: true,
            error: None,
        };
        self.outgoing
            .send_server_notification(ServerNotification::AccountLoginCompleted(
                payload_login_completed,
            ))
            .await;

        let payload_v2 = AccountUpdatedNotification {
            auth_mode: self.auth_manager.get_auth_mode(),
        };
        self.outgoing
            .send_server_notification(ServerNotification::AccountUpdated(payload_v2))
            .await;
    }

    async fn logout_common(&mut self) -> std::result::Result<Option<AuthMode>, JSONRPCErrorError> {
        // Cancel any active login attempt.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                drop(active);
            }
        }

        if let Err(err) = self.auth_manager.logout() {
            return Err(JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("logout failed: {err}"),
                data: None,
            });
        }

        // Reflect the current auth method after logout (likely None).
        Ok(self
            .auth_manager
            .auth_cached()
            .as_ref()
            .map(SavfoxAuth::api_auth_mode))
    }

    pub(crate) async fn logout_v2(&mut self, request_id: RequestId) {
        match self.logout_common().await {
            Ok(current_auth_method) => {
                self.outgoing
                    .send_response(request_id, LogoutAccountResponse {})
                    .await;

                let payload_v2 = AccountUpdatedNotification {
                    auth_mode: current_auth_method,
                };
                self.outgoing
                    .send_server_notification(ServerNotification::AccountUpdated(payload_v2))
                    .await;
            }
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    async fn refresh_token_if_requested(&self, do_refresh: bool) {
        if self.auth_manager.is_external_auth_active() {
            return;
        }
        if do_refresh && let Err(err) = self.auth_manager.refresh_token().await {
            tracing::warn!("failed to refresh token while getting account: {err}");
        }
    }

    pub(crate) async fn get_account(&self, request_id: RequestId, params: GetAccountParams) {
        let do_refresh = params.refresh_token;

        self.refresh_token_if_requested(do_refresh).await;

        // Whether auth is required for the active model provider.
        let requires_openai_auth = self.config.model_provider.requires_openai_auth;

        if !requires_openai_auth {
            let response = GetAccountResponse {
                account: None,
                requires_openai_auth,
            };
            self.outgoing.send_response(request_id, response).await;
            return;
        }

        let account = match self.auth_manager.auth_cached() {
            Some(auth) => Some(match auth {
                SavfoxAuth::ApiKey(_) => Account::ApiKey {},
                SavfoxAuth::Chatgpt(_) | SavfoxAuth::ChatgptAuthTokens(_) => {
                    let email = auth.get_account_email();
                    let plan_type = auth.account_plan_type();

                    match (email, plan_type) {
                        (Some(email), Some(plan_type)) => Account::Chatgpt { email, plan_type },
                        _ => {
                            let error = JSONRPCErrorError {
                                code: INVALID_REQUEST_ERROR_CODE,
                                message:
                                    "email and plan type are required for chatgpt authentication"
                                        .to_string(),
                                data: None,
                            };
                            self.outgoing.send_error(request_id, error).await;
                            return;
                        }
                    }
                }
            }),
            None => None,
        };

        let response = GetAccountResponse {
            account,
            requires_openai_auth,
        };
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn get_account_rate_limits(&self, request_id: RequestId) {
        match self.fetch_account_rate_limits().await {
            Ok(rate_limits) => {
                let response = GetAccountRateLimitsResponse {
                    rate_limits: rate_limits.into(),
                };
                self.outgoing.send_response(request_id, response).await;
            }
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    async fn fetch_account_rate_limits(&self) -> Result<CoreRateLimitSnapshot, JSONRPCErrorError> {
        Err(JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message: "rate limit fetching is not available".to_string(),
            data: None,
        })
    }
}
