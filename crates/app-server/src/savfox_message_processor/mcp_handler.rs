use std::sync::Arc;

use savfox_app_server_protocol::{
    JSONRPCErrorError, ListMcpServerStatusParams, ListMcpServerStatusResponse,
    McpServerOauthLoginCompletedNotification, McpServerOauthLoginParams,
    McpServerOauthLoginResponse, McpServerRefreshResponse, McpServerStatus, RequestId,
    ServerNotification,
};
use savfox_core::config::Config;
use savfox_core::config::types::McpServerTransportConfig;
use savfox_core::mcp::{collect_mcp_snapshot, group_tools_by_server};
use savfox_protocol::protocol::{McpAuthStatus as CoreMcpAuthStatus, McpServerRefreshConfig};
use savfox_rmcp_client::perform_oauth_login_return_url;

use super::{INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE, SavfoxMessageProcessor};
use crate::outgoing_message::OutgoingMessageSender;

impl SavfoxMessageProcessor {
    pub(crate) async fn mcp_server_refresh(&self, request_id: RequestId, _params: Option<()>) {
        let config = match self.load_latest_config().await {
            Ok(config) => config,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let mcp_servers = match serde_json::to_value(config.mcp_servers.get()) {
            Ok(value) => value,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to serialize MCP servers: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let mcp_oauth_credentials_store_mode =
            match serde_json::to_value(config.mcp_oauth_credentials_store_mode) {
                Ok(value) => value,
                Err(err) => {
                    let error = JSONRPCErrorError {
                        code: INTERNAL_ERROR_CODE,
                        message: format!(
                            "failed to serialize MCP OAuth credentials store mode: {err}"
                        ),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            };

        let refresh_config = McpServerRefreshConfig {
            mcp_servers,
            mcp_oauth_credentials_store_mode,
        };

        // Refresh requests are queued per session; each session rebuilds MCP connections on its
        // next active turn to avoid work for sessions that never resume.
        let session_manager = Arc::clone(&self.session_manager);
        session_manager.refresh_mcp_servers(refresh_config).await;
        let response = McpServerRefreshResponse {};
        self.outgoing.send_response(request_id, response).await;
    }

    pub(crate) async fn mcp_server_oauth_login(
        &self,
        request_id: RequestId,
        params: McpServerOauthLoginParams,
    ) {
        let config = match self.load_latest_config().await {
            Ok(config) => config,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let McpServerOauthLoginParams {
            name,
            scopes,
            timeout_secs,
        } = params;

        let Some(server) = config.mcp_servers.get().get(&name) else {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("No MCP server named '{name}' found."),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        };

        let (url, http_headers, env_http_headers) =
            if let McpServerTransportConfig::StreamableHttp {
                url,
                http_headers,
                env_http_headers,
                ..
            } = &server.transport
            {
                (url.clone(), http_headers.clone(), env_http_headers.clone())
            } else {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: "OAuth login is only supported for streamable HTTP servers."
                        .to_owned(),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            };

        let scopes = scopes.or_else(|| server.scopes.clone());

        match perform_oauth_login_return_url(
            &name,
            &url,
            config.mcp_oauth_credentials_store_mode,
            http_headers,
            env_http_headers,
            scopes.as_deref().unwrap_or_default(),
            timeout_secs,
            config.mcp_oauth_callback_port,
        )
        .await
        {
            Ok(handle) => {
                let authorization_url = handle.authorization_url().to_owned();
                let notification_name = name.clone();
                let outgoing = Arc::clone(&self.outgoing);

                tokio::spawn(async move {
                    let (success, error) = match handle.wait().await {
                        Ok(()) => (true, None),
                        Err(err) => (false, Some(err.to_string())),
                    };

                    let notification = ServerNotification::McpServerOauthLoginCompleted(
                        McpServerOauthLoginCompletedNotification {
                            name: notification_name,
                            success,
                            error,
                        },
                    );
                    outgoing.send_server_notification(notification).await;
                });

                let response = McpServerOauthLoginResponse { authorization_url };
                self.outgoing.send_response(request_id, response).await;
            }
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INTERNAL_ERROR_CODE,
                    message: format!("failed to login to MCP server '{name}': {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
            }
        }
    }

    pub(crate) async fn list_mcp_server_status(
        &self,
        request_id: RequestId,
        params: ListMcpServerStatusParams,
    ) {
        let outgoing = Arc::clone(&self.outgoing);
        let config = match self.load_latest_config().await {
            Ok(config) => config,
            Err(error) => {
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        tokio::spawn(async move {
            Self::list_mcp_server_status_task(outgoing, request_id, params, config).await;
        });
    }

    async fn list_mcp_server_status_task(
        outgoing: Arc<OutgoingMessageSender>,
        request_id: RequestId,
        params: ListMcpServerStatusParams,
        config: Config,
    ) {
        let snapshot = collect_mcp_snapshot(&config).await;

        let tools_by_server = group_tools_by_server(&snapshot.tools);

        let mut server_names: Vec<String> = config
            .mcp_servers
            .keys()
            .cloned()
            .chain(snapshot.auth_statuses.keys().cloned())
            .chain(snapshot.resources.keys().cloned())
            .chain(snapshot.resource_templates.keys().cloned())
            .collect();
        server_names.sort();
        server_names.dedup();

        let total = server_names.len();
        let limit = params.limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = limit.min(total);
        let start = match params.cursor {
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
                message: format!("cursor {start} exceeds total MCP servers {total}"),
                data: None,
            };
            outgoing.send_error(request_id, error).await;
            return;
        }

        let end = start.saturating_add(effective_limit).min(total);

        let data: Vec<McpServerStatus> = server_names[start..end]
            .iter()
            .map(|name| McpServerStatus {
                name: name.clone(),
                tools: tools_by_server.get(name).cloned().unwrap_or_default(),
                resources: snapshot.resources.get(name).cloned().unwrap_or_default(),
                resource_templates: snapshot
                    .resource_templates
                    .get(name)
                    .cloned()
                    .unwrap_or_default(),
                auth_status: snapshot
                    .auth_statuses
                    .get(name)
                    .cloned()
                    .unwrap_or(CoreMcpAuthStatus::Unsupported)
                    .into(),
            })
            .collect();

        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };

        let response = ListMcpServerStatusResponse { data, next_cursor };

        outgoing.send_response(request_id, response).await;
    }
}
