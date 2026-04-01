//! MCP lifecycle: server startup, shutdown, tool registration, and resource access.

use super::*;

impl Session {
    pub(super) async fn refresh_mcp_servers_inner(
        &self,
        turn_context: &TurnContext,
        mcp_servers: HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
    ) {
        let auth = self.services.auth_manager.auth().await;
        let config = self.get_config().await;
        let mcp_servers = with_savfox_apps_mcp(
            mcp_servers,
            self.features.enabled(Feature::Apps),
            auth.as_ref(),
            config.as_ref(),
        );
        let auth_statuses = compute_auth_statuses(mcp_servers.iter(), store_mode).await;
        let sandbox_state = SandboxState {
            sandbox_policy: turn_context.sandbox_policy.clone(),
            savfox_linux_sandbox_exe: turn_context.savfox_linux_sandbox_exe.clone(),
            sandbox_cwd: turn_context.cwd.clone(),
        };
        let cancel_token = self.reset_mcp_startup_cancellation_token().await;

        let mut refreshed_manager = McpConnectionManager::default();
        refreshed_manager
            .initialize(
                &mcp_servers,
                store_mode,
                auth_statuses,
                self.get_tx_event(),
                cancel_token,
                sandbox_state,
            )
            .await;

        let mut manager = self.services.mcp_connection_manager.write().await;
        *manager = refreshed_manager;
    }

    pub(super) async fn refresh_mcp_servers_if_requested(&self, turn_context: &TurnContext) {
        let refresh_config = { self.pending_mcp_server_refresh_config.lock().await.take() };
        let Some(refresh_config) = refresh_config else {
            return;
        };

        let McpServerRefreshConfig {
            mcp_servers,
            mcp_oauth_credentials_store_mode,
        } = refresh_config;

        let mcp_servers =
            match serde_json::from_value::<HashMap<String, McpServerConfig>>(mcp_servers) {
                Ok(servers) => servers,
                Err(err) => {
                    warn!("failed to parse MCP server refresh config: {err}");
                    return;
                }
            };
        let store_mode = match serde_json::from_value::<OAuthCredentialsStoreMode>(
            mcp_oauth_credentials_store_mode,
        ) {
            Ok(mode) => mode,
            Err(err) => {
                warn!("failed to parse MCP OAuth refresh config: {err}");
                return;
            }
        };

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }

    pub(crate) async fn refresh_mcp_servers_now(
        &self,
        turn_context: &TurnContext,
        mcp_servers: HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
    ) {
        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }

    pub(super) async fn mcp_startup_cancellation_token(&self) -> CancellationToken {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .clone()
    }

    async fn reset_mcp_startup_cancellation_token(&self) -> CancellationToken {
        let mut guard = self.services.mcp_startup_cancellation_token.lock().await;
        guard.cancel();
        let cancel_token = CancellationToken::new();
        *guard = cancel_token.clone();
        cancel_token
    }

    pub(super) async fn cancel_mcp_startup(&self) {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .cancel();
    }

    pub async fn resolve_elicitation(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> anyhow::Result<()> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .resolve_elicitation(server_name, id, response)
            .await
    }

    pub async fn list_resources(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourcesResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .list_resources(server, params)
            .await
    }

    pub async fn list_resource_templates(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourceTemplatesResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .list_resource_templates(server, params)
            .await
    }

    pub async fn read_resource(
        &self,
        server: &str,
        params: ReadResourceRequestParams,
    ) -> anyhow::Result<ReadResourceResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .read_resource(server, params)
            .await
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
    ) -> anyhow::Result<CallToolResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .call_tool(server, tool, arguments)
            .await
    }

    pub(crate) async fn parse_mcp_tool_name(&self, tool_name: &str) -> Option<(String, String)> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .parse_tool_name(tool_name)
            .await
    }
}
