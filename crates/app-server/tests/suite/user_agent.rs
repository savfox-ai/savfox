use anyhow::Result;
use app_test_support::{DEFAULT_CLIENT_NAME, McpProcess, to_response};
use savfox_app_server_protocol::{GetUserAgentResponse, JSONRPCResponse, RequestId};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_user_agent_returns_current_savfox_user_agent() -> Result<()> {
    let savfox_home = TempDir::new()?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp.send_get_user_agent_request().await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let received: GetUserAgentResponse = to_response(response)?;
    assert!(
        received
            .user_agent
            .starts_with(&format!("{DEFAULT_CLIENT_NAME}/")),
        "user agent should start with client originator, got {:?}",
        received.user_agent
    );
    assert!(
        received
            .user_agent
            .contains(&format!("({DEFAULT_CLIENT_NAME}; 0.1.0)")),
        "user agent should include initialize client suffix, got {:?}",
        received.user_agent
    );
    Ok(())
}
