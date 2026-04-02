use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use app_test_support::{
    DEFAULT_CLIENT_NAME, McpProcess, create_mock_responses_server_sequence_unchecked, to_response,
};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    ClientInfo, InitializeCapabilities, JSONRPCError, JSONRPCResponse, JsonRpcMessage,
    MockExperimentalMethodParams, RequestId, SessionStartParams, SessionStartResponse,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn mock_experimental_method_requires_experimental_api_capability() -> Result<()> {
    let savfox_home = TempDir::new()?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    let init = mcp
        .initialize_with_capabilities(
            default_client_info(),
            Some(InitializeCapabilities {
                experimental_api: false,
            }),
        )
        .await?;
    let JsonRpcMessage::Response(_) = init else {
        anyhow::bail!("expected initialize response, got {init:?}");
    };

    let request_id = mcp
        .send_mock_experimental_method_request(MockExperimentalMethodParams::default())
        .await?;
    let error = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_experimental_capability_error(error, "mock/experimentalMethod");
    Ok(())
}

#[tokio::test]
async fn session_start_mock_field_requires_experimental_api_capability() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    let init = mcp
        .initialize_with_capabilities(
            default_client_info(),
            Some(InitializeCapabilities {
                experimental_api: false,
            }),
        )
        .await?;
    let JsonRpcMessage::Response(_) = init else {
        anyhow::bail!("expected initialize response, got {init:?}");
    };

    let request_id = mcp
        .send_session_start_request(SessionStartParams {
            mock_experimental_field: Some("mock".to_owned()),
            ..Default::default()
        })
        .await?;

    let error = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_experimental_capability_error(error, "session/start.mockExperimentalField");
    Ok(())
}

#[tokio::test]
async fn session_start_without_dynamic_tools_allows_without_experimental_api_capability()
-> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    let init = mcp
        .initialize_with_capabilities(
            default_client_info(),
            Some(InitializeCapabilities {
                experimental_api: false,
            }),
        )
        .await?;
    let JsonRpcMessage::Response(_) = init else {
        anyhow::bail!("expected initialize response, got {init:?}");
    };

    let request_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("mock-model".to_owned()),
            ..Default::default()
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SessionStartResponse = to_response(response)?;
    Ok(())
}

fn default_client_info() -> ClientInfo {
    ClientInfo {
        name: DEFAULT_CLIENT_NAME.to_owned(),
        title: None,
        version: "0.1.0".to_owned(),
    }
}

fn assert_experimental_capability_error(error: JSONRPCError, reason: &str) {
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        format!("{reason} requires experimentalApi capability")
    );
    assert_eq!(error.error.data, None);
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
