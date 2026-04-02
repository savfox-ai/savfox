use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, create_mock_responses_server_repeating_assistant, to_response};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionLoadedListParams, SessionLoadedListResponse,
    SessionStartParams, SessionStartResponse,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_loaded_list_returns_loaded_session_ids() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let session_id = start_session(&mut mcp).await?;

    let list_id = mcp
        .send_session_loaded_list_request(SessionLoadedListParams::default())
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let SessionLoadedListResponse {
        mut data,
        next_cursor,
    } = to_response::<SessionLoadedListResponse>(resp)?;
    data.sort();
    assert_eq!(data, vec![session_id]);
    assert_eq!(next_cursor, None);

    Ok(())
}

#[tokio::test]
async fn session_loaded_list_paginates() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let first = start_session(&mut mcp).await?;
    let second = start_session(&mut mcp).await?;

    let mut expected = [first, second];
    expected.sort();

    let list_id = mcp
        .send_session_loaded_list_request(SessionLoadedListParams {
            cursor: None,
            limit: Some(1),
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let SessionLoadedListResponse {
        data: first_page,
        next_cursor,
    } = to_response::<SessionLoadedListResponse>(resp)?;
    assert_eq!(first_page, vec![expected[0].clone()]);
    assert_eq!(next_cursor, Some(expected[0].clone()));

    let list_id = mcp
        .send_session_loaded_list_request(SessionLoadedListParams {
            cursor: next_cursor,
            limit: Some(1),
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let SessionLoadedListResponse {
        data: second_page,
        next_cursor,
    } = to_response::<SessionLoadedListResponse>(resp)?;
    assert_eq!(second_page, vec![expected[1].clone()]);
    assert_eq!(next_cursor, None);

    Ok(())
}

fn create_config_toml(savfox_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

async fn start_session(mcp: &mut McpProcess) -> Result<String> {
    let req_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.1".to_owned()),
            ..Default::default()
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(resp)?;
    Ok(session.id)
}
