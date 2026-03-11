#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::io::Write;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use savfox_core::config::ConfigBuilder;
use savfox_core::protocol::SessionSource;
use savfox_core::{
    RolloutRecorder, RolloutRecorderParams, find_archived_session_path_by_id_str,
    find_session_path_by_id_str, find_session_path_by_name_str,
};
use savfox_protocol::SessionId;
use savfox_protocol::models::BaseInstructions;
use tempfile::TempDir;
use uuid::Uuid;

/// Create `<subdir>/` and write a minimal rollout file containing the
/// provided conversation id in the SessionMeta line. Returns the absolute path.
fn write_minimal_rollout_with_id_in_subdir(savfox_home: &Path, subdir: &str, id: Uuid) -> PathBuf {
    let sessions = savfox_home.join(subdir);
    std::fs::create_dir_all(&sessions).unwrap();

    let file = sessions.join(format!("{id}.jsonl"));
    let mut f = std::fs::File::create(&file).unwrap();
    // Minimal first line: session_meta with the id so content search can find it
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "timestamp": "2024-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": ".",
                "originator": "test",
                "cli_version": "test",
                "model_provider": "test-provider"
            }
        })
    )
    .unwrap();

    file
}

/// Create `sessions/` and write a minimal rollout file containing the
/// provided conversation id in the SessionMeta line. Returns the absolute path.
fn write_minimal_rollout_with_id(savfox_home: &Path, id: Uuid) -> PathBuf {
    write_minimal_rollout_with_id_in_subdir(savfox_home, "sessions", id)
}

#[tokio::test]
async fn find_locates_rollout_file_by_id() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(home.path(), id);

    let found = find_session_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found.unwrap(), expected);
}

#[tokio::test]
async fn find_handles_gitignore_covering_savfox_home_directory() {
    let repo = TempDir::new().unwrap();
    let savfox_home = repo.path().join(".savfox");
    std::fs::create_dir_all(&savfox_home).unwrap();
    std::fs::write(repo.path().join(".gitignore"), ".savfox/**\n").unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(&savfox_home, id);

    let found = find_session_path_by_id_str(&savfox_home, &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}

#[tokio::test]
async fn find_ignores_granular_gitignore_rules() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id(home.path(), id);
    std::fs::write(home.path().join("sessions/.gitignore"), "*.jsonl\n").unwrap();

    let found = find_session_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}

#[tokio::test]
async fn find_locates_rollout_file_written_by_recorder() -> std::io::Result<()> {
    // Ensures the name-based finder locates a rollout produced by the real recorder.
    let home = TempDir::new().unwrap();
    let config = ConfigBuilder::default()
        .savfox_home(home.path().to_path_buf())
        .build()
        .await?;
    let session_id = SessionId::new();
    let session_name = "named session";
    let recorder = RolloutRecorder::new(
        &config,
        RolloutRecorderParams::new(
            session_id,
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
        ),
        None,
        None,
    )
    .await?;
    recorder.flush().await?;

    let index_path = home.path().join("session_index.jsonl");
    std::fs::write(
        &index_path,
        format!(
            "{}\n",
            serde_json::json!({
                "id": session_id,
                "session_name": session_name,
                "updated_at": "2024-01-01T00:00:00Z"
            })
        ),
    )?;

    let found = find_session_path_by_name_str(home.path(), session_name).await?;

    let path = found.expect("expected rollout path to be found");
    assert!(path.exists());
    let contents = std::fs::read_to_string(&path)?;
    assert!(contents.contains(&session_id.to_string()));
    recorder.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn find_archived_locates_rollout_file_by_id() {
    let home = TempDir::new().unwrap();
    let id = Uuid::new_v4();
    let expected = write_minimal_rollout_with_id_in_subdir(home.path(), "archived_sessions", id);

    let found = find_archived_session_path_by_id_str(home.path(), &id.to_string())
        .await
        .unwrap();

    assert_eq!(found, Some(expected));
}
