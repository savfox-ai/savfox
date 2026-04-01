use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, to_response};
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SessionArchiveParams, SessionArchiveResponse, SessionStartParams,
    SessionStartResponse,
};
use savfox_core::{ARCHIVED_SESSIONS_SUBDIR, find_session_path_by_id_str};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn session_archive_moves_rollout_into_archived_directory() -> Result<()> {
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a session.
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
    assert!(!session.id.is_empty());

    // Locate the rollout path recorded for this session id.
    let rollout_path = find_session_path_by_id_str(savfox_home.path(), &session.id)
        .await?
        .expect("expected rollout path for session id to exist");
    assert!(
        rollout_path.exists(),
        "expected {} to exist",
        rollout_path.display()
    );

    // Archive the session.
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

    // Verify file moved.
    let archived_directory = savfox_home.path().join(ARCHIVED_SESSIONS_SUBDIR);
    // The archived file keeps the original filename (<id>.jsonl).
    let archived_rollout_path =
        archived_directory.join(rollout_path.file_name().expect("rollout file name"));
    assert!(
        !rollout_path.exists(),
        "expected rollout path {} to be moved",
        rollout_path.display()
    );
    assert!(
        archived_rollout_path.exists(),
        "expected archived rollout path {} to exist",
        archived_rollout_path.display()
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
