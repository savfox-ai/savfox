use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, create_mock_responses_server_repeating_assistant, to_response};
use savfox_app_server_protocol::{
    JSONRPCNotification, JSONRPCResponse, RequestId, SessionStartParams, SessionStartResponse,
    SessionStartedNotification,
};
use savfox_core::config::set_project_trust_level;
use savfox_protocol::config_types::TrustLevel;
use savfox_protocol::openai_models::ReasoningEffort;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_start_creates_session_and_emits_started() -> Result<()> {
    // Provide a mock server and config so model wiring is valid.
    let server = create_mock_responses_server_repeating_assistant("Done").await;

    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    // Start server and initialize.
    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a v2 session with an explicit model override.
    let req_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.1".to_string()),
            ..Default::default()
        })
        .await?;

    // Expect a proper JSON-RPC response with a session id.
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let SessionStartResponse {
        session,
        model_provider,
        ..
    } = to_response::<SessionStartResponse>(resp)?;
    assert!(!session.id.is_empty(), "session id should not be empty");
    assert!(
        session.preview.is_empty(),
        "new sessions should start with an empty preview"
    );
    assert_eq!(model_provider, "mock_provider");
    assert!(
        session.created_at > 0,
        "created_at should be a positive UNIX timestamp"
    );

    // A corresponding session/started notification should arrive.
    let notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("session/started"),
    )
    .await??;
    let started: SessionStartedNotification =
        serde_json::from_value(notif.params.expect("params must be present"))?;
    assert_eq!(started.session, session);

    Ok(())
}

#[tokio::test]
async fn session_start_respects_project_config_from_cwd() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;

    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let workspace = TempDir::new()?;
    let project_config_dir = workspace.path().join(".savfox");
    std::fs::create_dir_all(&project_config_dir)?;
    std::fs::write(
        project_config_dir.join("config.toml"),
        r#"
model_reasoning_effort = "high"
"#,
    )?;
    set_project_trust_level(savfox_home.path(), workspace.path(), TrustLevel::Trusted)?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let req_id = mcp
        .send_session_start_request(SessionStartParams {
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let SessionStartResponse {
        reasoning_effort, ..
    } = to_response::<SessionStartResponse>(resp)?;

    assert_eq!(reasoning_effort, Some(ReasoningEffort::High));
    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
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
