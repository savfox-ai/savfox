//! Persist Savfox session rollouts (.jsonl) so sessions can be replayed or inspected later.

use std::fs::{
    File, {self},
};
use std::io::Error as IoError;
use std::path::{Path, PathBuf};

use savfox_protocol::SessionId;
use savfox_protocol::dynamic_tools::DynamicToolSpec;
use savfox_protocol::models::{BaseInstructions, ContentItem};
use savfox_protocol::protocol::{
    InitialHistory, ResumedHistory, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine,
    SessionModel, SessionSource,
};
use savfox_state::SessionMetadataBuilder;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{
    Sender, {self},
};
use tokio::sync::oneshot;
use tracing::{info, warn};

use super::list::{
    Cursor, SessionListConfig, SessionListLayout, SessionSortKey, SessionsPage, get_sessions,
    get_sessions_in_root,
};
use super::policy::is_persisted_response_item;
use super::{ARCHIVED_SESSIONS_SUBDIR, SESSIONS_SUBDIR, metadata};
use crate::config::Config;
use crate::default_client::originator;
use crate::git_info::collect_git_info;
use crate::state_db::StateDbHandle;
use crate::{path_utils, state_db};

/// Records all [`ResponseItem`]s for a session and flushes them to disk after
/// every update.
///
/// Rollouts are recorded as JSONL and can be inspected with tools such as:
///
/// ```ignore
/// $ jq -C . ~/.savfox/sessions/01958648-1234-7000-8000-000000000001.jsonl
/// $ fx ~/.savfox/sessions/01958648-1234-7000-8000-000000000001.jsonl
/// ```
#[derive(Clone)]
pub struct RolloutRecorder {
    tx: Sender<RolloutCmd>,
    pub(crate) rollout_path: PathBuf,
    state_db: Option<StateDbHandle>,
}

#[derive(Clone)]
pub enum RolloutRecorderParams {
    Create {
        conversation_id: SessionId,
        forked_from_id: Option<SessionId>,
        source: SessionSource,
        base_instructions: BaseInstructions,
        dynamic_tools: Vec<DynamicToolSpec>,
    },
    Resume {
        path: PathBuf,
    },
}

enum RolloutCmd {
    AddItems(Vec<RolloutItem>),
    /// Ensure all prior writes are processed; respond when flushed.
    Flush {
        ack: oneshot::Sender<()>,
    },
    Shutdown {
        ack: oneshot::Sender<()>,
    },
}

impl RolloutRecorderParams {
    pub fn new(
        conversation_id: SessionId,
        forked_from_id: Option<SessionId>,
        source: SessionSource,
        base_instructions: BaseInstructions,
        dynamic_tools: Vec<DynamicToolSpec>,
    ) -> Self {
        Self::Create {
            conversation_id,
            forked_from_id,
            source,
            base_instructions,
            dynamic_tools,
        }
    }

    pub fn resume(path: PathBuf) -> Self {
        Self::Resume { path }
    }
}

/// Normalize rollout line shapes produced by older clients so they can be
/// deserialized as modern `RolloutLine` + `RolloutItem` values.
fn normalize_rollout_line_value(mut line: Value) -> Value {
    let Some(line_obj) = line.as_object_mut() else {
        return line;
    };

    if line_obj.get("type").and_then(Value::as_str) == Some("message") {
        line_obj.insert(
            "type".to_string(),
            Value::String("response_item".to_string()),
        );
    }

    if !line_obj.contains_key("payload")
        && let Some(message_payload) = line_obj.remove("message")
    {
        line_obj.insert("payload".to_string(), message_payload);
    }

    if line_obj.get("type").and_then(Value::as_str) == Some("response_item")
        && let Some(payload) = line_obj.get_mut("payload")
    {
        normalize_legacy_response_payload(payload);
    }

    line
}

fn normalize_legacy_response_payload(payload: &mut Value) {
    let Some(payload_obj) = payload.as_object_mut() else {
        return;
    };

    if matches!(
        payload_obj.get("type").and_then(Value::as_str),
        Some("user")
    ) || matches!(
        payload_obj.get("type").and_then(Value::as_str),
        Some("assistant")
    ) {
        let role = payload_obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        payload_obj.insert("type".to_string(), Value::String("message".to_string()));
        payload_obj
            .entry("role".to_string())
            .or_insert_with(|| Value::String(role.clone()));
        ensure_legacy_message_content(payload_obj, role.as_str());
        return;
    }

    if matches!(
        payload_obj.get("role").and_then(Value::as_str),
        Some("user" | "assistant")
    ) {
        if !payload_obj.contains_key("type") {
            payload_obj.insert("type".to_string(), Value::String("message".to_string()));
        }
        let role = payload_obj
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        ensure_legacy_message_content(payload_obj, role.as_str());
    }
}

fn ensure_legacy_message_content(payload_obj: &mut serde_json::Map<String, Value>, role: &str) {
    if let Some(content) = payload_obj.get_mut("content") {
        coerce_legacy_content(content, role);
        if serde_json::from_value::<Vec<ContentItem>>(content.clone()).is_ok() {
            return;
        }
    }

    let text = payload_obj
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload_obj.get("text").and_then(Value::as_str))
        .map(str::to_string);

    if let Some(text) = text
        && !text.is_empty()
    {
        let item_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        payload_obj.insert(
            "content".to_string(),
            Value::Array(vec![json!({ "type": item_type, "text": text })]),
        );
    }
}

fn coerce_legacy_content(content: &mut Value, role: &str) {
    let item_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    match content {
        Value::String(text) => {
            let text = text.clone();
            *content = Value::Array(vec![json!({ "type": item_type, "text": text })]);
        }
        Value::Array(items) => {
            let mut normalized = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    Value::String(text) => {
                        normalized.push(json!({ "type": item_type, "text": text }));
                    }
                    Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
                        Some("input_text" | "output_text" | "input_image") => {
                            normalized.push(Value::Object(obj.clone()))
                        }
                        Some("text") => {
                            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                                normalized.push(json!({ "type": item_type, "text": text }));
                            }
                        }
                        Some(_) => {}
                        None => {
                            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                                normalized.push(json!({ "type": item_type, "text": text }));
                            }
                        }
                    },
                    _ => {}
                }
            }
            if !normalized.is_empty() {
                *content = Value::Array(normalized);
            }
        }
        _ => {}
    }
}

impl RolloutRecorder {
    /// List sessions (rollout files) under the provided Savfox home directory.
    pub async fn list_sessions(
        savfox_home: &Path,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: SessionSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
    ) -> std::io::Result<SessionsPage> {
        let stage = "list_sessions";
        let page = get_sessions(
            savfox_home,
            page_size,
            cursor,
            sort_key,
            allowed_sources,
            model_providers,
            default_provider,
        )
        .await?;

        // TODO(jif): drop after sqlite migration phase 1
        let state_db_ctx = state_db::open_if_present(savfox_home, default_provider).await;
        if let Some(db_ids) = state_db::list_session_ids_db(
            state_db_ctx.as_deref(),
            savfox_home,
            page_size,
            cursor,
            sort_key,
            allowed_sources,
            model_providers,
            false,
            stage,
        )
        .await
        {
            if page.items.len() != db_ids.len() {
                state_db::record_discrepancy(stage, "bad_len");
                return Ok(page);
            }
            for (id, item) in db_ids.iter().zip(page.items.iter()) {
                if !item.path.display().to_string().contains(&id.to_string()) {
                    state_db::record_discrepancy(stage, "bad_id");
                }
            }
        }
        Ok(page)
    }

    /// List archived sessions (rollout files) under the archived sessions directory.
    pub async fn list_archived_sessions(
        savfox_home: &Path,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: SessionSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
    ) -> std::io::Result<SessionsPage> {
        let stage = "list_archived_sessions";
        let root = savfox_home.join(ARCHIVED_SESSIONS_SUBDIR);
        let page = get_sessions_in_root(
            root,
            page_size,
            cursor,
            sort_key,
            SessionListConfig {
                allowed_sources,
                model_providers,
                default_provider,
                layout: SessionListLayout::Flat,
            },
        )
        .await?;

        // TODO(jif): drop after sqlite migration phase 1
        let state_db_ctx = state_db::open_if_present(savfox_home, default_provider).await;
        if let Some(db_ids) = state_db::list_session_ids_db(
            state_db_ctx.as_deref(),
            savfox_home,
            page_size,
            cursor,
            sort_key,
            allowed_sources,
            model_providers,
            true,
            stage,
        )
        .await
        {
            if page.items.len() != db_ids.len() {
                state_db::record_discrepancy(stage, "bad_len");
                return Ok(page);
            }
            for (id, item) in db_ids.iter().zip(page.items.iter()) {
                if !item.path.display().to_string().contains(&id.to_string()) {
                    state_db::record_discrepancy(stage, "bad_id");
                }
            }
        }
        Ok(page)
    }

    /// Find the newest recorded session path, optionally filtering to a matching cwd.
    #[allow(clippy::too_many_arguments)]
    pub async fn find_latest_session_path(
        savfox_home: &Path,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: SessionSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
        filter_cwd: Option<&Path>,
    ) -> std::io::Result<Option<PathBuf>> {
        let mut cursor = cursor.cloned();
        loop {
            let page = Self::list_sessions(
                savfox_home,
                page_size,
                cursor.as_ref(),
                sort_key,
                allowed_sources,
                model_providers,
                default_provider,
            )
            .await?;
            if let Some(path) = select_resume_path(&page, filter_cwd) {
                return Ok(Some(path));
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(None);
            }
        }
    }

    /// Attempt to create a new [`RolloutRecorder`]. If the sessions directory
    /// cannot be created or the rollout file cannot be opened we return the
    /// error so the caller can decide whether to disable persistence.
    pub async fn new(
        config: &Config,
        params: RolloutRecorderParams,
        state_db_ctx: Option<StateDbHandle>,
        state_builder: Option<SessionMetadataBuilder>,
    ) -> std::io::Result<Self> {
        let (file, rollout_path, meta) = match params {
            RolloutRecorderParams::Create {
                conversation_id,
                forked_from_id,
                source,
                base_instructions,
                dynamic_tools,
            } => {
                let LogFileInfo {
                    file,
                    path,
                    conversation_id: session_id,
                    timestamp,
                } = create_log_file(config, conversation_id)?;

                let timestamp_format: &[FormatItem] = format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
                );
                let timestamp = timestamp
                    .to_offset(time::UtcOffset::UTC)
                    .format(timestamp_format)
                    .map_err(|e| IoError::other(format!("failed to format timestamp: {e}")))?;

                (
                    tokio::fs::File::from_std(file),
                    path,
                    Some(SessionMeta {
                        id: session_id,
                        forked_from_id,
                        timestamp,
                        cwd: config.cwd.clone(),
                        originator: originator().value,
                        cli_version: env!("CARGO_PKG_VERSION").to_string(),
                        source,
                        model: Some(SessionModel {
                            provider: config.model_provider_id.clone(),
                            model_code: config.model.clone().unwrap_or_default(),
                        }),
                        model_provider: None,
                        base_instructions: Some(base_instructions),
                        dynamic_tools: if dynamic_tools.is_empty() {
                            None
                        } else {
                            Some(dynamic_tools)
                        },
                    }),
                )
            }
            RolloutRecorderParams::Resume { path } => (
                tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .await?,
                path,
                None,
            ),
        };

        // Clone the cwd for the spawned task to collect git info asynchronously
        let cwd = config.cwd.clone();

        // A reasonably-sized bounded channel. If the buffer fills up the send
        // future will yield, which is fine – we only need to ensure we do not
        // perform *blocking* I/O on the caller's session.
        let (tx, rx) = mpsc::channel::<RolloutCmd>(256);

        // Spawn a Tokio task that owns the file handle and performs async
        // writes. Using `tokio::fs::File` keeps everything on the async I/O
        // driver instead of blocking the runtime.
        tokio::task::spawn(rollout_writer(
            file,
            rx,
            meta,
            cwd,
            rollout_path.clone(),
            state_db_ctx.clone(),
            state_builder,
            config.model_provider_id.clone(),
        ));

        Ok(Self {
            tx,
            rollout_path,
            state_db: state_db_ctx,
        })
    }

    pub fn rollout_path(&self) -> &Path {
        self.rollout_path.as_path()
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.state_db.clone()
    }

    pub(crate) async fn record_items(&self, items: &[RolloutItem]) -> std::io::Result<()> {
        let mut filtered = Vec::new();
        for item in items {
            // Note that function calls may look a bit strange if they are
            // "fully qualified MCP tool calls," so we could consider
            // reformatting them in that case.
            if is_persisted_response_item(item) {
                filtered.push(item.clone());
            }
        }
        if filtered.is_empty() {
            return Ok(());
        }
        self.tx
            .send(RolloutCmd::AddItems(filtered))
            .await
            .map_err(|e| IoError::other(format!("failed to queue rollout items: {e}")))
    }

    /// Flush all queued writes and wait until they are committed by the writer task.
    pub async fn flush(&self) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RolloutCmd::Flush { ack: tx })
            .await
            .map_err(|e| IoError::other(format!("failed to queue rollout flush: {e}")))?;
        rx.await
            .map_err(|e| IoError::other(format!("failed waiting for rollout flush: {e}")))
    }

    pub(crate) async fn load_rollout_items(
        path: &Path,
    ) -> std::io::Result<(Vec<RolloutItem>, Option<SessionId>, usize)> {
        info!("Resuming rollout from {path:?}");
        let text = tokio::fs::read_to_string(path).await?;
        if text.trim().is_empty() {
            return Err(IoError::other("empty session file"));
        }

        let mut items: Vec<RolloutItem> = Vec::new();
        let mut session_id: Option<SessionId> = None;
        let mut parse_errors = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let raw_line: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    warn!("failed to parse line as JSON: {line:?}, error: {e}");
                    parse_errors = parse_errors.saturating_add(1);
                    continue;
                }
            };
            let v = normalize_rollout_line_value(raw_line);

            // Parse the rollout line structure
            match serde_json::from_value::<RolloutLine>(v.clone()) {
                Ok(rollout_line) => match rollout_line.item {
                    RolloutItem::SessionMeta(session_meta_line) => {
                        // Use the FIRST SessionMeta encountered in the file as the canonical
                        // session id and main session information. Keep all items intact.
                        if session_id.is_none() {
                            session_id = Some(session_meta_line.meta.id);
                        }
                        items.push(RolloutItem::SessionMeta(session_meta_line));
                    }
                    RolloutItem::ResponseItem(item) => {
                        items.push(RolloutItem::ResponseItem(item));
                    }
                    RolloutItem::Compacted(item) => {
                        items.push(RolloutItem::Compacted(item));
                    }
                    RolloutItem::TurnContext(item) => {
                        items.push(RolloutItem::TurnContext(item));
                    }
                    RolloutItem::EventMsg(_ev) => {
                        items.push(RolloutItem::EventMsg(_ev));
                    }
                },
                Err(e) => {
                    warn!("failed to parse rollout line: {e}");
                    parse_errors = parse_errors.saturating_add(1);
                }
            }
        }

        tracing::debug!(
            "Resumed rollout with {} items, session ID: {:?}, parse errors: {}",
            items.len(),
            session_id,
            parse_errors,
        );
        Ok((items, session_id, parse_errors))
    }

    pub async fn get_rollout_history(path: &Path) -> std::io::Result<InitialHistory> {
        let (items, session_id, _parse_errors) = Self::load_rollout_items(path).await?;
        let conversation_id = session_id
            .ok_or_else(|| IoError::other("failed to parse session ID from rollout file"))?;

        if items.is_empty() {
            return Ok(InitialHistory::New);
        }

        info!("Resumed rollout successfully from {path:?}");
        Ok(InitialHistory::Resumed(ResumedHistory {
            conversation_id,
            history: items,
            rollout_path: path.to_path_buf(),
        }))
    }

    pub async fn shutdown(&self) -> std::io::Result<()> {
        let (tx_done, rx_done) = oneshot::channel();
        match self.tx.send(RolloutCmd::Shutdown { ack: tx_done }).await {
            Ok(_) => rx_done
                .await
                .map_err(|e| IoError::other(format!("failed waiting for rollout shutdown: {e}"))),
            Err(e) => {
                warn!("failed to send rollout shutdown command: {e}");
                Err(IoError::other(format!(
                    "failed to send rollout shutdown command: {e}"
                )))
            }
        }
    }
}

struct LogFileInfo {
    /// Opened file handle to the rollout file.
    file: File,

    /// Full path to the rollout file.
    path: PathBuf,

    /// Session ID (also embedded in filename).
    conversation_id: SessionId,

    /// Timestamp for the start of the session.
    timestamp: OffsetDateTime,
}

fn create_log_file(config: &Config, conversation_id: SessionId) -> std::io::Result<LogFileInfo> {
    // Resolve ~/.savfox/sessions and create it if missing.
    let timestamp = OffsetDateTime::now_local()
        .map_err(|e| IoError::other(format!("failed to get local time: {e}")))?;
    let mut dir = config.savfox_home.clone();
    dir.push(SESSIONS_SUBDIR);
    fs::create_dir_all(&dir)?;

    // Use UUID v7 as the filename (UUID v7 already embeds timestamp)
    let filename = format!("{conversation_id}.jsonl");

    let path = dir.join(filename);
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;

    Ok(LogFileInfo {
        file,
        path,
        conversation_id,
        timestamp,
    })
}

#[allow(clippy::too_many_arguments)]
async fn rollout_writer(
    file: tokio::fs::File,
    mut rx: mpsc::Receiver<RolloutCmd>,
    mut meta: Option<SessionMeta>,
    cwd: std::path::PathBuf,
    rollout_path: PathBuf,
    state_db_ctx: Option<StateDbHandle>,
    mut state_builder: Option<SessionMetadataBuilder>,
    default_provider: String,
) -> std::io::Result<()> {
    let mut writer = JsonlWriter { file };
    if let Some(builder) = state_builder.as_mut() {
        builder.rollout_path = rollout_path.clone();
    }

    // If we have a meta, collect git info asynchronously and write meta first
    if let Some(session_meta) = meta.take() {
        let git_info = collect_git_info(&cwd).await;
        let session_meta_line = SessionMetaLine {
            meta: session_meta,
            git: git_info,
        };
        if state_db_ctx.is_some() {
            state_builder =
                metadata::builder_from_session_meta(&session_meta_line, rollout_path.as_path());
        }

        // Write the SessionMeta as the first item in the file, wrapped in a rollout line
        let rollout_item = RolloutItem::SessionMeta(session_meta_line);
        writer.write_rollout_item(&rollout_item).await?;
        state_db::reconcile_rollout(
            state_db_ctx.as_deref(),
            rollout_path.as_path(),
            default_provider.as_str(),
            state_builder.as_ref(),
            std::slice::from_ref(&rollout_item),
        )
        .await;
    }

    // Process rollout commands
    while let Some(cmd) = rx.recv().await {
        match cmd {
            RolloutCmd::AddItems(items) => {
                let mut persisted_items = Vec::new();
                for item in items {
                    if is_persisted_response_item(&item) {
                        writer.write_rollout_item(&item).await?;
                        persisted_items.push(item);
                    }
                }
                if persisted_items.is_empty() {
                    continue;
                }
                if let Some(builder) = state_builder.as_mut() {
                    builder.rollout_path = rollout_path.clone();
                }
                state_db::apply_rollout_items(
                    state_db_ctx.as_deref(),
                    rollout_path.as_path(),
                    default_provider.as_str(),
                    state_builder.as_ref(),
                    persisted_items.as_slice(),
                    "rollout_writer",
                )
                .await;
            }
            RolloutCmd::Flush { ack } => {
                // Ensure underlying file is flushed and then ack.
                if let Err(e) = writer.file.flush().await {
                    let _ = ack.send(());
                    return Err(e);
                }
                let _ = ack.send(());
            }
            RolloutCmd::Shutdown { ack } => {
                let _ = ack.send(());
            }
        }
    }

    Ok(())
}

struct JsonlWriter {
    file: tokio::fs::File,
}

#[derive(serde::Serialize)]
struct RolloutLineRef<'a> {
    timestamp: String,
    #[serde(flatten)]
    item: &'a RolloutItem,
}

impl JsonlWriter {
    async fn write_rollout_item(&mut self, rollout_item: &RolloutItem) -> std::io::Result<()> {
        let timestamp_format: &[FormatItem] = format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        );
        let timestamp = OffsetDateTime::now_utc()
            .format(timestamp_format)
            .map_err(|e| IoError::other(format!("failed to format timestamp: {e}")))?;

        let line = RolloutLineRef {
            timestamp,
            item: rollout_item,
        };
        self.write_line(&line).await
    }
    async fn write_line(&mut self, item: &impl serde::Serialize) -> std::io::Result<()> {
        let mut json = serde_json::to_string(item)?;
        json.push('\n');
        self.file.write_all(json.as_bytes()).await?;
        self.file.flush().await?;
        Ok(())
    }
}

fn select_resume_path(page: &SessionsPage, filter_cwd: Option<&Path>) -> Option<PathBuf> {
    match filter_cwd {
        Some(cwd) => page.items.iter().find_map(|item| {
            if session_cwd_matches(&item.head, cwd) {
                Some(item.path.clone())
            } else {
                None
            }
        }),
        None => page.items.first().map(|item| item.path.clone()),
    }
}

fn session_cwd_matches(head: &[serde_json::Value], cwd: &Path) -> bool {
    let Some(session_cwd) = extract_session_cwd(head) else {
        return false;
    };
    if let (Ok(ca), Ok(cb)) = (
        path_utils::normalize_for_path_comparison(&session_cwd),
        path_utils::normalize_for_path_comparison(cwd),
    ) {
        return ca == cb;
    }
    session_cwd == cwd
}

fn extract_session_cwd(head: &[serde_json::Value]) -> Option<PathBuf> {
    head.iter().find_map(|value| {
        let meta_line = serde_json::from_value::<SessionMetaLine>(value.clone()).ok()?;
        Some(meta_line.meta.cwd)
    })
}

#[cfg(test)]
mod tests {
    use savfox_protocol::models::ResponseItem;
    use tempfile::tempdir;

    use super::*;

    fn text_from_content(content: &[ContentItem]) -> String {
        content
            .iter()
            .filter_map(|item| match item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    Some(text.as_str())
                }
                ContentItem::InputImage { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn session_meta_line(timestamp: &str, session_id: SessionId) -> RolloutLine {
        RolloutLine {
            timestamp: timestamp.to_string(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: session_id,
                    forked_from_id: None,
                    timestamp: timestamp.to_string(),
                    cwd: PathBuf::from("/tmp"),
                    originator: "test".to_string(),
                    cli_version: "0.0.0".to_string(),
                    source: SessionSource::Cli,
                    model: None,
                    model_provider: None,
                    base_instructions: None,
                    dynamic_tools: None,
                },
                git: None,
            }),
        }
    }

    #[tokio::test]
    async fn get_rollout_history_normalizes_legacy_response_item_user_and_assistant_types() {
        let dir = tempdir().expect("tempdir");
        let rollout_path = dir.path().join("legacy-response-item.jsonl");
        let timestamp = "2026-02-22T00:00:00.000Z";
        let session_id = SessionId::new();

        let lines = [
            serde_json::to_string(&session_meta_line(timestamp, session_id)).expect("meta line"),
            json!({
                "timestamp": "2026-02-22T00:00:01.000Z",
                "type": "response_item",
                "payload": {
                    "type": "user",
                    "text": "legacy user text"
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-02-22T00:00:02.000Z",
                "type": "response_item",
                "payload": {
                    "type": "assistant",
                    "message": "legacy assistant text"
                }
            })
            .to_string(),
        ];
        std::fs::write(&rollout_path, format!("{}\n", lines.join("\n"))).expect("write rollout");

        let history = RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .expect("load rollout");
        let InitialHistory::Resumed(resumed) = history else {
            panic!("expected resumed history");
        };

        let messages = resumed
            .history
            .iter()
            .filter_map(|item| match item {
                RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. }) => {
                    Some((role.clone(), text_from_content(content)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            messages,
            vec![
                ("user".to_string(), "legacy user text".to_string()),
                ("assistant".to_string(), "legacy assistant text".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn get_rollout_history_normalizes_legacy_message_envelope() {
        let dir = tempdir().expect("tempdir");
        let rollout_path = dir.path().join("legacy-message-envelope.jsonl");
        let timestamp = "2026-02-22T00:00:00.000Z";
        let session_id = SessionId::new();

        let lines = [
            serde_json::to_string(&session_meta_line(timestamp, session_id)).expect("meta line"),
            json!({
                "timestamp": "2026-02-22T00:00:01.000Z",
                "type": "message",
                "message": {
                    "role": "user",
                    "message": "legacy wrapped user"
                }
            })
            .to_string(),
        ];
        std::fs::write(&rollout_path, format!("{}\n", lines.join("\n"))).expect("write rollout");

        let history = RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .expect("load rollout");
        let InitialHistory::Resumed(resumed) = history else {
            panic!("expected resumed history");
        };

        let messages = resumed
            .history
            .iter()
            .filter_map(|item| match item {
                RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. }) => {
                    Some((role.clone(), text_from_content(content)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            messages,
            vec![("user".to_string(), "legacy wrapped user".to_string())]
        );
    }
}
