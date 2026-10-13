use std::path::Path;
use std::sync::{Arc, OnceLock};

use tracing::{info, warn};

use crate::compaction::{CompactionConfig, CompactionService};
use crate::log_store;
use crate::session::SessionStore;

fn compaction_service() -> &'static CompactionService {
    static SERVICE: OnceLock<CompactionService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let threshold = std::env::var("SAVFOX_MEMORY_FLUSH_SOFT_THRESHOLD_TOKENS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(50_000);
        let prompt = std::env::var("SAVFOX_MEMORY_FLUSH_PROMPT")
            .ok()
            .and_then(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                }
            });
        CompactionService::new(CompactionConfig {
            memory_flush_enabled: true,
            memory_flush_soft_threshold_tokens: threshold,
            memory_flush_prompt: prompt,
            ..CompactionConfig::default()
        })
    })
}

fn sanitize_session_id_for_path(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_owned()
    } else {
        out
    }
}

fn memory_flush_markdown(session_id: &str, flush: &serde_json::Value) -> String {
    let metadata = flush
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let removed_count = metadata
        .get("removed_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let pre_tokens = metadata
        .get("pre_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let post_tokens = metadata
        .get("post_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let prompt = metadata
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let summary = flush
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    format!(
        "# Compaction Memory Flush\n\n\
         - session_id: `{session_id}`\n\
         - removed_count: {removed_count}\n\
         - pre_tokens: {pre_tokens}\n\
         - post_tokens: {post_tokens}\n\
         - prompt: {prompt}\n\n\
         ## Summary\n\n\
         {summary}\n"
    )
}

async fn persist_memory_flush_record(
    savfox_home: &Path,
    session_id: &str,
    flush: &serde_json::Value,
) -> Result<(u64, u64), std::io::Error> {
    let dir = savfox_home.join("sessions").join("compaction_flush");
    tokio::fs::create_dir_all(&dir).await?;

    let file_name = format!(
        "{}-{}.md",
        sanitize_session_id_for_path(session_id),
        crate::json_store::now_ms()
    );
    let file_path = dir.join(file_name);
    let markdown = memory_flush_markdown(session_id, flush);
    tokio::fs::write(&file_path, markdown.as_bytes()).await?;

    let metadata = flush
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let pre = metadata
        .get("pre_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let post = metadata
        .get("post_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let bytes = markdown.len() as u64;
    Ok((bytes, pre.saturating_sub(post)))
}

pub(super) async fn maybe_auto_memory_flush(
    session_store: &Arc<SessionStore>,
    savfox_home: &Path,
    session_id: &str,
    previous_total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    prompt: &str,
    reply: &str,
) {
    if input_tokens == 0 && output_tokens == 0 {
        return;
    }

    let service = compaction_service();
    let cfg = service.config();
    if !cfg.memory_flush_enabled {
        return;
    }

    let threshold = cfg.memory_flush_soft_threshold_tokens.max(1);
    let updated_total = previous_total_tokens
        .saturating_add(input_tokens)
        .saturating_add(output_tokens);

    if updated_total < threshold {
        return;
    }

    // Trigger once per threshold bucket to avoid flushing every single message.
    if previous_total_tokens / threshold == updated_total / threshold {
        return;
    }

    let messages = vec![
        serde_json::json!({
            "role": "user",
            "content": prompt,
        }),
        serde_json::json!({
            "role": "assistant",
            "content": reply,
        }),
    ];
    let compacted = match service.compact(session_id, &messages, 1) {
        Ok(compacted) => compacted,
        Err(err) => {
            warn!(
                session_id = %session_id,
                "memory flush skipped because semantic compaction is unavailable: {err}"
            );
            log_store::append_log(
                "warn",
                "channel/runtime",
                format!(
                    "memory flush skipped: session_id={session_id}, semantic compaction unavailable: {err}"
                ),
            )
            .await;
            return;
        }
    };
    let Some(flush_entry) = service.generate_memory_flush(session_id, &compacted) else {
        return;
    };

    match persist_memory_flush_record(savfox_home, session_id, &flush_entry).await {
        Ok((bytes, tokens_saved)) => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.compaction_count = entry.compaction_count.saturating_add(1);
                    entry.memory_flush_count = entry.memory_flush_count.saturating_add(1);
                    entry.memory_flush_bytes = entry.memory_flush_bytes.saturating_add(bytes);
                    entry.memory_flush_tokens_saved =
                        entry.memory_flush_tokens_saved.saturating_add(tokens_saved);
                    entry.touch();
                })
                .await;
            info!(
                session_id,
                bytes, tokens_saved, "auto memory flush persisted for compaction"
            );
            log_store::append_log(
                "info",
                "channel/runtime",
                format!(
                    "memory flush persisted: session_id={session_id}, bytes={bytes}, tokens_saved={tokens_saved}"
                ),
            )
            .await;
        }
        Err(err) => {
            warn!(
                session_id = %session_id,
                "failed to persist memory flush record: {err}"
            );
            log_store::append_log(
                "warn",
                "channel/runtime",
                format!("memory flush persist failed: session_id={session_id}, err={err}"),
            )
            .await;
        }
    }
}
