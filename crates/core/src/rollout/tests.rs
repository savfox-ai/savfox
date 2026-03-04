#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::fs::{
    File, FileTimes, {self},
};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use pretty_assertions::assert_eq;
use savfox_protocol::SessionId;
use savfox_protocol::models::{ContentItem, ResponseItem};
use savfox_protocol::protocol::{
    EventMsg, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine, SessionSource,
    UserMessageEvent,
};
use tempfile::TempDir;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime};
use uuid::Uuid;

use crate::rollout::list::{Cursor, SessionItem, SessionSortKey, SessionsPage, get_sessions};
use crate::rollout::{INTERACTIVE_SESSION_SOURCES, rollout_date_parts};

const NO_SOURCE_FILTER: &[SessionSource] = &[];
const TEST_PROVIDER: &str = "test-provider";

fn provider_vec(providers: &[&str]) -> Vec<String> {
    providers
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

#[test]
fn rollout_date_parts_extracts_directory_components() {
    let file_name = OsStr::new("00000000-0000-0000-0000-000000000000.jsonl");
    let parts = rollout_date_parts(file_name);
    assert_eq!(
        parts,
        Some(("1970".to_string(), "01".to_string(), "01".to_string()))
    );
}

fn write_session_file(
    root: &Path,
    ts_str: &str,
    uuid: Uuid,
    num_records: usize,
    source: Option<SessionSource>,
) -> std::io::Result<(OffsetDateTime, Uuid)> {
    write_session_file_with_provider(
        root,
        ts_str,
        uuid,
        num_records,
        source,
        Some("test-provider"),
    )
}

fn write_session_file_with_provider(
    root: &Path,
    ts_str: &str,
    uuid: Uuid,
    num_records: usize,
    source: Option<SessionSource>,
    model_provider: Option<&str>,
) -> std::io::Result<(OffsetDateTime, Uuid)> {
    let format: &[FormatItem] =
        format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]");
    let dt = PrimitiveDateTime::parse(ts_str, format)
        .unwrap()
        .assume_utc();
    let dir = root.join("sessions");
    fs::create_dir_all(&dir)?;

    let filename = format!("{uuid}.jsonl");
    let file_path = dir.join(filename);
    let mut file = File::create(file_path)?;

    let mut payload = serde_json::json!({
        "id": uuid,
        "timestamp": ts_str,
        "cwd": ".",
        "originator": "test_originator",
        "cli_version": "test_version",
        "base_instructions": null,
    });

    if let Some(source) = source {
        payload["source"] = serde_json::to_value(source).unwrap();
    }
    if let Some(provider) = model_provider {
        payload["model_provider"] = serde_json::Value::String(provider.to_string());
    }

    let meta = serde_json::json!({
        "timestamp": ts_str,
        "type": "session_meta",
        "payload": payload,
    });
    writeln!(file, "{meta}")?;

    // Include at least one user message event to satisfy listing filters
    let user_event = serde_json::json!({
        "timestamp": ts_str,
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": "Hello from user",
            "kind": "plain"
        }
    });
    writeln!(file, "{user_event}")?;

    for i in 0..num_records {
        let rec = serde_json::json!({
            "record_type": "response",
            "index": i
        });
        writeln!(file, "{rec}")?;
    }
    let times = FileTimes::new().set_modified(dt.into());
    file.set_times(times)?;
    Ok((dt, uuid))
}

fn write_session_file_with_delayed_user_event(
    root: &Path,
    ts_str: &str,
    uuid: Uuid,
    meta_lines_before_user: usize,
) -> std::io::Result<()> {
    let format: &[FormatItem] =
        format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]");
    let dt = PrimitiveDateTime::parse(ts_str, format)
        .unwrap()
        .assume_utc();
    let dir = root.join("sessions");
    fs::create_dir_all(&dir)?;

    let filename = format!("{uuid}.jsonl");
    let file_path = dir.join(filename);
    let mut file = File::create(file_path)?;

    for i in 0..meta_lines_before_user {
        let id = if i == 0 {
            uuid
        } else {
            Uuid::from_u128(100 + i as u128)
        };
        let payload = serde_json::json!({
            "id": id,
            "timestamp": ts_str,
            "cwd": ".",
            "originator": "test_originator",
            "cli_version": "test_version",
            "source": "vscode",
            "model_provider": "test-provider",
        });
        let meta = serde_json::json!({
            "timestamp": ts_str,
            "type": "session_meta",
            "payload": payload,
        });
        writeln!(file, "{meta}")?;
    }

    let user_event = serde_json::json!({
        "timestamp": ts_str,
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Hello from user", "kind": "plain"}
    });
    writeln!(file, "{user_event}")?;

    let times = FileTimes::new().set_modified(dt.into());
    file.set_times(times)?;
    Ok(())
}

fn write_session_file_with_meta_payload(
    root: &Path,
    ts_str: &str,
    uuid: Uuid,
    payload: serde_json::Value,
) -> std::io::Result<()> {
    let format: &[FormatItem] =
        format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]");
    let dt = PrimitiveDateTime::parse(ts_str, format)
        .unwrap()
        .assume_utc();
    let dir = root.join("sessions");
    fs::create_dir_all(&dir)?;

    let filename = format!("{uuid}.jsonl");
    let file_path = dir.join(filename);
    let mut file = File::create(file_path)?;

    let meta = serde_json::json!({
        "timestamp": ts_str,
        "type": "session_meta",
        "payload": payload,
    });
    writeln!(file, "{meta}")?;

    let user_event = serde_json::json!({
        "timestamp": ts_str,
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Hello from user", "kind": "plain"}
    });
    writeln!(file, "{user_event}")?;

    let times = FileTimes::new().set_modified(dt.into());
    file.set_times(times)?;

    Ok(())
}

#[tokio::test]
async fn test_list_conversations_latest_first() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    // Fixed UUIDs for deterministic expectations
    let u1 = Uuid::from_u128(1);
    let u2 = Uuid::from_u128(2);
    let u3 = Uuid::from_u128(3);

    // Create three sessions across three days
    write_session_file(
        home,
        "2025-01-01T12-00-00",
        u1,
        3,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-01-02T12-00-00",
        u2,
        3,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-01-03T12-00-00",
        u3,
        3,
        Some(SessionSource::VSCode),
    )
    .unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(page.next_cursor.is_none());

    let ids: Vec<String> = page
        .items
        .iter()
        .filter_map(|item| {
            item.head
                .first()
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(ids, vec![u3.to_string(), u2.to_string(), u1.to_string()]);
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.created_at.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("2025-01-03T12-00-00"),
            Some("2025-01-02T12-00-00"),
            Some("2025-01-01T12-00-00"),
        ]
    );
}

#[tokio::test]
async fn test_pagination_cursor() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    // Fixed UUIDs for deterministic expectations
    let u1 = Uuid::from_u128(11);
    let u2 = Uuid::from_u128(22);
    let u3 = Uuid::from_u128(33);
    let u4 = Uuid::from_u128(44);
    let u5 = Uuid::from_u128(55);

    // Oldest to newest
    write_session_file(
        home,
        "2025-03-01T09-00-00",
        u1,
        1,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-03-02T09-00-00",
        u2,
        1,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-03-03T09-00-00",
        u3,
        1,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-03-04T09-00-00",
        u4,
        1,
        Some(SessionSource::VSCode),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-03-05T09-00-00",
        u5,
        1,
        Some(SessionSource::VSCode),
    )
    .unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page1 = get_sessions(
        home,
        2,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page1.items.len(), 2);
    assert!(page1.next_cursor.is_some());
    let page1_ids: Vec<String> = page1
        .items
        .iter()
        .filter_map(|item| {
            item.head
                .first()
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(page1_ids, vec![u5.to_string(), u4.to_string()]);

    let page2 = get_sessions(
        home,
        2,
        page1.next_cursor.as_ref(),
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_cursor.is_some());
    let page2_ids: Vec<String> = page2
        .items
        .iter()
        .filter_map(|item| {
            item.head
                .first()
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(page2_ids, vec![u3.to_string(), u2.to_string()]);

    let page3 = get_sessions(
        home,
        2,
        page2.next_cursor.as_ref(),
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page3.items.len(), 1);
    assert!(page3.next_cursor.is_none());
    let page3_id = page3
        .items
        .first()
        .and_then(|item| item.head.first())
        .and_then(|value| value.get("id"))
        .and_then(serde_json::Value::as_str);
    let u1_id = u1.to_string();
    assert_eq!(page3_id, Some(u1_id.as_str()));
}

#[tokio::test]
async fn test_list_sessions_scans_past_head_for_user_event() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let uuid = Uuid::from_u128(99);
    let ts = "2025-05-01T10-30-00";
    write_session_file_with_delayed_user_event(home, ts, uuid, 12).unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();

    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn test_get_session_contents() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let uuid = Uuid::from_u128(404);
    let ts = "2025-04-01T10-30-00";
    write_session_file(home, ts, uuid, 2, Some(SessionSource::VSCode)).unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        1,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    let path = &page.items[0].path;

    let content = tokio::fs::read_to_string(path).await.unwrap();

    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_none());
    assert!(page.items[0].path.ends_with(format!("{uuid}.jsonl")));
    assert_eq!(page.items[0].created_at.as_deref(), Some(ts));

    // Entire file contents equality
    let meta = serde_json::json!({
        "timestamp": ts,
        "type": "session_meta",
        "payload": {
            "id": uuid,
            "timestamp": ts,
            "cwd": ".",
            "originator": "test_originator",
            "cli_version": "test_version",
            "base_instructions": null,
            "source": "vscode",
            "model_provider": "test-provider",
        }
    });
    let user_event = serde_json::json!({
        "timestamp": ts,
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Hello from user", "kind": "plain"}
    });
    let rec0 = serde_json::json!({"record_type": "response", "index": 0});
    let rec1 = serde_json::json!({"record_type": "response", "index": 1});
    let expected_content = format!("{meta}\n{user_event}\n{rec0}\n{rec1}\n");
    assert_eq!(content, expected_content);
}

#[tokio::test]
async fn test_base_instructions_missing_in_meta_defaults_to_null() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let ts = "2025-04-02T10-30-00";
    let uuid = Uuid::from_u128(101);
    let payload = serde_json::json!({
        "id": uuid,
        "timestamp": ts,
        "cwd": ".",
        "originator": "test_originator",
        "cli_version": "test_version",
        "source": "vscode",
        "model_provider": "test-provider",
    });
    write_session_file_with_meta_payload(home, ts, uuid, payload).unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        1,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();

    let head = page
        .items
        .first()
        .and_then(|item| item.head.first())
        .expect("session meta head");
    assert_eq!(
        head.get("base_instructions"),
        Some(&serde_json::Value::Null)
    );
}

#[tokio::test]
async fn test_base_instructions_present_in_meta_is_preserved() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let ts = "2025-04-03T10-30-00";
    let uuid = Uuid::from_u128(102);
    let base_text = "Custom base instructions";
    let payload = serde_json::json!({
        "id": uuid,
        "timestamp": ts,
        "cwd": ".",
        "originator": "test_originator",
        "cli_version": "test_version",
        "source": "vscode",
        "model_provider": "test-provider",
        "base_instructions": {"text": base_text},
    });
    write_session_file_with_meta_payload(home, ts, uuid, payload).unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        1,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();

    let head = page
        .items
        .first()
        .and_then(|item| item.head.first())
        .expect("session meta head");
    let base = head
        .get("base_instructions")
        .and_then(|value| value.get("text"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(base, Some(base_text));
}

#[tokio::test]
async fn test_created_at_sort_uses_file_mtime_for_updated_at() -> Result<()> {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let ts = "2025-06-01T08-00-00";
    let uuid = Uuid::from_u128(43);
    write_session_file(home, ts, uuid, 0, Some(SessionSource::VSCode)).unwrap();

    let created = PrimitiveDateTime::parse(
        ts,
        format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]"),
    )?
    .assume_utc();
    let updated = created + Duration::hours(2);
    let expected_updated = updated.format(&time::format_description::well_known::Rfc3339)?;

    let file_path = home.join("sessions").join(format!("{uuid}.jsonl"));
    let file = std::fs::OpenOptions::new().write(true).open(&file_path)?;
    let times = FileTimes::new().set_modified(updated.into());
    file.set_times(times)?;

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        1,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await?;

    let item = page.items.first().expect("conversation item");
    assert_eq!(item.created_at.as_deref(), Some(ts));
    assert_eq!(item.updated_at.as_deref(), Some(expected_updated.as_str()));

    Ok(())
}

#[tokio::test]
async fn test_updated_at_uses_file_mtime() -> Result<()> {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let ts = "2025-06-01T08-00-00";
    let uuid = Uuid::from_u128(42);
    let day_dir = home.join("sessions");
    fs::create_dir_all(&day_dir)?;
    let file_path = day_dir.join(format!("{uuid}.jsonl"));
    let mut file = File::create(&file_path)?;

    let conversation_id = SessionId::from_string(&uuid.to_string())?;
    let meta_line = RolloutLine {
        timestamp: ts.to_string(),
        item: RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id: conversation_id,
                forked_from_id: None,
                timestamp: ts.to_string(),
                cwd: ".".into(),
                originator: "test_originator".into(),
                cli_version: "test_version".into(),
                source: SessionSource::VSCode,
                model: None,
                model_provider: Some("test-provider".into()),
                base_instructions: None,
                dynamic_tools: None,
            },
            git: None,
        }),
    };
    writeln!(file, "{}", serde_json::to_string(&meta_line)?)?;

    let user_event_line = RolloutLine {
        timestamp: ts.to_string(),
        item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
        })),
    };
    writeln!(file, "{}", serde_json::to_string(&user_event_line)?)?;

    let total_messages = 12usize;
    for idx in 0..total_messages {
        let response_line = RolloutLine {
            timestamp: format!("{ts}-{idx:02}"),
            item: RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: format!("reply-{idx}"),
                }],
                end_turn: None,
                phase: None,
            }),
        };
        writeln!(file, "{}", serde_json::to_string(&response_line)?)?;
    }
    drop(file);

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page = get_sessions(
        home,
        1,
        None,
        SessionSortKey::UpdatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await?;
    let item = page.items.first().expect("conversation item");
    assert_eq!(item.created_at.as_deref(), Some(ts));
    let updated = item
        .updated_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .expect("updated_at set from file mtime");
    let now = chrono::Utc::now();
    let age = now - updated;
    assert!(age.num_seconds().abs() < 30);

    Ok(())
}

#[tokio::test]
async fn test_stable_ordering_same_second_pagination() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let ts = "2025-07-01T00-00-00";
    let u1 = Uuid::from_u128(1);
    let u2 = Uuid::from_u128(2);
    let u3 = Uuid::from_u128(3);

    write_session_file(home, ts, u1, 0, Some(SessionSource::VSCode)).unwrap();
    write_session_file(home, ts, u2, 0, Some(SessionSource::VSCode)).unwrap();
    write_session_file(home, ts, u3, 0, Some(SessionSource::VSCode)).unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let page1 = get_sessions(
        home,
        2,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page1.items.len(), 2);
    assert!(page1.next_cursor.is_some());
    let page1_ids: Vec<String> = page1
        .items
        .iter()
        .filter_map(|item| {
            item.head
                .first()
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(page1_ids, vec![u3.to_string(), u2.to_string()]);

    let page2 = get_sessions(
        home,
        2,
        page1.next_cursor.as_ref(),
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    assert_eq!(page2.items.len(), 1);
    assert!(page2.next_cursor.is_none());
    let page2_id = page2
        .items
        .first()
        .and_then(|item| item.head.first())
        .and_then(|value| value.get("id"))
        .and_then(serde_json::Value::as_str);
    let u1_id = u1.to_string();
    assert_eq!(page2_id, Some(u1_id.as_str()));
}

#[tokio::test]
async fn test_source_filter_excludes_non_matching_sessions() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let interactive_id = Uuid::from_u128(42);
    let non_interactive_id = Uuid::from_u128(77);

    write_session_file(
        home,
        "2025-08-02T10-00-00",
        interactive_id,
        2,
        Some(SessionSource::Cli),
    )
    .unwrap();
    write_session_file(
        home,
        "2025-08-01T10-00-00",
        non_interactive_id,
        2,
        Some(SessionSource::Exec),
    )
    .unwrap();

    let provider_filter = provider_vec(&[TEST_PROVIDER]);
    let interactive_only = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        INTERACTIVE_SESSION_SOURCES,
        Some(provider_filter.as_slice()),
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    let paths: Vec<_> = interactive_only
        .items
        .iter()
        .map(|item| item.path.as_path())
        .collect();

    assert_eq!(paths.len(), 1);
    assert!(
        paths
            .iter()
            .all(|path| path.ends_with("00000000-0000-0000-0000-00000000002a.jsonl"))
    );

    let all_sessions = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        NO_SOURCE_FILTER,
        None,
        TEST_PROVIDER,
    )
    .await
    .unwrap();
    let all_paths: Vec<_> = all_sessions
        .items
        .into_iter()
        .map(|item| item.path)
        .collect();
    assert_eq!(all_paths.len(), 2);
    assert!(
        all_paths
            .iter()
            .any(|path| path.ends_with("00000000-0000-0000-0000-00000000002a.jsonl"))
    );
    assert!(
        all_paths
            .iter()
            .any(|path| path.ends_with("00000000-0000-0000-0000-00000000004d.jsonl"))
    );
}

#[tokio::test]
async fn test_model_provider_filter_selects_only_matching_sessions() -> Result<()> {
    let temp = TempDir::new().unwrap();
    let home = temp.path();

    let openai_id = Uuid::from_u128(1);
    let beta_id = Uuid::from_u128(2);
    let none_id = Uuid::from_u128(3);

    write_session_file_with_provider(
        home,
        "2025-09-01T12-00-00",
        openai_id,
        1,
        Some(SessionSource::VSCode),
        Some("openai"),
    )?;
    write_session_file_with_provider(
        home,
        "2025-09-01T11-00-00",
        beta_id,
        1,
        Some(SessionSource::VSCode),
        Some("beta"),
    )?;
    write_session_file_with_provider(
        home,
        "2025-09-01T10-00-00",
        none_id,
        1,
        Some(SessionSource::VSCode),
        None,
    )?;

    let openai_id_str = openai_id.to_string();
    let none_id_str = none_id.to_string();
    let openai_filter = provider_vec(&["openai"]);
    let openai_sessions = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        NO_SOURCE_FILTER,
        Some(openai_filter.as_slice()),
        "openai",
    )
    .await?;
    assert_eq!(openai_sessions.items.len(), 2);
    let openai_ids: Vec<_> = openai_sessions
        .items
        .iter()
        .filter_map(|item| {
            item.head
                .first()
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(openai_ids.contains(&openai_id_str));
    assert!(openai_ids.contains(&none_id_str));

    let beta_filter = provider_vec(&["beta"]);
    let beta_sessions = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        NO_SOURCE_FILTER,
        Some(beta_filter.as_slice()),
        "openai",
    )
    .await?;
    assert_eq!(beta_sessions.items.len(), 1);
    let beta_id_str = beta_id.to_string();
    let beta_head = beta_sessions
        .items
        .first()
        .and_then(|item| item.head.first())
        .and_then(|value| value.get("id"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(beta_head, Some(beta_id_str.as_str()));

    let unknown_filter = provider_vec(&["unknown"]);
    let unknown_sessions = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        NO_SOURCE_FILTER,
        Some(unknown_filter.as_slice()),
        "openai",
    )
    .await?;
    assert!(unknown_sessions.items.is_empty());

    let all_sessions = get_sessions(
        home,
        10,
        None,
        SessionSortKey::CreatedAt,
        NO_SOURCE_FILTER,
        None,
        "openai",
    )
    .await?;
    assert_eq!(all_sessions.items.len(), 3);

    Ok(())
}
