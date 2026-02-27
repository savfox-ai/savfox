use std::fs;
use std::fs::FileTimes;
use std::path::{Path, PathBuf};

use anyhow::Result;
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{
    GitInfo, SessionMeta, SessionMetaLine, SessionModel, SessionSource,
};
use serde_json::json;
use uuid::Uuid;

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
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = SessionId::from_string(&uuid_str)?;

    let file_path = rollout_path(savfox_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(dir)?;

    // Build JSONL lines
    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: PathBuf::from("/"),
        originator: "savfox".to_string(),
        cli_version: "0.0.0".to_string(),
        source,
        model: model_provider.map(|provider| SessionModel {
            provider: provider.to_string(),
            model_code: String::new(),
        }),
        model_provider: None,
        base_instructions: None,
        dynamic_tools: None,
    };
    let payload = serde_json::to_value(SessionMetaLine {
        meta,
        git: git_info,
    })?;

    let lines = [
        json!({
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"message",
            "payload": {
                "role":"assistant",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"message",
            "payload": {
                "role":"user",
                "message": preview,
            }
        })
        .to_string(),
    ];

    fs::write(&file_path, lines.join("\n") + "\n")?;
    let parsed = chrono::DateTime::parse_from_rfc3339(meta_rfc3339)?.with_timezone(&chrono::Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file_path)?
        .set_times(times)?;
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
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = SessionId::from_string(&uuid_str)?;

    let file_path = rollout_path(savfox_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(&dir)?;

    // Build JSONL lines
    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: PathBuf::from("/"),
        originator: "savfox".to_string(),
        cli_version: "0.0.0".to_string(),
        source: SessionSource::Cli,
        model: model_provider.map(|provider| SessionModel {
            provider: provider.to_string(),
            model_code: String::new(),
        }),
        model_provider: None,
        base_instructions: None,
        dynamic_tools: None,
    };
    let payload = serde_json::to_value(SessionMetaLine {
        meta,
        git: git_info,
    })?;

    let lines = [
        json!( {
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!( {
            "timestamp": meta_rfc3339,
            "type":"message",
            "message": {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!( {
            "timestamp": meta_rfc3339,
            "type":"message",
            "message": {
                "role":"user",
                "message": preview,
                "text_elements": text_elements,
                "local_images": []
            }
        })
        .to_string(),
    ];

    fs::write(file_path, lines.join("\n") + "\n")?;
    Ok(uuid_str)
}
