use std::time::Duration;

use anyhow::Result;
use app_test_support::{ChatGptAuthFixture, McpProcess, to_response, write_chatgpt_auth};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{JSONRPCResponse, RequestId, UserInfoResponse};
use savfox_core::auth::AuthCredentialsStoreMode;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_session", worker_sessions = 2)]
async fn user_info_returns_email_from_chatgpt_provider_store() -> Result<()> {
    let savfox_home = TempDir::new()?;

    write_chatgpt_auth(
        savfox_home.path(),
        ChatGptAuthFixture::new("access")
            .refresh_token("refresh")
            .email("user@example.com"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp.send_user_info_request().await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let received: UserInfoResponse = to_response(response)?;
    let expected = UserInfoResponse {
        alleged_user_email: Some("user@example.com".to_string()),
    };

    assert_eq!(received, expected);
    Ok(())
}
