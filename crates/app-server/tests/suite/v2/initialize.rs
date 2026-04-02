use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, create_mock_responses_server_sequence_unchecked, to_response};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{ClientInfo, InitializeResponse, JsonRpcMessage};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn initialize_uses_client_info_name_as_originator() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "savfox_vscode".to_owned(),
            title: Some("Savfox VS Code Extension".to_owned()),
            version: "0.1.0".to_owned(),
        }),
    )
    .await??;

    let JsonRpcMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got {message:?}");
    };
    let InitializeResponse { user_agent } = to_response::<InitializeResponse>(response)?;

    assert!(user_agent.starts_with("savfox_vscode/"));
    Ok(())
}

#[tokio::test]
async fn initialize_respects_originator_override_env_var() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new_with_env(
        savfox_home.path(),
        &[(
            "SAVFOX_INTERNAL_ORIGINATOR_OVERRIDE",
            Some("savfox_originator_via_env_var"),
        )],
    )
    .await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "savfox_vscode".to_owned(),
            title: Some("Savfox VS Code Extension".to_owned()),
            version: "0.1.0".to_owned(),
        }),
    )
    .await??;

    let JsonRpcMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got {message:?}");
    };
    let InitializeResponse { user_agent } = to_response::<InitializeResponse>(response)?;

    assert!(user_agent.starts_with("savfox_originator_via_env_var/"));
    Ok(())
}

#[tokio::test]
async fn initialize_rejects_invalid_client_name() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new_with_env(
        savfox_home.path(),
        &[("SAVFOX_INTERNAL_ORIGINATOR_OVERRIDE", None)],
    )
    .await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "bad\rname".to_owned(),
            title: Some("Bad Client".to_owned()),
            version: "0.1.0".to_owned(),
        }),
    )
    .await??;

    let JsonRpcMessage::Error(error) = message else {
        anyhow::bail!("expected initialize error, got {message:?}");
    };

    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        "Invalid clientInfo.name: 'bad\rname'. Must be a valid HTTP header value."
    );
    assert_eq!(error.error.data, None);
    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(
    savfox_home: &Path,
    server_uri: &str,
    approval_policy: &str,
) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "{approval_policy}"
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
