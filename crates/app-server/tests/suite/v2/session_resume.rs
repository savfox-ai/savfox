use std::fs::FileTimes;
use std::path::{Path, PathBuf};

use anyhow::Result;
use app_test_support::{
    McpProcess, create_fake_rollout_with_text_elements,
    create_mock_responses_server_repeating_assistant, rollout_path, to_response,
};
use chrono::Utc;
use core_test_support::{responses, skip_if_no_network};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionItem, SessionResumeParams, SessionResumeResponse,
    SessionSource, SessionStartParams, SessionStartResponse, TurnStartParams, TurnStatus,
    UserInput,
};
use savfox_protocol::config_types::Personality;
use savfox_protocol::models::{ContentItem, ResponseItem};
use savfox_protocol::user_input::{ByteRange, TextElement};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const SAVFOX_5_2_BASE_INSTRUCTIONS_PREFIX: &str =
    "You are an AI assistant running in the Savfox CLI, a terminal-based AI assistant.";

#[tokio::test]
async fn session_resume_returns_original_session() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a session.
    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.1-savfox-max".to_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    // Resume it via v2 API.
    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: session.id.clone(),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse {
        session: resumed, ..
    } = to_response::<SessionResumeResponse>(resume_resp)?;
    assert_eq!(resumed.id, session.id);
    assert_eq!(resumed.path, session.path);
    assert_eq!(resumed.model_provider, session.model_provider);
    assert_eq!(resumed.cwd, session.cwd);
    assert_eq!(resumed.cli_version, session.cli_version);
    assert_eq!(resumed.source, session.source);
    assert_eq!(resumed.created_at, session.created_at);
    assert!(resumed.updated_at >= session.updated_at);
    assert!(
        resumed.turns.len() >= session.turns.len(),
        "resumed session should include at least the original turns"
    );

    Ok(())
}

#[tokio::test]
async fn session_resume_returns_rollout_history() -> Result<()> {
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

    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: conversation_id.clone(),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse { session, .. } = to_response::<SessionResumeResponse>(resume_resp)?;

    assert_eq!(session.id, conversation_id);
    assert_eq!(session.preview, preview);
    assert_eq!(session.model_provider, "mock_provider");
    assert!(session.path.as_ref().expect("session path").is_absolute());
    assert_eq!(session.cwd, PathBuf::from("/"));
    assert_eq!(session.cli_version, "0.0.0");
    assert_eq!(session.source, SessionSource::Cli);
    assert_eq!(session.git_info, None);

    assert_eq!(
        session.turns.len(),
        1,
        "expected rollouts to include one turn"
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
                    text_elements: text_elements.clone().into_iter().map(Into::into).collect(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn session_resume_without_overrides_does_not_change_updated_at_or_mtime() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    let rollout = setup_rollout_fixture(savfox_home.path(), &server.uri())?;
    let session_id = rollout.conversation_id.clone();

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: session_id.clone(),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse { session, .. } = to_response::<SessionResumeResponse>(resume_resp)?;

    assert_eq!(session.updated_at, rollout.expected_updated_at);

    let after_modified = std::fs::metadata(&rollout.rollout_file_path)?.modified()?;
    assert_eq!(after_modified, rollout.before_modified);

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            session_id,
            input: vec![UserInput::Text {
                text: "Hello".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let after_turn_modified = std::fs::metadata(&rollout.rollout_file_path)?.modified()?;
    assert!(after_turn_modified > rollout.before_modified);

    Ok(())
}

#[tokio::test]
async fn session_resume_with_overrides_defers_updated_at_until_turn_start() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    let rollout = setup_rollout_fixture(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: rollout.conversation_id.clone(),
            model: Some("mock-model".to_owned()),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse { session, .. } = to_response::<SessionResumeResponse>(resume_resp)?;

    assert_eq!(session.updated_at, rollout.expected_updated_at);

    let after_resume_modified = std::fs::metadata(&rollout.rollout_file_path)?.modified()?;
    assert_eq!(after_resume_modified, rollout.before_modified);

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            session_id: rollout.conversation_id,
            input: vec![UserInput::Text {
                text: "Hello".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let after_turn_modified = std::fs::metadata(&rollout.rollout_file_path)?.modified()?;
    assert!(after_turn_modified > rollout.before_modified);

    Ok(())
}

#[tokio::test]
async fn session_resume_prefers_path_over_session_id() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.1-savfox-max".to_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    let session_path = session.path.clone().expect("session path");
    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: "not-a-valid-session-id".to_owned(),
            path: Some(session_path),
            ..Default::default()
        })
        .await?;

    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse {
        session: resumed, ..
    } = to_response::<SessionResumeResponse>(resume_resp)?;
    assert_eq!(resumed.id, session.id);
    assert_eq!(resumed.path, session.path);
    assert_eq!(resumed.model_provider, session.model_provider);
    assert_eq!(resumed.cwd, session.cwd);
    assert_eq!(resumed.cli_version, session.cli_version);
    assert_eq!(resumed.source, session.source);
    assert_eq!(resumed.created_at, session.created_at);
    assert!(resumed.updated_at >= session.updated_at);
    assert!(
        resumed.turns.len() >= session.turns.len(),
        "resumed session should include at least the original turns"
    );

    Ok(())
}

#[tokio::test]
async fn session_resume_supports_history_and_overrides() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a session.
    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.1-savfox-max".to_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    let history_text = "Hello from history";
    let history = vec![ResponseItem::Message {
        id: None,
        role: "user".to_owned(),
        content: vec![ContentItem::InputText {
            text: history_text.to_owned(),
        }],
        end_turn: None,
        phase: None,
    }];

    // Resume with explicit history and override the model.
    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: session.id,
            history: Some(history),
            model: Some("mock-model".to_owned()),
            model_provider: Some("mock_provider".to_owned()),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let SessionResumeResponse {
        session: resumed,
        model_provider,
        ..
    } = to_response::<SessionResumeResponse>(resume_resp)?;
    assert!(!resumed.id.is_empty());
    assert_eq!(model_provider, "mock_provider");
    assert_eq!(resumed.preview, history_text);

    Ok(())
}

#[tokio::test]
async fn session_resume_accepts_personality_override() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("gpt-5.2-savfox".to_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    let resume_id = mcp
        .send_session_resume_request(SessionResumeParams {
            session_id: session.id.clone(),
            model: Some("gpt-5.2-savfox".to_owned()),
            personality: Some(Personality::Pragmatic),
            ..Default::default()
        })
        .await?;
    let resume_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;
    let _resume: SessionResumeResponse = to_response::<SessionResumeResponse>(resume_resp)?;

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            session_id: session.id,
            input: vec![UserInput::Text {
                text: "Hello".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let instructions_text = request.instructions_text();
    assert!(
        instructions_text.contains(SAVFOX_5_2_BASE_INSTRUCTIONS_PREFIX),
        "expected default base instructions from history, got {instructions_text:?}"
    );
    assert!(
        !instructions_text.contains("<personality_spec>"),
        "expected no personality override wrapper in fallback instructions, got {instructions_text:?}"
    );

    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(savfox_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
approval_policy = "never"
sandbox_mode = "read-only"

model_provider = "mock_provider"

[model]
slug = "gpt-5.2-savfox"
provider = "mock_provider"

[features]
remote_models = false
personality = true

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

fn set_rollout_mtime(path: &Path, updated_at_rfc3339: &str) -> Result<()> {
    let parsed = chrono::DateTime::parse_from_rfc3339(updated_at_rfc3339)?.with_timezone(&Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)?
        .set_times(times)?;
    Ok(())
}

struct RolloutFixture {
    conversation_id: String,
    rollout_file_path: PathBuf,
    before_modified: std::time::SystemTime,
    expected_updated_at: i64,
}

fn setup_rollout_fixture(savfox_home: &Path, server_uri: &str) -> Result<RolloutFixture> {
    create_config_toml(savfox_home, server_uri)?;

    let preview = "Saved user message";
    let filename_ts = "2025-01-05T12-00-00";
    let meta_rfc3339 = "2025-01-05T12:00:00Z";
    let expected_updated_at_rfc3339 = "2025-01-07T00:00:00Z";
    let conversation_id = create_fake_rollout_with_text_elements(
        savfox_home,
        filename_ts,
        meta_rfc3339,
        preview,
        Vec::new(),
        Some("mock_provider"),
        None,
    )?;
    let rollout_file_path = rollout_path(savfox_home, filename_ts, &conversation_id);
    set_rollout_mtime(rollout_file_path.as_path(), expected_updated_at_rfc3339)?;
    let before_modified = std::fs::metadata(&rollout_file_path)?.modified()?;
    let expected_updated_at = chrono::DateTime::parse_from_rfc3339(expected_updated_at_rfc3339)?
        .with_timezone(&Utc)
        .timestamp();

    Ok(RolloutFixture {
        conversation_id,
        rollout_file_path,
        before_modified,
        expected_updated_at,
    })
}
