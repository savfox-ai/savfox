//! Persistent session behavior through the public production store API.
#![allow(clippy::unwrap_used)]

use pretty_assertions::assert_eq;
use savfox_gateway_server::session::{SessionEntry, SessionStore};

#[tokio::test]
async fn session_crud_persists_updates_and_deletion_across_reopen() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionStore::from_home(home.path());
    let entry = SessionEntry::new();
    let id = entry.session_id.clone();
    store.upsert(entry).await;
    assert!(store.set_label(&id, "Regression session".to_owned()).await);
    assert_eq!(store.list().await.len(), 1);
    drop(store);

    let reopened = SessionStore::from_home(home.path());
    assert_eq!(
        reopened.get(&id).await.unwrap().label.as_deref(),
        Some("Regression session")
    );
    assert!(reopened.remove(&id).await);
    drop(reopened);
    assert!(
        SessionStore::from_home(home.path())
            .get(&id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn store_rejects_non_session_identifiers_without_creating_metadata() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionStore::from_home(home.path());
    assert!(store.get_or_create("../../other").await.is_none());
    assert!(!store.set_label("../../other", "invalid".to_owned()).await);
    assert!(!store.remove("../../other").await);
    assert!(store.list().await.is_empty());
}

#[test]
fn session_migration_separates_core_and_remote_thread_identifiers() {
    let core_id = uuid::Uuid::now_v7().to_string();
    let mut core = SessionEntry {
        thread_id: Some(core_id.clone()),
        ..SessionEntry::default()
    };
    core.migrate_legacy_thread_namespaces();
    assert_eq!(core.core_thread_id.as_deref(), Some(core_id.as_str()));
    assert!(core.remote_thread_id.is_none());
    assert!(core.thread_id.is_none());

    let mut remote = SessionEntry {
        thread_id: Some("platform-strand-123".to_owned()),
        ..SessionEntry::default()
    };
    remote.migrate_legacy_thread_namespaces();
    assert_eq!(
        remote.remote_thread_id.as_deref(),
        Some("platform-strand-123")
    );
    assert!(remote.core_thread_id.is_none());
    assert!(remote.thread_id.is_none());
}
