use std::io;
use std::path::Path;

use savfox_protocol::config_types::Personality;
use savfox_protocol::protocol::SessionSource;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::config::ConfigToml;
use crate::config::edit::ConfigEditsBuilder;
use crate::rollout::list::{
    SessionListConfig, SessionListLayout, SessionSortKey, get_sessions_in_root,
};
use crate::rollout::{ARCHIVED_SESSIONS_SUBDIR, SESSIONS_SUBDIR};
use crate::state_db;

pub const PERSONALITY_MIGRATION_FILENAME: &str = ".personality_migration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalityMigrationStatus {
    SkippedMarker,
    SkippedExplicitPersonality,
    SkippedNoSessions,
    Applied,
}

pub async fn maybe_migrate_personality(
    savfox_home: &Path,
    config_toml: &ConfigToml,
) -> io::Result<PersonalityMigrationStatus> {
    let marker_path = savfox_home.join(PERSONALITY_MIGRATION_FILENAME);
    if tokio::fs::try_exists(&marker_path).await? {
        return Ok(PersonalityMigrationStatus::SkippedMarker);
    }

    if config_toml.personality.is_some() {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedExplicitPersonality);
    }

    let model_provider_id = config_toml
        .model_provider
        .clone()
        .unwrap_or_else(|| "openai".to_string());

    if !has_recorded_sessions(savfox_home, model_provider_id.as_str()).await? {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedNoSessions);
    }

    ConfigEditsBuilder::new(savfox_home)
        .set_personality(Some(Personality::Pragmatic))
        .apply()
        .await
        .map_err(|err| {
            io::Error::other(format!("failed to persist personality migration: {err}"))
        })?;

    create_marker(&marker_path).await?;
    Ok(PersonalityMigrationStatus::Applied)
}

async fn has_recorded_sessions(savfox_home: &Path, default_provider: &str) -> io::Result<bool> {
    let allowed_sources: &[SessionSource] = &[];

    if let Some(state_db_ctx) = state_db::open_if_present(savfox_home, default_provider).await
        && let Some(ids) = state_db::list_session_ids_db(
            Some(state_db_ctx.as_ref()),
            savfox_home,
            1,
            None,
            SessionSortKey::CreatedAt,
            allowed_sources,
            None,
            false,
            "personality_migration",
        )
        .await
        && !ids.is_empty()
    {
        return Ok(true);
    }

    let sessions = get_sessions_in_root(
        savfox_home.join(SESSIONS_SUBDIR),
        1,
        None,
        SessionSortKey::CreatedAt,
        SessionListConfig {
            allowed_sources,
            model_providers: None,
            default_provider,
            layout: SessionListLayout::Flat,
        },
    )
    .await?;
    if !sessions.items.is_empty() {
        return Ok(true);
    }

    let archived_sessions = get_sessions_in_root(
        savfox_home.join(ARCHIVED_SESSIONS_SUBDIR),
        1,
        None,
        SessionSortKey::CreatedAt,
        SessionListConfig {
            allowed_sources,
            model_providers: None,
            default_provider,
            layout: SessionListLayout::Flat,
        },
    )
    .await?;
    Ok(!archived_sessions.items.is_empty())
}

async fn create_marker(marker_path: &Path) -> io::Result<()> {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker_path)
        .await
    {
        Ok(mut file) => file.write_all(b"v1\n").await,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use savfox_protocol::SessionId;
    use savfox_protocol::protocol::{
        EventMsg, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine, SessionSource,
        UserMessageEvent,
    };
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    use super::*;

    const TEST_TIMESTAMP: &str = "2025-01-01T00-00-00";

    async fn read_config_toml(savfox_home: &Path) -> io::Result<ConfigToml> {
        let contents = tokio::fs::read_to_string(savfox_home.join("config.toml")).await?;
        toml::from_str(&contents).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    async fn write_session_with_user_event(savfox_home: &Path) -> io::Result<()> {
        let session_id = SessionId::new();
        let dir = savfox_home.join(SESSIONS_SUBDIR);
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

        file.write_all(format!("{}\n", serde_json::to_string(&meta_line)?).as_bytes())
            .await?;
        file.write_all(format!("{}\n", serde_json::to_string(&user_event)?).as_bytes())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn applies_when_sessions_exist_and_no_personality() -> io::Result<()> {
        let temp = TempDir::new()?;
        write_session_with_user_event(temp.path()).await?;

        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::Applied);
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());

        let persisted = read_config_toml(temp.path()).await?;
        assert_eq!(persisted.personality, Some(Personality::Pragmatic));
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_marker_exists() -> io::Result<()> {
        let temp = TempDir::new()?;
        create_marker(&temp.path().join(PERSONALITY_MIGRATION_FILENAME)).await?;

        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::SkippedMarker);
        assert!(!temp.path().join("config.toml").exists());
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_personality_explicit() -> io::Result<()> {
        let temp = TempDir::new()?;
        ConfigEditsBuilder::new(temp.path())
            .set_personality(Some(Personality::Friendly))
            .apply()
            .await
            .map_err(|err| io::Error::other(format!("failed to write config: {err}")))?;

        let config_toml = read_config_toml(temp.path()).await?;
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(
            status,
            PersonalityMigrationStatus::SkippedExplicitPersonality
        );
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());

        let persisted = read_config_toml(temp.path()).await?;
        assert_eq!(persisted.personality, Some(Personality::Friendly));
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_no_sessions() -> io::Result<()> {
        let temp = TempDir::new()?;
        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::SkippedNoSessions);
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());
        assert!(!temp.path().join("config.toml").exists());
        Ok(())
    }
}
