#![cfg(unix)]

use anyhow::Result;
use app_test_support::{
    McpProcess, create_mock_responses_server_sequence, create_shell_command_sse_response,
    to_response,
};
use savfox_app_server_protocol::{
    JSONRPCNotification, JSONRPCResponse, RequestId, SessionStartParams, SessionStartResponse,
    TurnCompletedNotification, TurnInterruptParams, TurnInterruptResponse, TurnStartParams,
    TurnStartResponse, TurnStatus, UserInput as V2UserInput,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn turn_interrupt_aborts_running_turn() -> Result<()> {
    // Use a portable sleep command to keep the turn running.
    #[cfg(target_os = "windows")]
    let shell_command = vec![
        "powershell".to_string(),
        "-Command".to_string(),
        "Start-Sleep -Seconds 10".to_string(),
    ];
    #[cfg(not(target_os = "windows"))]
    let shell_command = vec!["sleep".to_string(), "10".to_string()];

    let tmp = TempDir::new()?;
    let savfox_home = tmp.path().join("savfox_home");
    std::fs::create_dir(&savfox_home)?;
    let working_directory = tmp.path().join("workdir");
    std::fs::create_dir(&working_directory)?;

    // Mock server: long-running shell command then (after abort) nothing else needed.
    let server = create_mock_responses_server_sequence(vec![create_shell_command_sse_response(
        shell_command.clone(),
        Some(&working_directory),
        Some(10_000),
        "call_sleep",
    )?])
    .await;
    create_config_toml(&savfox_home, &server.uri())?;

    let mut mcp = McpProcess::new(&savfox_home).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a v2 session and capture its id.
    let session_req = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let session_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(session_req)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(session_resp)?;

    // Start a turn that triggers a long-running command.
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            session_id: session.id.clone(),
            input: vec![V2UserInput::Text {
                text: "run sleep".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(working_directory.clone()),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    // Give the command a brief moment to start.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let session_id = session.id.clone();
    // Interrupt the in-progress turn by id (v2 API).
    let interrupt_id = mcp
        .send_turn_interrupt_request(TurnInterruptParams {
            session_id: session_id.clone(),
            turn_id: turn.id,
        })
        .await?;
    let interrupt_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(interrupt_id)),
    )
    .await??;
    let _resp: TurnInterruptResponse = to_response::<TurnInterruptResponse>(interrupt_resp)?;

    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.session_id, session_id);
    assert_eq!(completed.turn.status, TurnStatus::Interrupted);

    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(savfox_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "workspace-write"

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
