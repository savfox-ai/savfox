use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use savfox_protocol::SessionId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const SESSION_INDEX_FILE: &str = "session_index.jsonl";
const READ_CHUNK_SIZE: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionIndexEntry {
    pub id: SessionId,
    pub session_name: String,
    pub updated_at: String,
}

/// Append a session name update to the session index.
/// The index is append-only; the most recent entry wins when resolving names or ids.
pub async fn append_session_name(
    savfox_home: &Path,
    session_id: SessionId,
    name: &str,
) -> std::io::Result<()> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let entry = SessionIndexEntry {
        id: session_id,
        session_name: name.to_string(),
        updated_at,
    };
    append_session_index_entry(savfox_home, &entry).await
}

/// Append a raw session index entry to `session_index.jsonl`.
/// The file is append-only; consumers scan from the end to find the newest match.
pub async fn append_session_index_entry(
    savfox_home: &Path,
    entry: &SessionIndexEntry,
) -> std::io::Result<()> {
    let path = session_index_path(savfox_home);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

/// Find the latest session name for a session id, if any.
pub async fn find_session_name_by_id(
    savfox_home: &Path,
    session_id: &SessionId,
) -> std::io::Result<Option<String>> {
    let path = session_index_path(savfox_home);
    if !path.exists() {
        return Ok(None);
    }
    let id = *session_id;
    let entry = tokio::task::spawn_blocking(move || scan_index_from_end_by_id(&path, &id))
        .await
        .map_err(std::io::Error::other)??;
    Ok(entry.map(|entry| entry.session_name))
}

/// Find the latest session names for a batch of session ids.
pub async fn find_session_names_by_ids(
    savfox_home: &Path,
    session_ids: &HashSet<SessionId>,
) -> std::io::Result<HashMap<SessionId, String>> {
    let path = session_index_path(savfox_home);
    if session_ids.is_empty() || !path.exists() {
        return Ok(HashMap::new());
    }

    let file = tokio::fs::File::open(&path).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut names = HashMap::with_capacity(session_ids.len());

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(trimmed) else {
            continue;
        };
        let name = entry.session_name.trim();
        if !name.is_empty() && session_ids.contains(&entry.id) {
            names.insert(entry.id, name.to_string());
        }
    }

    Ok(names)
}

/// Find the most recently updated session id for a session name, if any.
pub async fn find_session_id_by_name(
    savfox_home: &Path,
    name: &str,
) -> std::io::Result<Option<SessionId>> {
    if name.trim().is_empty() {
        return Ok(None);
    }
    let path = session_index_path(savfox_home);
    if !path.exists() {
        return Ok(None);
    }
    let name = name.to_string();
    let entry = tokio::task::spawn_blocking(move || scan_index_from_end_by_name(&path, &name))
        .await
        .map_err(std::io::Error::other)??;
    Ok(entry.map(|entry| entry.id))
}

/// Locate a recorded session rollout file by session name using newest-first ordering.
/// Returns `Ok(Some(path))` if found, `Ok(None)` if not present.
pub async fn find_session_path_by_name_str(
    savfox_home: &Path,
    name: &str,
) -> std::io::Result<Option<PathBuf>> {
    let Some(session_id) = find_session_id_by_name(savfox_home, name).await? else {
        return Ok(None);
    };
    super::list::find_session_path_by_id_str(savfox_home, &session_id.to_string()).await
}

fn session_index_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join(SESSION_INDEX_FILE)
}

fn scan_index_from_end_by_id(
    path: &Path,
    session_id: &SessionId,
) -> std::io::Result<Option<SessionIndexEntry>> {
    scan_index_from_end(path, |entry| entry.id == *session_id)
}

fn scan_index_from_end_by_name(
    path: &Path,
    name: &str,
) -> std::io::Result<Option<SessionIndexEntry>> {
    scan_index_from_end(path, |entry| entry.session_name == name)
}

fn scan_index_from_end<F>(
    path: &Path,
    mut predicate: F,
) -> std::io::Result<Option<SessionIndexEntry>>
where
    F: FnMut(&SessionIndexEntry) -> bool,
{
    let mut file = File::open(path)?;
    let mut remaining = file.metadata()?.len();
    let mut line_rev: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while remaining > 0 {
        let read_size = usize::try_from(remaining.min(READ_CHUNK_SIZE as u64))
            .map_err(std::io::Error::other)?;
        remaining -= read_size as u64;
        file.seek(SeekFrom::Start(remaining))?;
        file.read_exact(&mut buf[..read_size])?;

        for &byte in buf[..read_size].iter().rev() {
            if byte == b'\n' {
                if let Some(entry) = parse_line_from_rev(&mut line_rev, &mut predicate)? {
                    return Ok(Some(entry));
                }
                continue;
            }
            line_rev.push(byte);
        }
    }

    if let Some(entry) = parse_line_from_rev(&mut line_rev, &mut predicate)? {
        return Ok(Some(entry));
    }

    Ok(None)
}

fn parse_line_from_rev<F>(
    line_rev: &mut Vec<u8>,
    predicate: &mut F,
) -> std::io::Result<Option<SessionIndexEntry>>
where
    F: FnMut(&SessionIndexEntry) -> bool,
{
    if line_rev.is_empty() {
        return Ok(None);
    }
    line_rev.reverse();
    let line = std::mem::take(line_rev);
    let Ok(mut line) = String::from_utf8(line) else {
        return Ok(None);
    };
    if line.ends_with('\r') {
        line.pop();
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(trimmed) else {
        return Ok(None);
    };
    if predicate(&entry) {
        return Ok(Some(entry));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;
    fn write_index(path: &Path, lines: &[SessionIndexEntry]) -> std::io::Result<()> {
        let mut out = String::new();
        for entry in lines {
            out.push_str(&serde_json::to_string(entry).unwrap());
            out.push('\n');
        }
        std::fs::write(path, out)
    }

    #[test]
    fn find_session_id_by_name_prefers_latest_entry() -> std::io::Result<()> {
        let temp = TempDir::new()?;
        let path = session_index_path(temp.path());
        let id1 = SessionId::new();
        let id2 = SessionId::new();
        let lines = vec![
            SessionIndexEntry {
                id: id1,
                session_name: "same".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionIndexEntry {
                id: id2,
                session_name: "same".to_string(),
                updated_at: "2024-01-02T00:00:00Z".to_string(),
            },
        ];
        write_index(&path, &lines)?;

        let found = scan_index_from_end_by_name(&path, "same")?;
        assert_eq!(found.map(|entry| entry.id), Some(id2));
        Ok(())
    }

    #[test]
    fn find_session_name_by_id_prefers_latest_entry() -> std::io::Result<()> {
        let temp = TempDir::new()?;
        let path = session_index_path(temp.path());
        let id = SessionId::new();
        let lines = vec![
            SessionIndexEntry {
                id,
                session_name: "first".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionIndexEntry {
                id,
                session_name: "second".to_string(),
                updated_at: "2024-01-02T00:00:00Z".to_string(),
            },
        ];
        write_index(&path, &lines)?;

        let found = scan_index_from_end_by_id(&path, &id)?;
        assert_eq!(
            found.map(|entry| entry.session_name),
            Some("second".to_string())
        );
        Ok(())
    }

    #[test]
    fn scan_index_returns_none_when_entry_missing() -> std::io::Result<()> {
        let temp = TempDir::new()?;
        let path = session_index_path(temp.path());
        let id = SessionId::new();
        let lines = vec![SessionIndexEntry {
            id,
            session_name: "present".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        }];
        write_index(&path, &lines)?;

        let missing_name = scan_index_from_end_by_name(&path, "missing")?;
        assert_eq!(missing_name, None);

        let missing_id = scan_index_from_end_by_id(&path, &SessionId::new())?;
        assert_eq!(missing_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn find_session_names_by_ids_prefers_latest_entry() -> std::io::Result<()> {
        let temp = TempDir::new()?;
        let path = session_index_path(temp.path());
        let id1 = SessionId::new();
        let id2 = SessionId::new();
        let lines = vec![
            SessionIndexEntry {
                id: id1,
                session_name: "first".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionIndexEntry {
                id: id2,
                session_name: "other".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
            },
            SessionIndexEntry {
                id: id1,
                session_name: "latest".to_string(),
                updated_at: "2024-01-02T00:00:00Z".to_string(),
            },
        ];
        write_index(&path, &lines)?;

        let mut ids = HashSet::new();
        ids.insert(id1);
        ids.insert(id2);

        let mut expected = HashMap::new();
        expected.insert(id1, "latest".to_string());
        expected.insert(id2, "other".to_string());

        let found = find_session_names_by_ids(temp.path(), &ids).await?;
        assert_eq!(found, expected);
        Ok(())
    }

    #[test]
    fn scan_index_finds_latest_match_among_mixed_entries() -> std::io::Result<()> {
        let temp = TempDir::new()?;
        let path = session_index_path(temp.path());
        let id_target = SessionId::new();
        let id_other = SessionId::new();
        let expected = SessionIndexEntry {
            id: id_target,
            session_name: "target".to_string(),
            updated_at: "2024-01-03T00:00:00Z".to_string(),
        };
        let expected_other = SessionIndexEntry {
            id: id_other,
            session_name: "target".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };
        // Resolution is based on append order (scan from end), not updated_at.
        let lines = vec![
            SessionIndexEntry {
                id: id_target,
                session_name: "target".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
            },
            expected_other.clone(),
            expected.clone(),
            SessionIndexEntry {
                id: SessionId::new(),
                session_name: "another".to_string(),
                updated_at: "2024-01-04T00:00:00Z".to_string(),
            },
        ];
        write_index(&path, &lines)?;

        let found_by_name = scan_index_from_end_by_name(&path, "target")?;
        assert_eq!(found_by_name, Some(expected.clone()));

        let found_by_id = scan_index_from_end_by_id(&path, &id_target)?;
        assert_eq!(found_by_id, Some(expected));

        let found_other_by_id = scan_index_from_end_by_id(&path, &id_other)?;
        assert_eq!(found_other_by_id, Some(expected_other));
        Ok(())
    }
}
