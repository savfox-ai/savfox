#![allow(clippy::unwrap_used)]

mod support;

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use salvo::affix_state;
use salvo::http::StatusCode;
use salvo::http::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use salvo::prelude::*;
use serde_json::json;
use support::TestToolServer;
use tokio::task;

fn parse_bind_addr() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let default_addr = "127.0.0.1:3920";
    let bind_addr = std::env::var("MCP_STREAMABLE_HTTP_BIND_ADDR")
        .or_else(|_| std::env::var("BIND_ADDR"))
        .unwrap_or_else(|_| default_addr.to_owned());
    Ok(bind_addr.parse()?)
}

#[handler]
async fn oauth_metadata(req: &mut Request, res: &mut Response) {
    let bind_addr = req
        .uri()
        .authority()
        .map(|a| a.to_string())
        .or_else(|| {
            req.headers()
                .get(HOST)
                .and_then(|host| host.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "127.0.0.1:3920".to_owned());
    let metadata_base = format!("http://{bind_addr}");
    #[expect(clippy::expect_used)]
    let body = serde_json::to_vec(&json!({
        "issuer": format!("{metadata_base}/mcp"),
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
    let expected = depot.get_typed::<Arc<String>>().unwrap();
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
        || Ok(TestToolServer::new(false)),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mcp_handler = TowerServiceCompat::<salvo::http::ReqBody, _, _, _>::compat(mcp_service);

    let mut router = Router::new()
        .push(Router::with_path("/.well-known/oauth-authorization-server/mcp").get(oauth_metadata))
        .push(Router::with_path("mcp").goal(mcp_handler));

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
