use std::borrow::Cow;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, JsonObject, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParam, RawResource, RawResourceTemplate,
    ReadResourceRequestParam, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use salvo::affix_state;
use salvo::http::StatusCode;
use salvo::http::header::{AUTHORIZATION, CONTENT_TYPE};
use salvo::prelude::*;
use serde::Deserialize;
use serde_json::json;
use tokio::task;

#[derive(Clone)]
struct TestToolServer {
    tools: Arc<Vec<Tool>>,
    resources: Arc<Vec<Resource>>,
    resource_templates: Arc<Vec<ResourceTemplate>>,
}

const MEMO_URI: &str = "memo://savfox/example-note";
const MEMO_CONTENT: &str = "This is a sample MCP resource served by the rmcp test server.";

impl TestToolServer {
    fn new() -> Self {
        let tools = vec![Self::echo_tool()];
        let resources = vec![Self::memo_resource()];
        let resource_templates = vec![Self::memo_template()];
        Self {
            tools: Arc::new(tools),
            resources: Arc::new(resources),
            resource_templates: Arc::new(resource_templates),
        }
    }

    fn echo_tool() -> Tool {
        #[expect(clippy::expect_used)]
        let schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" },
                "env_var": { "type": "string" }
            },
            "required": ["message"],
            "additionalProperties": false
        }))
        .expect("echo tool schema should deserialize");

        Tool::new(
            Cow::Borrowed("echo"),
            Cow::Borrowed("Echo back the provided message and include environment data."),
            Arc::new(schema),
        )
    }

    fn memo_resource() -> Resource {
        let raw = RawResource {
            uri: MEMO_URI.to_string(),
            name: "example-note".to_string(),
            title: Some("Example Note".to_string()),
            description: Some("A sample MCP resource exposed for integration tests.".to_string()),
            mime_type: Some("text/plain".to_string()),
            size: None,
            icons: None,
            meta: None,
        };
        Resource::new(raw, None)
    }

    fn memo_template() -> ResourceTemplate {
        let raw = RawResourceTemplate {
            uri_template: "memo://savfox/{slug}".to_string(),
            name: "savfox-memo".to_string(),
            title: Some("Savfox Memo".to_string()),
            description: Some(
                "Template for memo://savfox/{slug} resources used in tests.".to_string(),
            ),
            mime_type: Some("text/plain".to_string()),
        };
        ResourceTemplate::new(raw, None)
    }

    fn memo_text() -> &'static str {
        MEMO_CONTENT
    }
}

#[derive(Deserialize)]
struct EchoArgs {
    message: String,
    #[allow(dead_code)]
    env_var: Option<String>,
}

impl ServerHandler for TestToolServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .build(),
            ..ServerInfo::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools = self.tools.clone();
        async move {
            Ok(ListToolsResult {
                tools: (*tools).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        let resources = self.resources.clone();
        async move {
            Ok(ListResourcesResult {
                resources: (*resources).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: (*self.resource_templates).clone(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        ReadResourceRequestParam { uri }: ReadResourceRequestParam,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if uri == MEMO_URI {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::TextResourceContents {
                    uri,
                    mime_type: Some("text/plain".to_string()),
                    text: Self::memo_text().to_string(),
                    meta: None,
                }],
            })
        } else {
            Err(McpError::resource_not_found(
                "resource_not_found",
                Some(json!({ "uri": uri })),
            ))
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "echo" => {
                let args: EchoArgs = match request.arguments {
                    Some(arguments) => serde_json::from_value(serde_json::Value::Object(
                        arguments.into_iter().collect(),
                    ))
                    .map_err(|err| McpError::invalid_params(err.to_string(), None))?,
                    None => {
                        return Err(McpError::invalid_params(
                            "missing arguments for echo tool",
                            None,
                        ));
                    }
                };

                let env_snapshot: HashMap<String, String> = std::env::vars().collect();
                let structured_content = json!({
                    "echo": format!("ECHOING: {}", args.message),
                    "env": env_snapshot.get("MCP_TEST_VALUE"),
                });

                Ok(CallToolResult {
                    content: Vec::new(),
                    structured_content: Some(structured_content),
                    is_error: Some(false),
                    meta: None,
                })
            }
            other => Err(McpError::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

fn parse_bind_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let default_addr = "127.0.0.1:3920";
    let bind_addr = std::env::var("MCP_STREAMABLE_HTTP_BIND_ADDR")
        .or_else(|_| std::env::var("BIND_ADDR"))
        .unwrap_or_else(|_| default_addr.to_string());
    Ok(bind_addr.parse()?)
}

#[handler]
async fn oauth_metadata(req: &mut Request, res: &mut Response) {
    let bind_addr = req
        .uri()
        .authority()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "127.0.0.1:3920".to_string());
    let metadata_base = format!("http://{bind_addr}");
    #[expect(clippy::expect_used)]
    let body = serde_json::to_vec(&json!({
        "authorization_endpoint": format!("{metadata_base}/oauth/authorize"),
        "token_endpoint": format!("{metadata_base}/oauth/token"),
        "scopes_supported": [""],
    }))
    .expect("failed to serialize metadata");
    res.status_code(StatusCode::OK);
    res.headers_mut()
        .insert(CONTENT_TYPE, "application/json".parse().unwrap());
    res.write_body(body).ok();
}

#[handler]
async fn require_bearer(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    if req.uri().path().contains("/.well-known/") {
        return;
    }
    let expected = depot.obtain::<Arc<String>>().unwrap();
    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .is_some_and(|value| value.as_bytes() == expected.as_bytes());
    if !authorized {
        res.status_code(StatusCode::UNAUTHORIZED);
        ctrl.skip_rest();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = parse_bind_addr()?;

    // Verify we can bind before starting the server.
    match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(_listener) => { /* drop it; salvo will re-bind */ }
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            eprintln!(
                "failed to bind to {bind_addr}: {err}. make sure the process has network access"
            );
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    eprintln!("starting rmcp streamable http test server on http://{bind_addr}/mcp");

    let mcp_service = StreamableHttpService::new(
        || Ok(TestToolServer::new()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mcp_handler: salvo_extra::tower_compat::TowerServiceHandler<_, salvo::http::ReqBody> =
        mcp_service.compat();

    let mut router = Router::new()
        .push(Router::with_path("/.well-known/oauth-authorization-server/mcp").get(oauth_metadata))
        .push(Router::with_path("mcp/<**rest>").goal(mcp_handler));

    if let Ok(token) = std::env::var("MCP_EXPECT_BEARER") {
        let expected = Arc::new(format!("Bearer {token}"));
        router = Router::new()
            .hoop(affix_state::inject(expected))
            .hoop(require_bearer)
            .push(router);
    }

    let acceptor = TcpListener::new(bind_addr.to_string()).bind().await;
    Server::new(acceptor).serve(router).await;
    task::yield_now().await;
    Ok(())
}
