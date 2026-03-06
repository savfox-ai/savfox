use std::collections::HashSet;
use std::path::Path;

use savfox_protocol::SessionId;
use savfox_protocol::protocol::{Op, TokenUsage};

use crate::channel::GatewayChannel;
use crate::session::{SessionStore, session_file_to_store_value};

pub(crate) fn validate_uuid_v7_session_id(raw: Option<&str>) -> Result<Option<String>, String> {
    let requested = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let Some(session_id) = requested else {
        return Ok(None);
    };

    let parsed = uuid::Uuid::parse_str(&session_id)
        .map_err(|err| format!("invalid session_id '{session_id}': {err}"))?;
    if parsed.get_version_num() != 7 {
        return Err(format!(
            "invalid session_id '{session_id}': UUID v7 required"
        ));
    }

    Ok(Some(session_id))
}

pub(crate) fn provider_from_model(model: &str) -> String {
    model.split('/').next().unwrap_or("unknown").to_string()
}

pub(crate) async fn persist_chat_session_metadata(
    session_store: &SessionStore,
    savfox_home: &Path,
    logical_session_id: &str,
    thread_id: &str,
    model: &str,
    provider: &str,
    rollout_path: Option<&Path>,
    token_usage: Option<&TokenUsage>,
) {
    let session_file = rollout_path.map(|path| session_file_to_store_value(savfox_home, path));
    let input_tokens = token_usage
        .map(|usage| usage.input_tokens.max(0) as u64)
        .unwrap_or(0);
    let output_tokens = token_usage
        .map(|usage| usage.output_tokens.max(0) as u64)
        .unwrap_or(0);
    let model = model.to_string();
    let provider = provider.to_string();
    let thread_id = thread_id.to_string();

    let updated = session_store
        .update(logical_session_id, {
            let session_file = session_file.clone();
            let model = model.clone();
            let provider = provider.clone();
            let thread_id = thread_id.clone();
            move |entry| {
                entry.model = Some(model.clone());
                entry.provider = Some(provider.clone());
                entry.thread_id = Some(thread_id.clone());
                if let Some(file) = session_file.as_ref() {
                    entry.session_file = Some(file.clone());
                }
                if input_tokens > 0 || output_tokens > 0 {
                    entry.add_tokens(input_tokens, output_tokens);
                }
            }
        })
        .await;

    if updated.is_some() {
        return;
    }

    if let Some(mut entry) = session_store.get_or_create(logical_session_id).await {
        entry.model = Some(model);
        entry.provider = Some(provider);
        entry.thread_id = Some(thread_id);
        if let Some(file) = session_file {
            entry.session_file = Some(file);
        }
        if input_tokens > 0 || output_tokens > 0 {
            entry.add_tokens(input_tokens, output_tokens);
        } else {
            entry.touch();
        }
        session_store.upsert(entry).await;
    }
}

pub(crate) async fn resolve_abort_candidate_ids(
    session_store: &SessionStore,
    thread_id: Option<&str>,
    session_id: Option<&str>,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(thread_id) = thread_id.map(str::trim).filter(|value| !value.is_empty()) {
        candidates.push(thread_id.to_string());
    }

    if let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) {
        candidates.push(session_id.to_string());
        if let Some(entry) = session_store.get(session_id).await
            && let Some(mapped_thread_id) = entry.thread_id
        {
            let mapped_thread_id = mapped_thread_id.trim();
            if !mapped_thread_id.is_empty() {
                candidates.push(mapped_thread_id.to_string());
            }
        }
    }

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

pub(crate) async fn abort_first_active_candidate(
    channel: &GatewayChannel,
    candidates: &[String],
) -> Option<String> {
    for candidate in candidates {
        if let Ok(thread_id) = SessionId::from_string(candidate)
            && let Ok(thread) = channel.session_manager().get_session(thread_id).await
        {
            let _ = thread.submit(Op::Interrupt).await;
            return Some(candidate.clone());
        }
    }

    None
}

pub(crate) async fn abort_all_active_threads(channel: &GatewayChannel) -> u32 {
    let thread_ids = channel.session_manager().list_session_ids().await;
    let mut aborted = 0u32;
    for thread_id in &thread_ids {
        if let Ok(thread) = channel.session_manager().get_session(*thread_id).await
            && thread.submit(Op::Interrupt).await.is_ok()
        {
            aborted += 1;
        }
    }
    aborted
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use savfox_protocol::protocol::TokenUsage;
    use tempfile::tempdir;

    use super::{
        persist_chat_session_metadata, resolve_abort_candidate_ids, validate_uuid_v7_session_id,
    };
    use crate::session::SessionStore;

    #[test]
    fn validate_uuid_v7_session_id_rejects_invalid_inputs() {
        assert_eq!(validate_uuid_v7_session_id(None).unwrap(), None);
        assert_eq!(validate_uuid_v7_session_id(Some("  ")).unwrap(), None);

        let invalid = validate_uuid_v7_session_id(Some("not-a-uuid"));
        assert!(invalid.is_err());

        let v4 = uuid::Uuid::new_v4().to_string();
        let wrong_version = validate_uuid_v7_session_id(Some(&v4));
        assert!(wrong_version.is_err());

        let v7 = uuid::Uuid::now_v7().to_string();
        assert_eq!(validate_uuid_v7_session_id(Some(&v7)).unwrap(), Some(v7));
    }

    #[tokio::test]
    async fn persist_chat_session_metadata_updates_shared_entry_fields() {
        let home = tempdir().expect("tempdir");
        let sessions_dir = home.path().join("sessions");
        tokio::fs::create_dir_all(&sessions_dir)
            .await
            .expect("create sessions dir");

        let session_id = uuid::Uuid::now_v7().to_string();
        let thread_id = uuid::Uuid::now_v7().to_string();
        let rollout_path: PathBuf = sessions_dir.join(format!("{session_id}.jsonl"));
        tokio::fs::write(&rollout_path, "")
            .await
            .expect("create rollout file");

        let store = SessionStore::from_home(home.path());
        let usage = TokenUsage {
            input_tokens: 11,
            cached_input_tokens: 0,
            output_tokens: 7,
            reasoning_output_tokens: 0,
            total_tokens: 18,
        };
        persist_chat_session_metadata(
            &store,
            home.path(),
            &session_id,
            &thread_id,
            "openai/gpt-4.1",
            "openai",
            Some(&rollout_path),
            Some(&usage),
        )
        .await;

        let entry = store
            .get(&session_id)
            .await
            .expect("session entry should exist");
        assert_eq!(entry.model.as_deref(), Some("openai/gpt-4.1"));
        assert_eq!(entry.provider.as_deref(), Some("openai"));
        assert_eq!(entry.thread_id.as_deref(), Some(thread_id.as_str()));
        assert_eq!(entry.session_file.as_deref(), Some(session_id.as_str()));
        assert_eq!(entry.input_tokens, 11);
        assert_eq!(entry.output_tokens, 7);
        assert_eq!(entry.total_tokens, 18);
    }

    #[tokio::test]
    async fn resolve_abort_candidates_includes_mapped_thread_id() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());

        let session_id = uuid::Uuid::now_v7().to_string();
        let mapped_thread_id = uuid::Uuid::now_v7().to_string();
        let explicit_thread_id = uuid::Uuid::now_v7().to_string();

        let mut entry = store
            .get_or_create(&session_id)
            .await
            .expect("session entry should be created");
        entry.thread_id = Some(mapped_thread_id.clone());
        store.upsert(entry).await;

        let candidates =
            resolve_abort_candidate_ids(&store, Some(&explicit_thread_id), Some(&session_id)).await;

        assert_eq!(
            candidates,
            vec![explicit_thread_id, session_id, mapped_thread_id]
        );
    }
}
