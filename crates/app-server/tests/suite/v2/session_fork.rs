use std::path::Path;

use anyhow::Result;
use app_test_support::{
    McpProcess, create_fake_rollout, create_mock_responses_server_repeating_assistant,
    rollout_path, to_response,
};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCNotification, JSONRPCResponse, RequestId, SessionForkParams, SessionForkResponse,
    SessionItem, SessionSource, SessionStartedNotification, TurnStatus, UserInput,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_fork_creates_new_session_and_emits_started() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let preview = "Saved user message";
    let filename_ts = "2025-01-05T12-00-00";
    let conversation_id = create_fake_rollout(
        savfox_home.path(),
        filename_ts,
        "2025-01-05T12:00:00Z",
        preview,
        Some("mock_provider"),
        None,
    )?;

    let original_path = rollout_path(savfox_home.path(), filename_ts, &conversation_id);
    assert!(
        original_path.exists(),
        "expected original rollout to exist at {}",
        original_path.display()
    );
    let original_contents = std::fs::read_to_string(&original_path)?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let fork_id = mcp
        .send_session_fork_request(SessionForkParams {
            session_id: conversation_id.clone(),
            ..Default::default()
        })
        .await?;
    let fork_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(fork_id)),
    )
    .await??;
    let SessionForkResponse { session, .. } = to_response::<SessionForkResponse>(fork_resp)?;

    let after_contents = std::fs::read_to_string(&original_path)?;
    assert_eq!(
        after_contents, original_contents,
        "fork should not mutate the original rollout file"
    );

    assert_ne!(session.id, conversation_id);
    assert_eq!(session.preview, preview);
    assert_eq!(session.model_provider, "mock_provider");
    let session_path = session.path.clone().expect("session path");
    assert!(session_path.is_absolute());
    assert_ne!(session_path, original_path);
    assert!(session.cwd.is_absolute());
    assert_eq!(session.source, SessionSource::VsCode);

    assert_eq!(
        session.turns.len(),
        1,
        "expected forked session to include one turn"
    );
    let turn = &session.turns[0];
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.items.len(), 1, "expected user message item");
    match &turn.items[0] {
        SessionItem::UserMessage { content, .. } => {
            assert_eq!(
                content,
                &vec![UserInput::Text {
                    text: preview.to_owned(),
                    text_elements: Vec::new(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

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
