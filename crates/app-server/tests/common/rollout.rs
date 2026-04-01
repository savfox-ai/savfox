use std::fs;
use std::fs::FileTimes;
use std::path::{Path, PathBuf};

use anyhow::Result;
use savfox_protocol::SessionId;
use savfox_protocol::models::{ContentItem, ResponseItem};
use savfox_protocol::protocol::{
    EventMsg, GitInfo, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine, SessionModel,
    SessionSource, UserMessageEvent,
};
use savfox_protocol::user_input::TextElement;
use uuid::{NoContext, Timestamp, Uuid};

pub fn rollout_path(savfox_home: &Path, filename_ts: &str, session_id: &str) -> PathBuf {
    let _ = filename_ts;
    savfox_home
        .join("sessions")
        .join(format!("{session_id}.jsonl"))
}

/// Create a minimal rollout file under `SAVFOX_HOME/sessions/`.
///
/// - `filename_ts` is retained for compatibility with existing call sites.
/// - `meta_rfc3339` is the envelope timestamp used in JSON lines.
/// - `preview` is the user message preview text.
/// - `model_provider` optionally sets `payload.model.provider` in session meta.
///
/// Returns the generated conversation/session UUID as a string.
pub fn create_fake_rollout(
    savfox_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
) -> Result<String> {
    create_fake_rollout_with_source(
        savfox_home,
        filename_ts,
        meta_rfc3339,
        preview,
        model_provider,
        git_info,
        SessionSource::Cli,
    )
}

/// Create a minimal rollout file with an explicit session source.
pub fn create_fake_rollout_with_source(
    savfox_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
    source: SessionSource,
) -> Result<String> {
    let uuid = uuid_from_filename_ts(filename_ts)?;
    let uuid_str = uuid.to_string();
    let conversation_id = SessionId::from_string(&uuid_str)?;

    let file_path = rollout_path(savfox_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(dir)?;

    let meta_line = SessionMetaLine {
        meta: SessionMeta {
            id: conversation_id,
            forked_from_id: None,
            timestamp: meta_rfc3339.to_string(),
            cwd: PathBuf::from("/"),
            originator: "savfox".to_string(),
            cli_version: "0.0.0".to_string(),
            source,
            model: model_provider.map(|provider| SessionModel {
                provider: provider.to_string(),
                model_slug: String::new(),
            }),
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
        },
        git: git_info,
    };

    let lines = build_rollout_lines(meta_rfc3339, meta_line, preview, Vec::new())?;
    fs::write(&file_path, lines.join("\n") + "\n")?;
    set_rollout_mtime(&file_path, meta_rfc3339)?;
    Ok(uuid_str)
}

pub fn create_fake_rollout_with_text_elements(
    savfox_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    text_elements: Vec<serde_json::Value>,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
) -> Result<String> {
    let uuid = uuid_from_filename_ts(filename_ts)?;
    let uuid_str = uuid.to_string();
    let conversation_id = SessionId::from_string(&uuid_str)?;

    let file_path = rollout_path(savfox_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(&dir)?;

    let parsed_text_elements: Vec<TextElement> = text_elements
        .into_iter()
        .map(serde_json::from_value::<TextElement>)
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let meta_line = SessionMetaLine {
        meta: SessionMeta {
            id: conversation_id,
            forked_from_id: None,
            timestamp: meta_rfc3339.to_string(),
            cwd: PathBuf::from("/"),
            originator: "savfox".to_string(),
            cli_version: "0.0.0".to_string(),
            source: SessionSource::Cli,
            model: model_provider.map(|provider| SessionModel {
                provider: provider.to_string(),
                model_slug: String::new(),
            }),
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
        },
        git: git_info,
    };

    let lines = build_rollout_lines(meta_rfc3339, meta_line, preview, parsed_text_elements)?;
    fs::write(&file_path, lines.join("\n") + "\n")?;
    set_rollout_mtime(&file_path, meta_rfc3339)?;
    Ok(uuid_str)
}

fn build_rollout_lines(
    meta_rfc3339: &str,
    meta_line: SessionMetaLine,
    preview: &str,
    text_elements: Vec<TextElement>,
) -> Result<Vec<String>> {
    let lines = vec![
        RolloutLine {
            timestamp: meta_rfc3339.to_string(),
            item: RolloutItem::SessionMeta(meta_line),
        },
        RolloutLine {
            timestamp: meta_rfc3339.to_string(),
            item: RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: preview.to_string(),
                }],
                end_turn: None,
                phase: None,
            }),
        },
        RolloutLine {
            timestamp: meta_rfc3339.to_string(),
            item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: preview.to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements,
            })),
        },
    ];

    lines
        .into_iter()
        .map(|line| Ok(serde_json::to_string(&line)?))
        .collect()
}

fn set_rollout_mtime(file_path: &Path, meta_rfc3339: &str) -> Result<()> {
    let parsed = chrono::DateTime::parse_from_rfc3339(meta_rfc3339)?.with_timezone(&chrono::Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(file_path)?
        .set_times(times)?;
    Ok(())
}

fn uuid_from_filename_ts(filename_ts: &str) -> Result<Uuid> {
    let naive = chrono::NaiveDateTime::parse_from_str(filename_ts, "%Y-%m-%dT%H-%M-%S")?;
    let dt = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc);
    let seconds = dt.timestamp().max(0) as u64;
    let nanos = dt.timestamp_subsec_nanos();
    let ts = Timestamp::from_unix(NoContext, seconds, nanos);
    Ok(Uuid::new_v7(ts))
}
