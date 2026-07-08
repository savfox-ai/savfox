//! File-backed client-core cursor and event-cache store for Cokret account mode.
//!
//! Personal-agent listeners need restart-safe account subscribe cursors and
//! event dedupe. This store implements `garth`'s `CursorStore` and
//! `EventCacheStore` traits over a small JSON file so the gateway can persist
//! those positions without taking a database dependency.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use cokret::{DeviceId, Did, Error, EventId, Hash, RealmId, Result as CokretResult};
use garth::{CursorScope, CursorStore, EventCacheStore, OpaqueCursor};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

const STATE_VERSION: &str = "savfox.cokret.account_store.v1";
const DEFAULT_EVENT_CACHE_MAX: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CokretAccountStoreFile {
    version: String,
    scope_id: String,
    #[serde(default)]
    cursors: BTreeMap<String, OpaqueCursor>,
    #[serde(default)]
    event_cache: VecDeque<String>,
}

impl CokretAccountStoreFile {
    fn new(scope_id: String) -> Self {
        Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            cursors: BTreeMap::new(),
            event_cache: VecDeque::new(),
        }
    }
}

#[derive(Debug)]
struct FileCokretAccountStoreInner {
    path: PathBuf,
    scope_id: String,
    max_events: usize,
    state: Mutex<CokretAccountStoreFile>,
}

/// Restart-safe Cokret account state implementing client-core stores.
#[derive(Clone, Debug)]
pub struct FileCokretAccountStore {
    inner: Arc<FileCokretAccountStoreInner>,
}

impl FileCokretAccountStore {
    /// Open the default account store under
    /// `{savfox_home}/gateway/cokret-account-state/{channel_id}-{account_id}.json`.
    pub fn for_account(
        savfox_home: &Path,
        channel_id: &str,
        account_id: &str,
    ) -> CokretResult<Self> {
        Self::for_account_with_limit(savfox_home, channel_id, account_id, DEFAULT_EVENT_CACHE_MAX)
    }

    /// Open an account store with an explicit event-cache limit.
    pub fn for_account_with_limit(
        savfox_home: &Path,
        channel_id: &str,
        account_id: &str,
        max_events: usize,
    ) -> CokretResult<Self> {
        let scope_id = crate::cokret::account_scope_id(channel_id, account_id);
        let path = savfox_home
            .join("gateway")
            .join("cokret-account-state")
            .join(format!("{}.json", safe_file_stem(&scope_id)));
        Self::new(path, scope_id, max_events)
    }

    /// Open a store at an explicit path. Primarily useful for tests.
    pub fn new(path: PathBuf, scope_id: String, max_events: usize) -> CokretResult<Self> {
        let mut state = load_state(&path, &scope_id)?;
        trim_event_cache(&mut state.event_cache, max_events.max(1));
        Ok(Self {
            inner: Arc::new(FileCokretAccountStoreInner {
                path,
                scope_id,
                max_events: max_events.max(1),
                state: Mutex::new(state),
            }),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn ensure_created(&self) -> CokretResult<()> {
        let state = self.inner.state.lock();
        self.persist(&state)
    }

    /// Load the persisted account-subscribe cursor for a principal/device pair.
    pub async fn load_account_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
    ) -> CokretResult<Option<OpaqueCursor>> {
        self.load(account_scope(service_did, actor_id, device_id)?)
            .await
    }

    /// Persist the account-subscribe cursor for a principal/device pair.
    pub async fn save_account_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
        cursor: OpaqueCursor,
    ) -> CokretResult<()> {
        self.save(account_scope(service_did, actor_id, device_id)?, cursor)
            .await
    }

    /// Clear the account-subscribe cursor for a principal/device pair.
    pub async fn clear_account_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
    ) -> CokretResult<()> {
        self.clear(account_scope(service_did, actor_id, device_id)?)
            .await
    }

    /// Load the persisted Realm events cursor used by scan catch-up.
    pub async fn load_realm_events_cursor(
        &self,
        service_did: Option<&str>,
        realm_id: &str,
    ) -> CokretResult<Option<OpaqueCursor>> {
        self.load(realm_events_scope(service_did, realm_id)?).await
    }

    /// Persist the Realm events cursor used by scan catch-up.
    pub async fn save_realm_events_cursor(
        &self,
        service_did: Option<&str>,
        realm_id: &str,
        cursor: OpaqueCursor,
    ) -> CokretResult<()> {
        self.save(realm_events_scope(service_did, realm_id)?, cursor)
            .await
    }

    /// Clear the persisted Realm events cursor used by scan catch-up.
    pub async fn clear_realm_events_cursor(
        &self,
        service_did: Option<&str>,
        realm_id: &str,
    ) -> CokretResult<()> {
        self.clear(realm_events_scope(service_did, realm_id)?).await
    }

    /// Remember an event id if it has not been seen before.
    pub async fn remember_event_id_str(&self, event_id: &str) -> CokretResult<bool> {
        let event_id = EventId::new(event_id.to_owned())
            .map_err(|err| Error::Protocol(format!("invalid event id '{event_id}': {err}")))?;
        if self.seen(event_id.clone()).await? {
            return Ok(false);
        }
        self.remember(event_id).await?;
        Ok(true)
    }

    /// Clear the persisted event dedupe cache for this account.
    pub fn clear_event_cache(&self) -> CokretResult<()> {
        let mut state = self.inner.state.lock();
        state.event_cache.clear();
        self.persist(&state)
    }

    /// Load the direct `/_cokret/self/device_messages` cursor for a
    /// principal/device pair.
    pub async fn load_device_message_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
    ) -> CokretResult<Option<OpaqueCursor>> {
        let key = device_messages_scope_key(service_did, actor_id, device_id)?;
        let state = self.inner.state.lock();
        Ok(state.cursors.get(&key).cloned())
    }

    /// Persist the direct `/_cokret/self/device_messages` cursor for a
    /// principal/device pair.
    pub async fn save_device_message_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
        cursor: OpaqueCursor,
    ) -> CokretResult<()> {
        let key = device_messages_scope_key(service_did, actor_id, device_id)?;
        let mut state = self.inner.state.lock();
        state.cursors.insert(key, cursor);
        self.persist(&state)
    }

    /// Clear the direct device-message cursor for a principal/device pair.
    pub async fn clear_device_message_cursor(
        &self,
        service_did: Option<&str>,
        actor_id: &str,
        device_id: &str,
    ) -> CokretResult<()> {
        let key = device_messages_scope_key(service_did, actor_id, device_id)?;
        let mut state = self.inner.state.lock();
        state.cursors.remove(&key);
        self.persist(&state)
    }

    fn persist(&self, state: &CokretAccountStoreFile) -> CokretResult<()> {
        if state.scope_id != self.inner.scope_id {
            return Err(Error::Protocol(format!(
                "cokret account store scope mismatch: expected '{}', got '{}'",
                self.inner.scope_id, state.scope_id
            )));
        }
        if let Some(parent) = self.inner.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::Protocol(format!(
                    "cokret account store create {}: {err}",
                    parent.display()
                ))
            })?;
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|err| Error::Protocol(format!("cokret account store serialize: {err}")))?;
        let tmp = tmp_path(&self.inner.path);
        std::fs::write(&tmp, bytes).map_err(|err| {
            Error::Protocol(format!(
                "cokret account store write {}: {err}",
                tmp.display()
            ))
        })?;
        std::fs::rename(&tmp, &self.inner.path).map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            Error::Protocol(format!(
                "cokret account store rename {} -> {}: {err}",
                tmp.display(),
                self.inner.path.display()
            ))
        })
    }
}

impl CursorStore for FileCokretAccountStore {
    async fn load(&self, scope: CursorScope) -> CokretResult<Option<OpaqueCursor>> {
        let key = scope_key(&scope)?;
        let state = self.inner.state.lock();
        Ok(state.cursors.get(&key).cloned())
    }

    async fn save(&self, scope: CursorScope, cursor: OpaqueCursor) -> CokretResult<()> {
        let key = scope_key(&scope)?;
        let mut state = self.inner.state.lock();
        state.cursors.insert(key, cursor);
        self.persist(&state)
    }

    async fn clear(&self, scope: CursorScope) -> CokretResult<()> {
        let key = scope_key(&scope)?;
        let mut state = self.inner.state.lock();
        state.cursors.remove(&key);
        self.persist(&state)
    }
}

impl EventCacheStore for FileCokretAccountStore {
    async fn seen(&self, event_id: EventId) -> CokretResult<bool> {
        let state = self.inner.state.lock();
        Ok(state
            .event_cache
            .iter()
            .any(|seen| seen == event_id.as_str()))
    }

    async fn remember(&self, event_id: EventId) -> CokretResult<()> {
        let mut state = self.inner.state.lock();
        let event_id = event_id.as_str().to_owned();
        if !state.event_cache.iter().any(|seen| seen == &event_id) {
            state.event_cache.push_back(event_id);
            trim_event_cache(&mut state.event_cache, self.inner.max_events);
            self.persist(&state)?;
        }
        Ok(())
    }
}

fn load_state(path: &Path, scope_id: &str) -> CokretResult<CokretAccountStoreFile> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(CokretAccountStoreFile::new(scope_id.to_owned())),
        Ok(bytes) => {
            let state: CokretAccountStoreFile = serde_json::from_slice(&bytes).map_err(|err| {
                Error::Protocol(format!(
                    "cokret account store parse {}: {err}",
                    path.display()
                ))
            })?;
            if state.version != STATE_VERSION {
                return Err(Error::Protocol(format!(
                    "unsupported Cokret account store version '{}' in {}",
                    state.version,
                    path.display()
                )));
            }
            if state.scope_id != scope_id {
                return Err(Error::Protocol(format!(
                    "Cokret account store scope mismatch: expected '{scope_id}', got '{}'",
                    state.scope_id
                )));
            }
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(CokretAccountStoreFile::new(scope_id.to_owned()))
        }
        Err(err) => Err(Error::Protocol(format!(
            "cokret account store read {}: {err}",
            path.display()
        ))),
    }
}

fn account_scope(
    service_did: Option<&str>,
    actor_id: &str,
    device_id: &str,
) -> CokretResult<CursorScope> {
    Ok(CursorScope::Account {
        service_did: service_did
            .map(|value| {
                Did::new(value.to_owned())
                    .map_err(|err| Error::Protocol(format!("invalid service DID '{value}': {err}")))
            })
            .transpose()?,
        actor_id: Did::new(actor_id.to_owned())
            .map_err(|err| Error::Protocol(format!("invalid actor DID '{actor_id}': {err}")))?,
        device_id: DeviceId::new(device_id.to_owned()).map_err(|err| {
            Error::Protocol(format!("invalid Cokret device id '{device_id}': {err}"))
        })?,
    })
}

fn realm_events_scope(service_did: Option<&str>, realm_id: &str) -> CokretResult<CursorScope> {
    Ok(CursorScope::RealmEvents {
        service_did: service_did
            .map(|value| {
                Did::new(value.to_owned())
                    .map_err(|err| Error::Protocol(format!("invalid service DID '{value}': {err}")))
            })
            .transpose()?,
        realm_id: RealmId::new(realm_id.to_owned()).map_err(|err| {
            Error::Protocol(format!("invalid Cokret realm id '{realm_id}': {err}"))
        })?,
    })
}

fn device_messages_scope_key(
    service_did: Option<&str>,
    actor_id: &str,
    device_id: &str,
) -> CokretResult<String> {
    let service_did = service_did
        .map(|value| {
            Did::new(value.to_owned())
                .map_err(|err| Error::Protocol(format!("invalid service DID '{value}': {err}")))
        })
        .transpose()?;
    let actor_id = Did::new(actor_id.to_owned())
        .map_err(|err| Error::Protocol(format!("invalid actor DID '{actor_id}': {err}")))?;
    let device_id = DeviceId::new(device_id.to_owned())
        .map_err(|err| Error::Protocol(format!("invalid Cokret device id '{device_id}': {err}")))?;
    serde_json::to_string(&json!({
        "kind": "device_messages",
        "service_did": service_did.as_ref().map(Did::as_str),
        "actor_id": actor_id.as_str(),
        "device_id": device_id.as_str(),
    }))
    .context("serialize Cokret device-message cursor scope key")
    .map_err(|err| Error::Protocol(err.to_string()))
}

fn scope_key(scope: &CursorScope) -> CokretResult<String> {
    let value = match scope {
        CursorScope::Account {
            service_did,
            actor_id,
            device_id,
        } => json!({
            "kind": "account",
            "service_did": service_did.as_ref().map(Did::as_str),
            "actor_id": actor_id.as_str(),
            "device_id": device_id.as_str(),
        }),
        CursorScope::RealmEvents {
            service_did,
            realm_id,
        } => json!({
            "kind": "realm_events",
            "service_did": service_did.as_ref().map(Did::as_str),
            "realm_id": realm_id.as_str(),
        }),
        CursorScope::EventsQuery {
            service_did,
            realms,
            actors,
            order,
            filter_digest,
        } => json!({
            "kind": "events_query",
            "service_did": service_did.as_ref().map(Did::as_str),
            "realms": realms.iter().map(RealmId::as_str).collect::<Vec<_>>(),
            "actors": actors.iter().map(Did::as_str).collect::<Vec<_>>(),
            "order": order,
            "filter_digest": filter_digest.as_ref().map(Hash::as_str),
        }),
    };
    serde_json::to_string(&value)
        .context("serialize Cokret cursor scope key")
        .map_err(|err| Error::Protocol(err.to_string()))
}

fn trim_event_cache(cache: &mut VecDeque<String>, max_events: usize) {
    while cache.len() > max_events {
        cache.pop_front();
    }
}

fn safe_file_stem(scope_id: &str) -> String {
    let mut out = String::with_capacity(scope_id.len());
    for ch in scope_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_owned()
    } else {
        out
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "cokret-account-store.json".into());
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-cokret-account-store-{}-{}-{}.json",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn account_cursor_scope() -> CursorScope {
        account_scope(
            None,
            "did:webvh:z6mkfixture:alice.example",
            "ck:device:01904100-0000-7000-8000-000000000001",
        )
        .unwrap()
    }

    fn realm_id() -> &'static str {
        "ck:realm:01904100-0000-7000-8000-000000000001"
    }

    fn event_id(n: u8) -> EventId {
        EventId::new(format!("ck:event:01904100-0000-7000-8000-{:0>12}", n)).unwrap()
    }

    #[tokio::test]
    async fn cursor_round_trips_across_instances() {
        let path = temp_file("cursor");
        let scope_id = "account:channel:account".to_owned();

        let store = FileCokretAccountStore::new(path.clone(), scope_id.clone(), 16).unwrap();
        store
            .save(account_cursor_scope(), "ck:cursor:account-1".to_owned())
            .await
            .unwrap();

        let reopened = FileCokretAccountStore::new(path.clone(), scope_id, 16).unwrap();
        assert_eq!(
            reopened
                .load(account_cursor_scope())
                .await
                .unwrap()
                .as_deref(),
            Some("ck:cursor:account-1")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn realm_events_cursor_round_trips_across_instances() {
        let path = temp_file("realm-cursor");
        let scope_id = "account:channel:account".to_owned();

        let store = FileCokretAccountStore::new(path.clone(), scope_id.clone(), 16).unwrap();
        store
            .save_realm_events_cursor(None, realm_id(), "ck:cursor:realm-1".to_owned())
            .await
            .unwrap();

        let reopened = FileCokretAccountStore::new(path.clone(), scope_id, 16).unwrap();
        assert_eq!(
            reopened
                .load_realm_events_cursor(None, realm_id())
                .await
                .unwrap()
                .as_deref(),
            Some("ck:cursor:realm-1")
        );

        reopened
            .clear_realm_events_cursor(None, realm_id())
            .await
            .unwrap();
        assert!(
            reopened
                .load_realm_events_cursor(None, realm_id())
                .await
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn device_message_cursor_round_trips_across_instances() {
        let path = temp_file("device-messages");
        let scope_id = "account:channel:account".to_owned();

        let store = FileCokretAccountStore::new(path.clone(), scope_id.clone(), 16).unwrap();
        store
            .save_device_message_cursor(
                None,
                "did:webvh:z6mkfixture:alice.example",
                "ck:device:01904100-0000-7000-8000-000000000001",
                "ck:cursor:device-1".to_owned(),
            )
            .await
            .unwrap();

        let reopened = FileCokretAccountStore::new(path.clone(), scope_id, 16).unwrap();
        assert_eq!(
            reopened
                .load_device_message_cursor(
                    None,
                    "did:webvh:z6mkfixture:alice.example",
                    "ck:device:01904100-0000-7000-8000-000000000001",
                )
                .await
                .unwrap()
                .as_deref(),
            Some("ck:cursor:device-1")
        );

        reopened
            .clear_device_message_cursor(
                None,
                "did:webvh:z6mkfixture:alice.example",
                "ck:device:01904100-0000-7000-8000-000000000001",
            )
            .await
            .unwrap();
        assert!(
            reopened
                .load_device_message_cursor(
                    None,
                    "did:webvh:z6mkfixture:alice.example",
                    "ck:device:01904100-0000-7000-8000-000000000001",
                )
                .await
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn event_cache_dedupes_and_trims_oldest_entries() {
        let path = temp_file("events");
        let scope_id = "account:channel:account".to_owned();
        let store = FileCokretAccountStore::new(path.clone(), scope_id.clone(), 2).unwrap();

        store.remember(event_id(1)).await.unwrap();
        store.remember(event_id(2)).await.unwrap();
        assert!(store.seen(event_id(1)).await.unwrap());

        store.remember(event_id(3)).await.unwrap();
        assert!(!store.seen(event_id(1)).await.unwrap());
        assert!(store.seen(event_id(2)).await.unwrap());
        assert!(store.seen(event_id(3)).await.unwrap());

        let reopened = FileCokretAccountStore::new(path.clone(), scope_id, 2).unwrap();
        assert!(reopened.seen(event_id(2)).await.unwrap());
        assert!(reopened.seen(event_id(3)).await.unwrap());

        let _ = std::fs::remove_file(&path);
    }
}
