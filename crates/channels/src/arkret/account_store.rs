use std::path::{Path, PathBuf};

use arkret::{AccountId, ActorId, DeviceId, DidCoreId};
use garth::{CursorScope, FileStore};

use crate::arkret::account_scope_id;

pub fn open_account_store(
    savfox_home: &Path,
    channel_id: &str,
    account_id: &str,
    event_cache_capacity: usize,
) -> arkret::Result<FileStore> {
    FileStore::open_with_seen_capacity(
        account_store_path(savfox_home, channel_id, account_id),
        event_cache_capacity,
    )
    .map_err(|error| arkret::Error::Protocol(error.to_string()))
}

pub(super) fn account_store_path(
    savfox_home: &Path,
    channel_id: &str,
    account_id: &str,
) -> PathBuf {
    let scope_id = account_scope_id(channel_id, account_id);
    savfox_home
        .join("gateway")
        .join("arkret-account-state")
        .join(format!("{}.json", safe_file_stem(&scope_id)))
}

/// Delete the durable account subscribe/inbox state file for one (channel,
/// account) pair. Used by unbind so an Agent's cursors, dedupe set and pending
/// deliveries do not survive into the next binding. Removing a missing file is
/// a no-op, not an error.
pub fn delete_account_store(
    savfox_home: &Path,
    channel_id: &str,
    account_id: &str,
) -> std::io::Result<()> {
    match std::fs::remove_file(account_store_path(savfox_home, channel_id, account_id)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub fn device_messages_scope(
    service_id: Option<&str>,
    account_id: &AccountId,
    device_id: &str,
) -> arkret::Result<CursorScope> {
    Ok(CursorScope::DeviceMessages {
        service_id: service_id
            .map(|value| {
                DidCoreId::new(value.to_owned()).map_err(|error| {
                    arkret::Error::Protocol(format!("invalid service DID '{value}': {error}"))
                })
            })
            .transpose()?,
        actor_id: ActorId::account(account_id.clone()),
        device_id: DeviceId::new(device_id.to_owned()).map_err(|error| {
            arkret::Error::Protocol(format!("invalid Arkret device id '{device_id}': {error}"))
        })?,
    })
}

pub(super) fn safe_file_stem(scope_id: &str) -> String {
    let value: String = scope_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if value.is_empty() {
        "default".to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use garth::{ClientEvent, CursorStore, DurableInboxStore};

    use super::*;

    #[test]
    fn account_store_path_is_stable_and_safe() {
        let path = account_store_path(Path::new("/tmp/savfox"), "channel:one", "account/two");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("account_channel_one_account_two.json")
        );
    }

    #[tokio::test]
    async fn device_message_cursor_isolated_by_complete_account_identity() {
        let home = std::env::temp_dir().join(format!(
            "savfox-garth-account-isolation-{}",
            arkret::new_prefixed_uuid7("scope-")
        ));
        let store = open_account_store(&home, "channel", "account", 16).unwrap();
        let principal = DidCoreId::new("ak:did_core:web:alice.example").unwrap();
        let first = AccountId::new(
            principal.clone(),
            DidCoreId::new("ak:did_core:web:first.example").unwrap(),
        );
        let second = AccountId::new(
            principal,
            DidCoreId::new("ak:did_core:web:second.example").unwrap(),
        );
        let device = "ak:device:01904100-0000-7000-8000-000000000001";
        let first_scope = device_messages_scope(None, &first, device).unwrap();
        let second_scope = device_messages_scope(None, &second, device).unwrap();
        assert_ne!(first_scope, second_scope);
        store
            .save(first_scope, "ak:cursor:first".to_owned())
            .await
            .unwrap();
        assert_eq!(store.load(second_scope).await.unwrap(), None);
    }

    #[tokio::test]
    async fn pending_delivery_survives_account_store_reopen() {
        let home = std::env::temp_dir().join(format!("savfox-garth-inbox-{}", std::process::id()));
        let path = account_store_path(&home, "channel", "account");
        let _ = std::fs::remove_file(&path);
        let store = open_account_store(&home, "channel", "account", 16).unwrap();
        let scope = device_messages_scope(
            None,
            &AccountId::new(
                DidCoreId::new("ak:did_core:webvh:z6mkfixturealice").unwrap(),
                DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            ),
            "ak:device:01904100-0000-7000-8000-000000000001",
        )
        .unwrap();
        let delivery_id = store
            .commit(
                scope,
                Some("ak:cursor:restart".to_owned()),
                vec![ClientEvent::AccountUpdates(Default::default())],
            )
            .await
            .unwrap();
        drop(store);

        let reopened = open_account_store(&home, "channel", "account", 16).unwrap();
        let pending = reopened.pending(8).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, delivery_id);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(home);
    }
}
