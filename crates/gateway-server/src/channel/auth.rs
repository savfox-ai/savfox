use savfox_app_server_protocol::{
    AccountLoginCompletedNotification, AccountUpdatedNotification, CancelLoginAccountParams,
    CancelLoginAccountResponse, CancelLoginAccountStatus, LoginAccountParams, LoginAccountResponse,
    RequestId,
};
use savfox_core::auth::{CLIENT_ID, login_with_api_key};
use savfox_login_oauth::{
    ServerOptions, complete_device_code_login, request_device_code, run_login_server,
};
use tracing::warn;
use uuid::Uuid;

use super::{ActiveLogin, BridgeOutgoing, GatewayChannel, INTERNAL_ERROR_CODE};

impl GatewayChannel {
    pub(in crate::channel) async fn handle_login_account(
        &self,
        request_id: RequestId,
        params: LoginAccountParams,
    ) {
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
            CLIENT_ID.to_owned(),
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
                        Err(_) => (false, Some("Login timed out".to_owned())),
                    };

                    {
                        let mut guard = active_login.lock().await;
                        if let Some(active) = guard.take()
                            && active.login_id != login_id {
                                *guard = Some(active);
                            }
                    }

                    let notification = AccountLoginCompletedNotification {
                        login_id: Some(login_id.to_string()),
                        success,
                        error: error_msg,
                    };
                    if let Err(err) = outgoing_tx
                        .send(BridgeOutgoing::Notification {
                            method: "account/login/completed".to_owned(),
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
                                method: "account/updated".to_owned(),
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
            CLIENT_ID.to_owned(),
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
                        Err(_) => (false, Some("Device code login timed out".to_owned())),
                    };

                    let notification = AccountLoginCompletedNotification {
                        login_id: Some(login_id.to_string()),
                        success,
                        error: error_msg,
                    };
                    if let Err(err) = outgoing_tx
                        .send(BridgeOutgoing::Notification {
                            method: "account/login/completed".to_owned(),
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
                                method: "account/updated".to_owned(),
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

    pub(in crate::channel) async fn handle_cancel_login_account(
        &self,
        request_id: RequestId,
        params: CancelLoginAccountParams,
    ) {
        let login_id = if let Ok(id) = Uuid::parse_str(&params.login_id) { id } else {
            let response = CancelLoginAccountResponse {
                status: CancelLoginAccountStatus::NotFound,
            };
            self.send_response(request_id, serde_json::to_value(response).unwrap())
                .await;
            return;
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
