use std::fs::{FileTimes, OpenOptions};
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use app_test_support::{McpProcess, to_response};
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionArchiveParams, SessionArchiveResponse, SessionStartParams,
    SessionStartResponse, SessionUnarchiveParams, SessionUnarchiveResponse,
};
use savfox_core::{find_archived_session_path_by_id_str, find_session_path_by_id_str};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::test]
async fn session_unarchive_moves_rollout_back_into_sessions_directory() -> Result<()> {
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_session_start_request(SessionStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let SessionStartResponse { session, .. } = to_response::<SessionStartResponse>(start_resp)?;

    assert!(
        find_session_path_by_id_str(savfox_home.path(), &session.id)
            .await?
            .is_some(),
        "expected rollout path for session id to exist"
    );

    let archive_id = mcp
        .send_session_archive_request(SessionArchiveParams {
            session_id: session.id.clone(),
        })
        .await?;
    let archive_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(archive_id)),
    )
    .await??;
    let _: SessionArchiveResponse = to_response::<SessionArchiveResponse>(archive_resp)?;

    let archived_path = find_archived_session_path_by_id_str(savfox_home.path(), &session.id)
        .await?
        .expect("expected archived rollout path for session id to exist");
    let archived_path_display = archived_path.display();
    assert!(
        archived_path.exists(),
        "expected {archived_path_display} to exist"
    );
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let old_timestamp = old_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("old timestamp")
        .as_secs() as i64;
    let times = FileTimes::new().set_modified(old_time);
    OpenOptions::new()
        .append(true)
        .open(&archived_path)?
        .set_times(times)?;

    let unarchive_id = mcp
        .send_session_unarchive_request(SessionUnarchiveParams {
            session_id: session.id.clone(),
        })
        .await?;
    let unarchive_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(unarchive_id)),
    )
    .await??;
    let SessionUnarchiveResponse {
        session: unarchived_session,
    } = to_response::<SessionUnarchiveResponse>(unarchive_resp)?;
    assert!(
        unarchived_session.updated_at > old_timestamp,
        "expected updated_at to be bumped on unarchive"
    );

    let restored_rollout_path = find_session_path_by_id_str(savfox_home.path(), &session.id)
        .await?
        .expect("expected restored rollout path for session id to exist");
    let rollout_path_display = restored_rollout_path.display();
    assert!(
        restored_rollout_path.exists(),
        "expected rollout path {rollout_path_display} to be restored"
    );
    let response_path = unarchived_session
        .path
        .as_ref()
        .expect("expected response session path after unarchive");
    assert!(
        response_path.exists(),
        "expected response session path to exist"
    );
    assert_eq!(
        response_path.file_name(),
        restored_rollout_path.file_name(),
        "expected response path and restored path to point to same rollout file"
    );
    assert!(
        !archived_path.exists(),
        "expected archived rollout path {archived_path_display} to be moved"
    );

    Ok(())
}

fn create_config_toml(savfox_home: &Path) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(config_toml, config_contents())
}

fn config_contents() -> &'static str {
    r#"model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"
"#
}
