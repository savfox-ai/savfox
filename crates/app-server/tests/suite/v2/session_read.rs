use std::path::{Path, PathBuf};

use anyhow::Result;
use app_test_support::{
    McpProcess, create_fake_rollout_with_text_elements,
    create_mock_responses_server_repeating_assistant, to_response,
};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionItem, SessionReadParams, SessionReadResponse, SessionSource,
    TurnStatus, UserInput,
};
use savfox_protocol::user_input::{ByteRange, TextElement};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_read_returns_summary_without_turns() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let preview = "Saved user message";
    let text_elements = [TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".into()),
    )];
    let conversation_id = create_fake_rollout_with_text_elements(
        savfox_home.path(),
        "2025-01-05T12-00-00",
        "2025-01-05T12:00:00Z",
        preview,
        text_elements
            .iter()
            .map(|elem| serde_json::to_value(elem).expect("serialize text element"))
            .collect(),
        Some("mock_provider"),
        None,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_session_read_request(SessionReadParams {
            session_id: conversation_id.clone(),
            include_turns: false,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let SessionReadResponse { session } = to_response::<SessionReadResponse>(read_resp)?;

    assert_eq!(session.id, conversation_id);
    assert_eq!(session.preview, preview);
    assert_eq!(session.model_provider, "mock_provider");
    assert!(session.path.as_ref().expect("session path").is_absolute());
    assert_eq!(session.cwd, PathBuf::from("/"));
    assert_eq!(session.cli_version, "0.0.0");
    assert_eq!(session.source, SessionSource::Cli);
    assert_eq!(session.git_info, None);
    assert_eq!(session.turns.len(), 0);

    Ok(())
}

#[tokio::test]
async fn session_read_can_include_turns() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let preview = "Saved user message";
    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".into()),
    )];
    let conversation_id = create_fake_rollout_with_text_elements(
        savfox_home.path(),
        "2025-01-05T12-00-00",
        "2025-01-05T12:00:00Z",
        preview,
        text_elements
            .iter()
            .map(|elem| serde_json::to_value(elem).expect("serialize text element"))
            .collect(),
        Some("mock_provider"),
        None,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_session_read_request(SessionReadParams {
            session_id: conversation_id.clone(),
            include_turns: true,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let SessionReadResponse { session } = to_response::<SessionReadResponse>(read_resp)?;

    assert_eq!(session.turns.len(), 1);
    let turn = &session.turns[0];
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.items.len(), 1, "expected user message item");
    match &turn.items[0] {
        SessionItem::UserMessage { content, .. } => {
            assert_eq!(
                content,
                &vec![UserInput::Text {
                    text: preview.to_string(),
                    text_elements: text_elements.clone().into_iter().map(Into::into).collect(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

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
