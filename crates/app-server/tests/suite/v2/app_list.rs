use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use app_test_support::{ChatGptAuthFixture, McpProcess, to_response, write_chatgpt_auth};
use pretty_assertions::assert_eq;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    JsonObject, ListToolsResult, Meta, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use salvo::affix_state;
use salvo::http::StatusCode;
use salvo::http::header::AUTHORIZATION;
use salvo::prelude::*;
use savfox_app_server_protocol::{
    AppInfo, AppsListParams, AppsListResponse, JSONRPCResponse, RequestId,
};
use savfox_core::auth::AuthCredentialsStoreMode;
use serde_json::json;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn list_apps_returns_empty_when_connectors_disabled() -> Result<()> {
    let savfox_home = TempDir::new()?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_apps_list_request(AppsListParams {
            limit: Some(50),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let AppsListResponse { data, next_cursor } = to_response(response)?;

    assert!(data.is_empty());
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_apps_returns_connectors_with_accessible_flags() -> Result<()> {
    let connectors = vec![
        AppInfo {
            id: "alpha".to_owned(),
            name: "Alpha".to_owned(),
            description: Some("Alpha connector".to_owned()),
            logo_url: Some("https://example.com/alpha.png".to_owned()),
            logo_url_dark: None,
            distribution_channel: None,
            install_url: None,
            is_accessible: false,
        },
        AppInfo {
            id: "beta".to_owned(),
            name: "beta".to_owned(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            install_url: None,
            is_accessible: false,
        },
    ];

    let tools = vec![connector_tool("beta", "Beta App")?];
    let (server_url, server_handle) = start_apps_server(connectors.clone(), tools).await?;

    let savfox_home = TempDir::new()?;
    write_connectors_config(savfox_home.path(), &server_url)?;
    write_chatgpt_auth(
        savfox_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_apps_list_request(AppsListParams {
            limit: None,
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let AppsListResponse { data, next_cursor } = to_response(response)?;

    let expected = vec![
        AppInfo {
            id: "beta".to_owned(),
            name: "Beta App".to_owned(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            install_url: Some("https://savfox.ai/apps/beta/beta".to_owned()),
            is_accessible: true,
        },
        AppInfo {
            id: "alpha".to_owned(),
            name: "Alpha".to_owned(),
            description: Some("Alpha connector".to_owned()),
            logo_url: Some("https://example.com/alpha.png".to_owned()),
            logo_url_dark: None,
            distribution_channel: None,
            install_url: Some("https://savfox.ai/apps/alpha/alpha".to_owned()),
            is_accessible: false,
        },
    ];

    assert_eq!(data, expected);
    assert!(next_cursor.is_none());

    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn list_apps_paginates_results() -> Result<()> {
    let connectors = vec![
        AppInfo {
            id: "alpha".to_owned(),
            name: "Alpha".to_owned(),
            description: Some("Alpha connector".to_owned()),
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            install_url: None,
            is_accessible: false,
        },
        AppInfo {
            id: "beta".to_owned(),
            name: "beta".to_owned(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            install_url: None,
            is_accessible: false,
        },
    ];

    let tools = vec![connector_tool("beta", "Beta App")?];
    let (server_url, server_handle) = start_apps_server(connectors.clone(), tools).await?;

    let savfox_home = TempDir::new()?;
    write_connectors_config(savfox_home.path(), &server_url)?;
    write_chatgpt_auth(
        savfox_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .chatgpt_account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let first_request = mcp
        .send_apps_list_request(AppsListParams {
            limit: Some(1),
            cursor: None,
        })
        .await?;
    let first_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_request)),
    )
    .await??;
    let AppsListResponse {
        data: first_page,
        next_cursor: first_cursor,
    } = to_response(first_response)?;

    let expected_first = vec![AppInfo {
        id: "beta".to_owned(),
        name: "Beta App".to_owned(),
        description: None,
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        install_url: Some("https://savfox.ai/apps/beta/beta".to_owned()),
        is_accessible: true,
    }];

    assert_eq!(first_page, expected_first);
    let next_cursor = first_cursor.ok_or_else(|| anyhow::anyhow!("missing cursor"))?;

    let second_request = mcp
        .send_apps_list_request(AppsListParams {
            limit: Some(1),
            cursor: Some(next_cursor),
        })
        .await?;
    let second_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(second_request)),
    )
    .await??;
    let AppsListResponse {
        data: second_page,
        next_cursor: second_cursor,
    } = to_response(second_response)?;

    let expected_second = vec![AppInfo {
        id: "alpha".to_owned(),
        name: "Alpha".to_owned(),
        description: Some("Alpha connector".to_owned()),
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        install_url: Some("https://savfox.ai/apps/alpha/alpha".to_owned()),
        is_accessible: false,
    }];

    assert_eq!(second_page, expected_second);
    assert!(second_cursor.is_none());

    server_handle.abort();
    Ok(())
}

#[derive(Clone)]
struct AppsServerState {
    expected_bearer: String,
    expected_account_id: String,
    response: serde_json::Value,
}

#[derive(Clone)]
struct AppListMcpServer {
    tools: Arc<Vec<Tool>>,
}

impl AppListMcpServer {
    fn new(tools: Arc<Vec<Tool>>) -> Self {
        Self { tools }
    }
}

impl ServerHandler for AppListMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        let tools = self.tools.clone();
        async move {
            Ok(ListToolsResult {
                tools: (*tools).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }
}

async fn start_apps_server(
    connectors: Vec<AppInfo>,
    tools: Vec<Tool>,
) -> Result<(String, JoinHandle<()>)> {
    let state = AppsServerState {
        expected_bearer: "Bearer chatgpt-token".to_owned(),
        expected_account_id: "account-123".to_owned(),
        response: json!({ "apps": connectors, "next_token": null }),
    };
    let state = Arc::new(state);
    let tools = Arc::new(tools);

    let mcp_service = StreamableHttpService::new(
        {
            let tools = tools.clone();
            move || Ok(AppListMcpServer::new(tools.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mcp_handler: salvo_extra::tower_compat::TowerServiceHandler<_, salvo::http::ReqBody> =
        mcp_service.compat();

    let router = Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("connectors/directory/list").get(list_directory_connectors))
        .push(
            Router::with_path("connectors/directory/list_workspace").get(list_directory_connectors),
        )
        .push(Router::with_path("api/codex/apps{**rest}").goal(mcp_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let acceptor = TcpListener::new(addr.to_string()).bind().await;
    let handle = tokio::spawn(async move {
        Server::new(acceptor).serve(router).await;
    });

    Ok((format!("http://{addr}"), handle))
}

#[handler]
async fn list_directory_connectors(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let state = depot.obtain::<Arc<AppsServerState>>().unwrap();
    let headers = req.headers();

    let bearer_ok = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == state.expected_bearer);
    let account_ok = headers
        .get("chatgpt-account-id")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == state.expected_account_id);

    if bearer_ok && account_ok {
        res.render(Json(state.response.clone()));
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
    }
}

fn connector_tool(connector_id: &str, connector_name: &str) -> Result<Tool> {
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false
    }))?;
    let mut tool = Tool::new(
        Cow::Owned(format!("connector_{connector_id}")),
        Cow::Borrowed("Connector test tool"),
        Arc::new(schema),
    );
    tool.annotations = Some(ToolAnnotations::new().read_only(true));

    let mut meta = Meta::new();
    meta.0
        .insert("connector_id".to_owned(), json!(connector_id));
    meta.0
        .insert("connector_name".to_owned(), json!(connector_name));
    tool.meta = Some(meta);
    Ok(tool)
}

fn write_connectors_config(savfox_home: &std::path::Path, base_url: &str) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
chatgpt_base_url = "{base_url}"

[features]
connectors = true
"#
        ),
    )
}
