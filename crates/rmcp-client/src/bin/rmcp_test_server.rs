#[path = "support/echo.rs"]
mod echo;

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::{ErrorData as McpError, ServiceExt};
use tokio::task;

#[derive(Clone)]
struct TestToolServer {
    tools: Arc<Vec<Tool>>,
}
#[must_use]
pub fn stdio() -> (tokio::io::Stdin, tokio::io::Stdout) {
    (tokio::io::stdin(), tokio::io::stdout())
}
impl TestToolServer {
    fn new() -> Self {
        let tools = vec![echo::tool()];
        Self {
            tools: Arc::new(tools),
        }
    }
}

impl ServerHandler for TestToolServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools = self.tools.clone();
        async move { Ok(ListToolsResult::with_all_items((*tools).clone())) }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match request.name.as_ref() {
            "echo" => echo::call(request, ""),
            other => Err(McpError::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("starting rmcp test server");
    // Run the server with STDIO transport. If the client disconnects we simply
    // bubble up the error so the process exits.
    let service = TestToolServer::new();
    let running = service.serve(stdio()).await?;

    // Wait for the client to finish interacting with the server.
    running.waiting().await?;
    // Drain background tasks to ensure clean shutdown.
    task::yield_now().await;
    Ok(())
}
