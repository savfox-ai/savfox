use anyhow::Result;
use app_test_support::{
    McpProcess, create_final_assistant_message_sse_response,
    create_mock_responses_server_sequence_unchecked, to_response,
};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionItem, SessionResumeParams, SessionResumeResponse,
    SessionRollbackParams, SessionRollbackResponse, SessionStartParams, SessionStartResponse,
    TurnStartParams, UserInput as V2UserInput,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_rollback_drops_last_turns_and_persists_to_rollout() -> Result<()> {
    // Three Savfox turns hit the mock model (session start + two turn/start calls).
    let responses = vec![
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a session.
    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("mock-model".to_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    // Two turns.
    let first_text = "First";
    let turn1_id = mcp
        .send_turn_start_request(TurnStartParams {
            session_id: session.id.clone(),
            input: vec![V2UserInput::Text {
                text: first_text.to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _turn1_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn1_id)),
    )
    .await??;
    let _completed1 = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let turn2_id = mcp
        .send_turn_start_request(TurnStartParams {
            session_id: session.id.clone(),
            input: vec![V2UserInput::Text {
                text: "Second".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _turn2_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn2_id)),
    )
    .await??;
    let _completed2 = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    // Roll back the last turn.
    let rollback_id = mcp
        .send_session_rollback_request(SessionRollbackParams {
            session_id: session.id.clone(),
            num_turns: 1,
        })
        .await?;
    let rollback_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(rollback_id)),
    )
    .await??;
    let SessionRollbackResponse {
        session: rolled_back_session,
    } = to_response::<SessionRollbackResponse>(rollback_resp)?;

    assert_eq!(rolled_back_session.turns.len(), 1);
    assert_eq!(rolled_back_session.turns[0].items.len(), 2);
    match &rolled_back_session.turns[0].items[0] {
        SessionItem::UserMessage { content, .. } => {
            assert_eq!(
                content,
                &vec![V2UserInput::Text {
                    text: first_text.to_owned(),
                    text_elements: Vec::new(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    // Resume and confirm the history is pruned.
    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: session.id,
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse { session, .. } = to_response::<SessionResumeResponse>(resume_resp)?;

    assert_eq!(session.turns.len(), 1);
    assert_eq!(session.turns[0].items.len(), 2);
    match &session.turns[0].items[0] {
        SessionItem::UserMessage { content, .. } => {
            assert_eq!(
                content,
                &vec![V2UserInput::Text {
                    text: first_text.to_owned(),
                    text_elements: Vec::new(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    Ok(())
}

fn create_config_toml(savfox_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
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
