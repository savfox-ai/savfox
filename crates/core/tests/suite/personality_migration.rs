use std::io;
use std::path::Path;

use pretty_assertions::assert_eq;
use savfox_core::config::ConfigToml;
use savfox_core::personality_migration::{
    PERSONALITY_MIGRATION_FILENAME, PersonalityMigrationStatus, maybe_migrate_personality,
};
use savfox_core::{ARCHIVED_SESSIONS_SUBDIR, SESSIONS_SUBDIR};
use savfox_protocol::SessionId;
use savfox_protocol::config_types::Personality;
use savfox_protocol::protocol::{
    EventMsg, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine, SessionSource,
    UserMessageEvent,
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;

const TEST_TIMESTAMP: &str = "2025-01-01T00-00-00";

async fn read_config_toml(savfox_home: &Path) -> io::Result<ConfigToml> {
    let contents = tokio::fs::read_to_string(savfox_home.join("config.toml")).await?;
    toml::from_str(&contents).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

async fn write_session_with_user_event(savfox_home: &Path) -> io::Result<()> {
    let session_id = SessionId::new();
    let dir = savfox_home.join(SESSIONS_SUBDIR);
    write_rollout_with_user_event(&dir, session_id).await
}

async fn write_archived_session_with_user_event(savfox_home: &Path) -> io::Result<()> {
    let session_id = SessionId::new();
    let dir = savfox_home.join(ARCHIVED_SESSIONS_SUBDIR);
    write_rollout_with_user_event(&dir, session_id).await
}

async fn write_rollout_with_user_event(dir: &Path, session_id: SessionId) -> io::Result<()> {
    tokio::fs::create_dir_all(&dir).await?;
    let file_path = dir.join(format!("{session_id}.jsonl"));
    let mut file = tokio::fs::File::create(&file_path).await?;

    let session_meta = SessionMetaLine {
        meta: SessionMeta {
            id: session_id,
            forked_from_id: None,
            timestamp: TEST_TIMESTAMP.to_string(),
            cwd: std::path::PathBuf::from("."),
            originator: "test_originator".to_string(),
            cli_version: "test_version".to_string(),
            source: SessionSource::Cli,
            model: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
        },
        git: None,
    };
    let meta_line = RolloutLine {
        timestamp: TEST_TIMESTAMP.to_string(),
        item: RolloutItem::SessionMeta(session_meta),
    };
    let user_event = RolloutLine {
        timestamp: TEST_TIMESTAMP.to_string(),
        item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "hello".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
    };

    let meta_json = serde_json::to_string(&meta_line)?;
    file.write_all(format!("{meta_json}\n").as_bytes()).await?;
    let user_json = serde_json::to_string(&user_event)?;
    file.write_all(format!("{user_json}\n").as_bytes()).await?;
    Ok(())
}

#[tokio::test]
async fn migration_marker_exists_no_sessions_no_change() -> io::Result<()> {
    let temp = TempDir::new()?;
    let marker_path = temp.path().join(PERSONALITY_MIGRATION_FILENAME);
    tokio::fs::write(&marker_path, "v1\n").await?;

    let status = maybe_migrate_personality(temp.path(), &ConfigToml::default()).await?;

    assert_eq!(status, PersonalityMigrationStatus::SkippedMarker);
    assert_eq!(
        tokio::fs::try_exists(temp.path().join("config.toml")).await?,
        false
    );
    Ok(())
}

#[tokio::test]
async fn no_marker_no_sessions_no_change() -> io::Result<()> {
    let temp = TempDir::new()?;

    let status = maybe_migrate_personality(temp.path(), &ConfigToml::default()).await?;

    assert_eq!(status, PersonalityMigrationStatus::SkippedNoSessions);
    assert_eq!(
        tokio::fs::try_exists(temp.path().join(PERSONALITY_MIGRATION_FILENAME)).await?,
        true
    );
    assert_eq!(
        tokio::fs::try_exists(temp.path().join("config.toml")).await?,
        false
    );
    Ok(())
}

#[tokio::test]
async fn no_marker_sessions_sets_personality() -> io::Result<()> {
    let temp = TempDir::new()?;
    write_session_with_user_event(temp.path()).await?;

    let status = maybe_migrate_personality(temp.path(), &ConfigToml::default()).await?;

    assert_eq!(status, PersonalityMigrationStatus::Applied);
    assert_eq!(
        tokio::fs::try_exists(temp.path().join(PERSONALITY_MIGRATION_FILENAME)).await?,
        true
    );

    let persisted = read_config_toml(temp.path()).await?;
    assert_eq!(persisted.personality, Some(Personality::Pragmatic));
    Ok(())
}

#[tokio::test]
async fn no_marker_archived_sessions_sets_personality() -> io::Result<()> {
    let temp = TempDir::new()?;
    write_archived_session_with_user_event(temp.path()).await?;

    let status = maybe_migrate_personality(temp.path(), &ConfigToml::default()).await?;

    assert_eq!(status, PersonalityMigrationStatus::Applied);
    assert_eq!(
        tokio::fs::try_exists(temp.path().join(PERSONALITY_MIGRATION_FILENAME)).await?,
        true
    );

    let persisted = read_config_toml(temp.path()).await?;
    assert_eq!(persisted.personality, Some(Personality::Pragmatic));
    Ok(())
}
