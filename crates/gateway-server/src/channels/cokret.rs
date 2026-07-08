//! Cokret personal-agent channel runtime.
//!
//! Owns one async task per (channel, account) pair. Each task:
//!
//! 1. Mints a short-lived `agent_key_proof` session grant bound to DPoP.
//! 2. Opens `/_cokret/self/account/subscribe` for the owning user account.
//! 3. Extracts dispatchable `ck.message.create` events.
//! 4. Dispatches each event to the agent pipeline.
//!
//! Outbound sends go through [`send_to_cokret_account`].

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use cokret::{
    DeviceId, DeviceMessagesAckRequestBody, Did, KeyPackageUploadEntry,
    KeyPackagesConsumeRequestBody, KeyPackagesUploadRequestBody, MlsKeyPackageRecord, RealmId,
    StrandId,
};
use garth::{ClientEvent, SubscriptionStopReason};
use savfox_channels::cokret::{
    CokretAccountConfig, CokretChannelConfig, CokretDecryptDetailedOutcome, CokretEncryptOutcome,
    CokretHttpClient, CokretInboundEvent, CokretInboundParseResult, CokretInboundSkipReason,
    CokretInboundSkippedEvent, CokretMlsWelcomeConsumeBinding, FileCokretAccountStore,
    FileCokretCryptoStore, MessageCreateRequest, account_allows_event_read,
    build_message_create_event, parse_delta_frame_for_account,
    parse_notification_delta_for_account, resolve_cokret_outbound_account,
    sign_key_operation_value,
};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use super::{ChannelRegistry, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// Per-(channel, account) runtime handles. Indexed by `{channel_id}::{account_id}`.
#[derive(Default)]
struct CokretRuntimeState {
    handles: HashMap<String, tokio::task::JoinHandle<()>>,
}

const ACCOUNT_EVENT_DEDUPE_MAX: usize = 4096;
const ACCOUNT_SCAN_CATCHUP_LIMIT: u32 = 100;
const ACCOUNT_SCAN_CATCHUP_MAX_PAGES: usize = 64;
const DEVICE_MESSAGES_PULL_LIMIT: u32 = 100;
const DEVICE_MESSAGES_PULL_MAX_PAGES: usize = 16;

const KEYPACKAGES_UPLOAD_SCOPE: &str = "ck.self.keys.keypackages.upload.create";
const KEYPACKAGES_CONSUME_SCOPE: &str = "ck.self.keys.keypackages.command.consume";
const DEVICE_MESSAGES_LIST_SCOPE: &str = "ck.self.device_messages.query.list";
const DEVICE_MESSAGES_ACK_SCOPE: &str = "ck.self.device_messages.command.ack";

/// Refresh an agent session grant this many seconds before it expires.
const SESSION_REFRESH_SKEW_SECS: i64 = 60;

fn runtime_state() -> &'static StdMutex<CokretRuntimeState> {
    static STATE: OnceLock<StdMutex<CokretRuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| StdMutex::new(CokretRuntimeState::default()))
}

fn task_key(channel_id: &str, account_id: &str) -> String {
    format!("{channel_id}::{account_id}")
}

pub(crate) async fn start_cokret_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let cokret_config = CokretChannelConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Cokret channel config must be an object"))?;
    cokret_config
        .validate()
        .with_context(|| format!("Cokret channel '{}' config validation failed", config.id))?;

    info!(
        "cokret: channel '{}' validated; {} account(s), base_url='{}'",
        cokret_config.id,
        cokret_config.accounts.len(),
        cokret_config.base_url,
    );

    for account in &cokret_config.accounts {
        if account.listen {
            spawn_account_listener(
                gateway_channel.config().savfox_home.clone(),
                cokret_config.clone(),
                account.clone(),
                Arc::clone(gateway_channel),
                Arc::clone(session_store),
            );
        }
    }

    Ok(())
}

/// Abort and drop all account-subscribe listener tasks belonging to a channel.
///
/// Called when a Cokret channel is disabled/deleted so the long-poll tasks
/// (and their `JoinHandle`s) don't leak and keep dispatching events for a
/// channel the operator already removed. Returns the number of tasks stopped.
pub(crate) fn stop_cokret_account_listeners(channel_id: &str) -> usize {
    let prefix = format!("{channel_id}::");
    let Ok(mut state) = runtime_state().lock() else {
        warn!("cokret: runtime state mutex poisoned; cannot stop listeners for '{channel_id}'");
        return 0;
    };
    let keys: Vec<String> = state
        .handles
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect();
    let mut stopped = 0;
    for key in keys {
        if let Some(handle) = state.handles.remove(&key) {
            handle.abort();
            stopped += 1;
        }
    }
    stopped
}

pub(crate) fn cokret_account_listener_count(channel_id: &str) -> usize {
    let prefix = format!("{channel_id}::");
    let Ok(state) = runtime_state().lock() else {
        warn!("cokret: runtime state mutex poisoned; cannot inspect listeners for '{channel_id}'");
        return 0;
    };
    state
        .handles
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .count()
}

fn spawn_account_listener(
    savfox_home: PathBuf,
    channel: CokretChannelConfig,
    account: CokretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let key = task_key(&channel.id, &account.id);
    let handle = tokio::spawn(async move {
        run_account_listener(
            savfox_home,
            channel,
            account,
            gateway_channel,
            session_store,
        )
        .await;
    });
    let Ok(mut state) = runtime_state().lock() else {
        warn!("cokret: runtime state mutex poisoned; aborting listener task '{key}'");
        handle.abort();
        return;
    };
    if let Some(prev) = state.handles.insert(key, handle) {
        prev.abort();
    }
}

async fn run_account_listener(
    savfox_home: PathBuf,
    channel: CokretChannelConfig,
    account: CokretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    if !account.has_requested_scope("ck.self.events.stream.subscribe") {
        warn!(
            "cokret: account '{}' listen=true but missing ck.self.events.stream.subscribe; refusing to open subscribe endpoint",
            account.id
        );
        runtime::record_channel_probe("cokret", "error").await;
        return;
    }

    let mut backoff = Duration::from_secs(1);
    let account_store = match FileCokretAccountStore::for_account_with_limit(
        &savfox_home,
        &channel.id,
        &account.id,
        ACCOUNT_EVENT_DEDUPE_MAX,
    ) {
        Ok(store) => {
            if let Err(err) = store.ensure_created() {
                warn!(
                    "cokret: account '{}' durable subscribe state unavailable at {}: {err}",
                    account.id,
                    store.path().display()
                );
                runtime::record_channel_probe("cokret", "error").await;
                return;
            } else {
                store
            }
        }
        Err(err) => {
            warn!(
                "cokret: account '{}' failed to open durable subscribe state: {err}",
                account.id
            );
            runtime::record_channel_probe("cokret", "error").await;
            return;
        }
    };
    let crypto_store = FileCokretCryptoStore::for_account(&savfox_home, &channel.id, &account.id);
    if let Err(err) =
        FileCokretCryptoStore::feature_report().and_then(|_| crypto_store.ensure_created())
    {
        warn!(
            "cokret: account '{}' crypto state unavailable at {}: {err:#}",
            account.id,
            crypto_store.path().display()
        );
    }

    let (mut client, mut session_expiry) = match construct_account_client(&channel, &account).await
    {
        Ok(pair) => pair,
        Err(err) => {
            warn!(
                "cokret: account '{}' on channel '{}' failed to construct HTTP client: {err:#}",
                account.id, channel.id
            );
            runtime::record_channel_probe("cokret", "error").await;
            return;
        }
    };
    runtime::record_channel_probe("cokret", "ok").await;
    run_account_key_lifecycle_maintenance(
        &client,
        &channel,
        &account,
        &account_store,
        &crypto_store,
        "startup",
    )
    .await;

    loop {
        runtime::record_channel_probe("cokret", "ok").await;

        // Proactively refresh the agent session grant before it expires so a
        // long-lived stream does not wait for Unauthorized to recover.
        if let Some(expiry) = session_expiry
            && session_grant_needs_refresh(Some(expiry), Utc::now())
        {
            info!(
                "cokret: account '{}' session grant near expiry ({expiry}) — refreshing",
                account.id
            );
            match construct_account_client(&channel, &account).await {
                Ok((fresh, fresh_expiry)) => {
                    client = fresh;
                    session_expiry = fresh_expiry;
                    runtime::record_channel_probe("cokret", "ok").await;
                }
                Err(err) => {
                    warn!(
                        "cokret: account '{}' proactive refresh failed: {err:#}; stopping listener",
                        account.id
                    );
                    runtime::record_channel_probe("cokret", "error").await;
                    return;
                }
            }
        }

        match drive_account_subscription_engine(
            &client,
            session_expiry,
            &channel,
            &account,
            account_store.clone(),
            crypto_store.clone(),
            Arc::clone(&gateway_channel),
            Arc::clone(&session_store),
        )
        .await
        {
            AccountEngineOutcome::RefreshSession => {
                info!(
                    "cokret: account '{}' session grant near expiry — refreshing",
                    account.id
                );
                match construct_account_client(&channel, &account).await {
                    Ok((fresh, fresh_expiry)) => {
                        client = fresh;
                        session_expiry = fresh_expiry;
                        backoff = Duration::from_secs(1);
                        runtime::record_channel_probe("cokret", "ok").await;
                    }
                    Err(err) => {
                        warn!(
                            "cokret: account '{}' proactive refresh failed: {err:#}; stopping listener",
                            account.id
                        );
                        runtime::record_channel_probe("cokret", "error").await;
                        return;
                    }
                }
            }
            AccountEngineOutcome::Unauthorized { reason } => {
                // The agent session grant expired or was revoked. Back off and
                // try a fresh agent_key_proof session exchange; the shared
                // subscription engine owns cursor recovery semantics.
                warn!(
                    "cokret: account '{}' became unauthorized mid-stream{} — refreshing agent session",
                    account.id,
                    reason
                        .as_deref()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default()
                );
                sleep_with_backoff(&mut backoff).await;
                match construct_account_client(&channel, &account).await {
                    Ok((fresh, fresh_expiry)) => {
                        client = fresh;
                        session_expiry = fresh_expiry;
                        backoff = Duration::from_secs(1);
                        runtime::record_channel_probe("cokret", "ok").await;
                        info!(
                            "cokret: account '{}' agent session refresh succeeded",
                            account.id
                        );
                    }
                    Err(err) => {
                        warn!(
                            "cokret: account '{}' agent session refresh failed: {err:#}; stopping listener",
                            account.id
                        );
                        runtime::record_channel_probe("cokret", "error").await;
                        return;
                    }
                }
            }
            AccountEngineOutcome::Cancelled => {
                debug!(
                    "cokret: account '{}' subscription engine cancelled; reconnecting",
                    account.id
                );
                backoff = Duration::from_secs(1);
            }
            AccountEngineOutcome::Retry { error } => {
                warn!(
                    "cokret: subscribe engine for '{}/{}' failed: {error:#}",
                    channel.id, account.id
                );
                runtime::record_channel_probe("cokret", "error").await;
                sleep_with_backoff(&mut backoff).await;
            }
        }
    }
}

#[derive(Debug)]
enum AccountEngineOutcome {
    RefreshSession,
    Unauthorized { reason: Option<String> },
    Cancelled,
    Retry { error: anyhow::Error },
}

fn session_grant_needs_refresh(session_expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    session_expiry
        .is_some_and(|expiry| now >= expiry - chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS))
}

fn session_refresh_delay(
    session_expiry: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let expiry = session_expiry?;
    let refresh_at = expiry - chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS);
    if now >= refresh_at {
        return Some(Duration::ZERO);
    }
    (refresh_at - now).to_std().ok()
}

async fn drive_account_subscription_engine(
    client: &CokretHttpClient,
    session_expiry: Option<DateTime<Utc>>,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: FileCokretAccountStore,
    crypto_store: FileCokretCryptoStore,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> AccountEngineOutcome {
    let actor_id = match Did::new(account.principal_id.clone()) {
        Ok(actor_id) => actor_id,
        Err(err) => {
            return AccountEngineOutcome::Retry {
                error: anyhow::anyhow!("invalid Cokret account principal id: {err}"),
            };
        }
    };
    let device_id = match DeviceId::new(account.device_id.clone()) {
        Ok(device_id) => device_id,
        Err(err) => {
            return AccountEngineOutcome::Retry {
                error: anyhow::anyhow!("invalid Cokret account device id: {err}"),
            };
        }
    };
    let service_did = match account_subscription_service_did(channel, account) {
        Ok(service_did) => service_did,
        Err(error) => return AccountEngineOutcome::Retry { error },
    };
    let client_core = client.client_core_with_account_store(account_store.clone());
    let mut engine = client_core.subscription_engine();
    if let Some(service_did) = service_did {
        engine = engine.with_service_did(service_did);
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = move |event: ClientEvent| {
        let _ = tx.send(event);
    };
    let run = engine.run_account(actor_id, device_id, client.inner(), &sink);
    tokio::pin!(run);

    match session_refresh_delay(session_expiry, Utc::now()) {
        Some(delay) => {
            let refresh = tokio::time::sleep(delay);
            tokio::pin!(refresh);
            loop {
                tokio::select! {
                    result = &mut run => return account_engine_outcome_from_result(result),
                    maybe_event = rx.recv() => {
                        if let Some(event) = maybe_event {
                            handle_account_client_event(
                                client,
                                event,
                                channel,
                                account,
                                &account_store,
                                &crypto_store,
                                &gateway_channel,
                                &session_store,
                            )
                            .await;
                        }
                    }
                    _ = &mut refresh => {
                        engine.cancel();
                        return AccountEngineOutcome::RefreshSession;
                    }
                }
            }
        }
        None => loop {
            tokio::select! {
                result = &mut run => return account_engine_outcome_from_result(result),
                maybe_event = rx.recv() => {
                    if let Some(event) = maybe_event {
                        handle_account_client_event(
                            client,
                            event,
                            channel,
                            account,
                            &account_store,
                            &crypto_store,
                            &gateway_channel,
                            &session_store,
                        )
                        .await;
                    }
                }
            }
        },
    }
}

fn account_engine_outcome_from_result(
    result: cokret::Result<SubscriptionStopReason>,
) -> AccountEngineOutcome {
    match result {
        Ok(SubscriptionStopReason::Cancelled) => AccountEngineOutcome::Cancelled,
        Ok(SubscriptionStopReason::Unauthorized { reason }) => {
            AccountEngineOutcome::Unauthorized { reason }
        }
        Err(err) => AccountEngineOutcome::Retry {
            error: anyhow::anyhow!("account subscription engine: {err}"),
        },
    }
}

async fn handle_account_client_event(
    client: &CokretHttpClient,
    event: ClientEvent,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    match event {
        ClientEvent::AccountUpdates(updates) => {
            handle_sync_updates_for_account(
                client,
                updates,
                channel,
                account,
                account_store,
                crypto_store,
                gateway_channel,
                session_store,
            )
            .await;
        }
        other => {
            debug!(
                "cokret: account '{}/{}' ignored non-account subscription event: {:?}",
                channel.id, account.id, other
            );
        }
    }
}

fn account_cursor_service_did(
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
) -> Option<String> {
    account
        .cokret_server_did
        .clone()
        .or_else(|| channel.service_did.clone())
        .or_else(|| {
            account
                .inkson_bootstrap
                .as_ref()
                .map(|bootstrap| bootstrap.service_did.to_string())
        })
}

fn account_subscription_service_did(
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
) -> anyhow::Result<Option<Did>> {
    account_cursor_service_did(channel, account)
        .map(|value| {
            Did::new(value.clone())
                .map_err(|err| anyhow::anyhow!("invalid Cokret service DID '{value}': {err}"))
        })
        .transpose()
}

async fn run_account_key_lifecycle_maintenance(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    reason: &'static str,
) {
    publish_account_mls_key_packages(client, channel, account, crypto_store).await;
    drain_account_device_messages(
        client,
        channel,
        account,
        account_store,
        crypto_store,
        reason,
    )
    .await;
}

async fn publish_account_mls_key_packages(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    crypto_store: &FileCokretCryptoStore,
) {
    if !account.has_requested_scope(KEYPACKAGES_UPLOAD_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = KEYPACKAGES_UPLOAD_SCOPE,
            "cokret: cannot publish MLS KeyPackages without requested standard scope"
        );
        return;
    }
    let principal = match Did::new(account.principal_id.clone()) {
        Ok(principal) => principal,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "cokret: invalid principal id for MLS KeyPackage upload: {err}"
            );
            return;
        }
    };
    let device = match DeviceId::new(account.device_id.clone()) {
        Ok(device) => device,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "cokret: invalid device id for MLS KeyPackage upload: {err}"
            );
            return;
        }
    };

    let mut records = Vec::with_capacity(2);
    for last_resort in [false, true] {
        match crypto_store.ensure_mls_key_package(
            &account.principal_id,
            &account.device_id,
            last_resort,
        ) {
            Ok(record) => records.push(record),
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    last_resort,
                    "cokret: failed to ensure local MLS KeyPackage: {err:#}"
                );
                return;
            }
        }
    }

    let mut entries = records
        .iter()
        .map(|record| key_package_upload_entry(record, None))
        .collect::<Vec<_>>();
    let signature_value = match key_package_upload_signature_value(&principal, &device, &entries) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "cokret: failed to build MLS KeyPackage upload signature payload: {err:#}"
            );
            return;
        }
    };
    let signature =
        match sign_account_key_operation(account, KEYPACKAGES_UPLOAD_SCOPE, &signature_value) {
            Ok(signature) => signature,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    "cokret: failed to sign MLS KeyPackage upload request: {err:#}"
                );
                return;
            }
        };
    for entry in &mut entries {
        entry.device_signature = Some(signature.clone());
    }
    let request = KeyPackagesUploadRequestBody {
        principal_id: principal,
        device_id: device,
        key_packages: entries,
        device_signature: signature,
        expires_at: None,
        strand_id: None,
        mls_group_id: None,
    };

    match client.inner().keypackages_upload(&request).await {
        Ok(outcome) => {
            if !outcome.rejected.is_empty() {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    accepted = outcome.accepted,
                    rejected = ?outcome.rejected,
                    "cokret: MLS KeyPackage upload returned rejected entries"
                );
            } else {
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    accepted = outcome.accepted,
                    refs = outcome.key_package_refs.len(),
                    "cokret: published MLS KeyPackages"
                );
            }
        }
        Err(err) => warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "cokret: MLS KeyPackage upload failed: {err}"
        ),
    }
}

fn key_package_upload_entry(
    record: &MlsKeyPackageRecord,
    device_signature: Option<cokret::KeyOperationSignature>,
) -> KeyPackageUploadEntry {
    KeyPackageUploadEntry {
        keypackage_id: record.keypackage_id.clone(),
        keypackage_ref: record.keypackage_ref.as_str().to_owned(),
        keypackage_digest: record.keypackage_ref.clone(),
        key_package: Value::String(record.key_package.clone()),
        cipher_suites: record.cipher_suites.clone(),
        capabilities: record.capabilities.clone(),
        expires_at: record
            .expires_at
            .unwrap_or_else(|| record.created_at + chrono::Duration::days(7)),
        created_at: record.created_at,
        device_signature,
        last_resort: Some(record.last_resort),
    }
}

fn key_package_upload_signature_value(
    principal: &Did,
    device: &DeviceId,
    entries: &[KeyPackageUploadEntry],
) -> anyhow::Result<Value> {
    let key_packages = serde_json::to_value(entries)?;
    Ok(json!({
        "principal_id": principal.as_str(),
        "device_id": device.as_str(),
        "key_packages": key_packages,
    }))
}

async fn drain_account_device_messages_from_cursor(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    initial_cursor: Option<String>,
    reason: &'static str,
) {
    if !account.has_requested_scope(DEVICE_MESSAGES_LIST_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = DEVICE_MESSAGES_LIST_SCOPE,
            reason,
            "cokret: cannot pull standard device messages without requested scope"
        );
        return;
    }
    let service_did = account_cursor_service_did(channel, account);
    let mut cursor = if let Some(cursor) = initial_cursor {
        Some(cursor)
    } else {
        match account_store
            .load_device_message_cursor(
                service_did.as_deref(),
                &account.principal_id,
                &account.device_id,
            )
            .await
        {
            Ok(cursor) => cursor,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    reason,
                    "cokret: failed to load device-message cursor: {err}"
                );
                None
            }
        }
    };

    for page in 0..DEVICE_MESSAGES_PULL_MAX_PAGES {
        let outcome = match client
            .inner()
            .receive_device_messages(cursor.as_deref(), Some(DEVICE_MESSAGES_PULL_LIMIT))
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    reason,
                    "cokret: standard device_messages pull failed: {err}"
                );
                return;
            }
        };

        for message in &outcome.messages {
            record_account_mls_welcome_from_value_tree(
                crypto_store,
                &message.content,
                channel,
                account,
                "device_messages",
            );
        }
        if outcome.lost {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                page,
                "cokret: device_messages reported cursor loss; clearing local cursor without ack"
            );
            if let Err(err) = account_store
                .clear_device_message_cursor(
                    service_did.as_deref(),
                    &account.principal_id,
                    &account.device_id,
                )
                .await
            {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    "cokret: failed to clear lost device-message cursor: {err}"
                );
            }
            return;
        }
        if !outcome.messages.is_empty() && outcome.ack_token.is_none() {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                "cokret: device_messages returned messages without ack_token; leaving cursor unchanged"
            );
            return;
        }
        if let Some(ack_token) = outcome.ack_token.as_deref()
            && !ack_account_device_messages(client, channel, account, ack_token, reason).await
        {
            return;
        }
        if let Some(next_cursor) = outcome.next_cursor {
            if let Err(err) = account_store
                .save_device_message_cursor(
                    service_did.as_deref(),
                    &account.principal_id,
                    &account.device_id,
                    next_cursor.clone(),
                )
                .await
            {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    reason,
                    "cokret: failed to persist device-message cursor: {err}"
                );
                return;
            }
            cursor = Some(next_cursor);
        }
        if outcome.limited {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                page,
                "cokret: device_messages page was limited"
            );
        }
        if !outcome.has_more {
            return;
        }
    }
    warn!(
        channel_id = %channel.id,
        account_id = %account.id,
        reason,
        max_pages = DEVICE_MESSAGES_PULL_MAX_PAGES,
        "cokret: stopped device_messages pull after page cap"
    );
}

async fn ack_account_device_messages(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    ack_token: &str,
    reason: &'static str,
) -> bool {
    if !account.has_requested_scope(DEVICE_MESSAGES_ACK_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = DEVICE_MESSAGES_ACK_SCOPE,
            reason,
            "cokret: cannot ack standard device messages without requested scope"
        );
        return false;
    }
    let request = DeviceMessagesAckRequestBody {
        ack_token: ack_token.to_owned(),
    };
    match client.inner().ack_device_messages(&request).await {
        Ok(outcome) if outcome.ok => true,
        Ok(outcome) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                pruned_count = ?outcome.pruned_count,
                "cokret: device_messages ack returned ok=false"
            );
            false
        }
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                "cokret: device_messages ack failed: {err}"
            );
            false
        }
    }
}

async fn consume_account_mls_key_packages(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    crypto_store: &FileCokretCryptoStore,
    bindings: &[CokretMlsWelcomeConsumeBinding],
) {
    if bindings.is_empty() {
        return;
    }
    if !account.has_requested_scope(KEYPACKAGES_CONSUME_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = KEYPACKAGES_CONSUME_SCOPE,
            count = bindings.len(),
            "cokret: cannot consume MLS KeyPackages without requested standard scope"
        );
        return;
    }
    let consumer_device = match DeviceId::new(account.device_id.clone()) {
        Ok(device) => device,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "cokret: invalid device id for MLS KeyPackage consume: {err}"
            );
            return;
        }
    };

    for binding in bindings {
        let realm_id = match optional_realm_id(binding.realm_id.as_deref()) {
            Ok(realm_id) => realm_id,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    "cokret: invalid Realm id in MLS Welcome consume binding: {err}"
                );
                continue;
            }
        };
        let strand_id = match optional_strand_id(binding.strand_id.as_deref()) {
            Ok(strand_id) => strand_id,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    "cokret: invalid Strand id in MLS Welcome consume binding: {err}"
                );
                continue;
            }
        };
        let unsigned_value =
            key_package_consume_signature_value(binding, &consumer_device, &realm_id, &strand_id);
        let signature =
            match sign_account_key_operation(account, KEYPACKAGES_CONSUME_SCOPE, &unsigned_value) {
                Ok(signature) => signature,
                Err(err) => {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        keypackage_ref = %binding.keypackage_ref,
                        "cokret: failed to sign MLS KeyPackage consume request: {err:#}"
                    );
                    continue;
                }
            };
        let request = KeyPackagesConsumeRequestBody {
            key_package_refs: vec![binding.keypackage_ref.clone()],
            consumer_device_id: consumer_device.clone(),
            signature,
            claim_ids: vec![binding.claim_id.clone()],
            welcome_ref: binding.welcome_ref.clone(),
            realm_id,
            strand_id,
            mls_group_id: Some(binding.mls_group_id.clone()),
            epoch: Some(binding.epoch),
        };
        match client.inner().keypackages_consume(&request).await {
            Ok(outcome) if outcome.failures.is_empty() => {
                if let Err(err) =
                    crypto_store.mark_mls_key_package_consumed(&binding.keypackage_ref)
                {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        keypackage_ref = %binding.keypackage_ref,
                        "cokret: failed to mark local MLS KeyPackage consumed after server ack: {err:#}"
                    );
                }
                if let Err(err) = crypto_store.mark_mls_welcome_consume_binding_acked(binding) {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        keypackage_ref = %binding.keypackage_ref,
                        "cokret: failed to clear MLS Welcome consume binding after server ack: {err:#}"
                    );
                }
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    consumed = outcome.consumed.len(),
                    "cokret: consumed MLS KeyPackage after Welcome decrypt"
                );
            }
            Ok(outcome) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    failures = ?outcome.failures,
                    "cokret: MLS KeyPackage consume returned failures"
                );
            }
            Err(err) => warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %binding.keypackage_ref,
                "cokret: MLS KeyPackage consume failed: {err}"
            ),
        }
    }
}

fn key_package_consume_signature_value(
    binding: &CokretMlsWelcomeConsumeBinding,
    consumer_device: &DeviceId,
    realm_id: &Option<RealmId>,
    strand_id: &Option<StrandId>,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "key_package_refs".to_owned(),
        Value::Array(vec![Value::String(binding.keypackage_ref.clone())]),
    );
    object.insert(
        "consumer_device_id".to_owned(),
        Value::String(consumer_device.as_str().to_owned()),
    );
    object.insert(
        "claim_ids".to_owned(),
        Value::Array(vec![Value::String(binding.claim_id.clone())]),
    );
    if let Some(welcome_ref) = &binding.welcome_ref {
        object.insert("welcome_ref".to_owned(), Value::String(welcome_ref.clone()));
    }
    if let Some(realm_id) = realm_id {
        object.insert(
            "realm_id".to_owned(),
            Value::String(realm_id.as_str().to_owned()),
        );
    }
    if let Some(strand_id) = strand_id {
        object.insert(
            "strand_id".to_owned(),
            Value::String(strand_id.as_str().to_owned()),
        );
    }
    object.insert(
        "mls_group_id".to_owned(),
        Value::String(binding.mls_group_id.clone()),
    );
    object.insert(
        "epoch".to_owned(),
        Value::Number(serde_json::Number::from(binding.epoch)),
    );
    Value::Object(object)
}

fn optional_realm_id(value: Option<&str>) -> anyhow::Result<Option<RealmId>> {
    value
        .map(|value| RealmId::new(value.to_owned()).map_err(anyhow::Error::from))
        .transpose()
}

fn optional_strand_id(value: Option<&str>) -> anyhow::Result<Option<StrandId>> {
    value
        .map(|value| StrandId::new(value.to_owned()).map_err(anyhow::Error::from))
        .transpose()
}

fn sign_account_key_operation(
    account: &CokretAccountConfig,
    context: &str,
    value: &Value,
) -> anyhow::Result<cokret::KeyOperationSignature> {
    let key_ref = account
        .key_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cokret account '{}' missing keyRef", account.id))?;
    let verification_method = account.verification_method.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret account '{}' missing authorized verificationMethod",
            account.id
        )
    })?;
    sign_key_operation_value(key_ref, verification_method, context, value)
}

async fn remember_account_event(
    account_store: &FileCokretAccountStore,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    event_id: &str,
) -> bool {
    match account_store.remember_event_id_str(event_id).await {
        Ok(is_new) => is_new,
        Err(err) => {
            warn!(
                "cokret: account '{}/{}' durable event cache failed for event '{}': {err}; dispatching without local dedupe fallback",
                channel.id, account.id, event_id
            );
            true
        }
    }
}

async fn handle_sync_updates_for_account(
    client: &CokretHttpClient,
    updates: cokret::SyncUpdates,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    let to_device_lost = updates.to_device_lost;
    let saw_to_device_messages = !updates.to_device.is_empty();
    let to_device_ack_token = updates.to_device_ack_token.clone();
    let to_device_limited = updates.to_device_limited;
    let to_device_next_cursor = updates.to_device_next_cursor.clone();
    for message in &updates.to_device {
        record_account_mls_welcome_from_value_tree(
            crypto_store,
            &message.content,
            channel,
            account,
            "to_device",
        );
    }
    for item in &updates.account_data {
        record_account_mls_welcome_from_value_tree(
            crypto_store,
            &item.content,
            channel,
            account,
            "account_data",
        );
    }
    for update in updates.realm_updates {
        let scan_request = account_scan_catchup_request_for_update(&update);
        record_account_mls_welcomes_from_realm_update(&update, crypto_store, channel, account);
        let parsed = parse_realm_update_for_account(update, account);
        handle_parsed_account_events(
            client,
            parsed,
            channel,
            account,
            account_store,
            crypto_store,
            gateway_channel,
            session_store,
        )
        .await;
        if let Some(scan_request) = scan_request {
            scan_limited_realm_timeline_for_account(
                client,
                scan_request,
                channel,
                account,
                account_store,
                crypto_store,
                gateway_channel,
                session_store,
            )
            .await;
        }
    }
    for notification in updates.notifications {
        let notification_value = notification_delta_to_value(notification);
        record_account_mls_welcome_from_value_tree(
            crypto_store,
            &notification_value,
            channel,
            account,
            "notification",
        );
        let parsed = parse_notification_delta_for_account(&notification_value, account);
        handle_parsed_account_events(
            client,
            parsed,
            channel,
            account,
            account_store,
            crypto_store,
            gateway_channel,
            session_store,
        )
        .await;
    }
    match account_to_device_ack_plan(
        saw_to_device_messages,
        to_device_lost,
        to_device_ack_token.as_deref(),
        to_device_limited,
        to_device_next_cursor.as_deref(),
    ) {
        AccountToDeviceAckPlan::None => {}
        AccountToDeviceAckPlan::Pull(pull) => {
            if pull.reason == "to_device_lost" {
                warn!(
                    account_id = %account.id,
                    "cokret: account sync reported lost to-device messages; pulling standard device_messages queue"
                );
            }
            drain_account_device_messages_from_cursor(
                client,
                channel,
                account,
                account_store,
                crypto_store,
                pull.initial_cursor,
                pull.reason,
            )
            .await;
        }
        AccountToDeviceAckPlan::Ack {
            ack_token,
            followup,
        } => {
            if !ack_account_device_messages(
                client,
                channel,
                account,
                ack_token.as_str(),
                "to_device_sync",
            )
            .await
            {
                drain_account_device_messages(
                    client,
                    channel,
                    account,
                    account_store,
                    crypto_store,
                    "to_device_sync_ack_fallback",
                )
                .await;
                return;
            }
            if let Some(pull) = followup {
                if pull.initial_cursor.is_none() {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        "cokret: account subscribe to-device batch was limited without next_cursor; falling back to stored device_messages cursor"
                    );
                }
                drain_account_device_messages_from_cursor(
                    client,
                    channel,
                    account,
                    account_store,
                    crypto_store,
                    pull.initial_cursor,
                    pull.reason,
                )
                .await;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountDeviceMessagesPull {
    initial_cursor: Option<String>,
    reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AccountToDeviceAckPlan {
    None,
    Pull(AccountDeviceMessagesPull),
    Ack {
        ack_token: String,
        followup: Option<AccountDeviceMessagesPull>,
    },
}

fn account_to_device_ack_plan(
    saw_to_device_messages: bool,
    to_device_lost: bool,
    to_device_ack_token: Option<&str>,
    to_device_limited: bool,
    to_device_next_cursor: Option<&str>,
) -> AccountToDeviceAckPlan {
    if to_device_lost {
        return AccountToDeviceAckPlan::Pull(AccountDeviceMessagesPull {
            initial_cursor: None,
            reason: "to_device_lost",
        });
    }
    if !saw_to_device_messages {
        return AccountToDeviceAckPlan::None;
    }
    let Some(ack_token) = to_device_ack_token else {
        return AccountToDeviceAckPlan::Pull(AccountDeviceMessagesPull {
            initial_cursor: None,
            reason: "to_device_sync_missing_ack_token",
        });
    };
    AccountToDeviceAckPlan::Ack {
        ack_token: ack_token.to_owned(),
        followup: to_device_limited.then(|| AccountDeviceMessagesPull {
            initial_cursor: to_device_next_cursor.map(str::to_owned),
            reason: "to_device_sync_limited",
        }),
    }
}

async fn drain_account_device_messages(
    client: &CokretHttpClient,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    reason: &'static str,
) {
    drain_account_device_messages_from_cursor(
        client,
        channel,
        account,
        account_store,
        crypto_store,
        None,
        reason,
    )
    .await;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountScanCatchupRequest {
    realm_id: cokret::RealmId,
    before: Option<String>,
}

#[derive(Debug)]
struct AccountScanCatchupOutcome {
    events: Vec<cokret::Event>,
    limited: bool,
    pages: usize,
}

fn account_scan_catchup_request_for_update(
    update: &cokret::RealmUpdate,
) -> Option<AccountScanCatchupRequest> {
    let timeline = update.timeline.as_ref()?;
    if !timeline.limited {
        return None;
    }
    Some(AccountScanCatchupRequest {
        realm_id: update.realm_id.clone(),
        before: timeline.prev_cursor.clone(),
    })
}

async fn collect_account_scan_catchup<F, Fut>(
    request: AccountScanCatchupRequest,
    mut fetch: F,
) -> anyhow::Result<AccountScanCatchupOutcome>
where
    F: FnMut(cokret::RealmId, Option<String>, u32) -> Fut,
    Fut: Future<Output = anyhow::Result<cokret::SyncBackfillOutcome>>,
{
    let mut before = request.before;
    let mut events = Vec::new();
    let mut pages = 0;
    let mut limited = false;

    for _ in 0..ACCOUNT_SCAN_CATCHUP_MAX_PAGES {
        pages += 1;
        let outcome = fetch(
            request.realm_id.clone(),
            before.clone(),
            ACCOUNT_SCAN_CATCHUP_LIMIT,
        )
        .await?;
        let next_before = outcome.prev_cursor.clone();
        limited = outcome.limited;
        events.extend(outcome.events);
        match (limited, next_before) {
            (true, Some(cursor)) => before = Some(cursor),
            _ => break,
        }
    }

    Ok(AccountScanCatchupOutcome {
        events,
        limited,
        pages,
    })
}

async fn scan_limited_realm_timeline_for_account(
    client: &CokretHttpClient,
    request: AccountScanCatchupRequest,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    if !account.has_requested_scope("ck.self.events.query.scan") {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            realm_id = %request.realm_id.as_str(),
            "cokret: limited account timeline cannot be scan-backfilled without ck.self.events.query.scan"
        );
        return;
    }

    let realm_id = request.realm_id.clone();
    let outcome =
        match collect_account_scan_catchup(request, |realm_id, before, limit| async move {
            client
                .inner()
                .events_query(
                    realm_id.as_str(),
                    before.as_deref(),
                    None,
                    None,
                    Some(limit),
                )
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    realm_id = %realm_id.as_str(),
                    "cokret: scan catch-up failed for limited account timeline: {err:#}"
                );
                return;
            }
        };

    for event in &outcome.events {
        if let Ok(value) = serde_json::to_value(event) {
            record_account_mls_welcome_from_value_tree(
                crypto_store,
                &value,
                channel,
                account,
                "realm_scan_catchup",
            );
        }
    }
    let parsed = parse_backfill_events_for_account(&realm_id, outcome.events, account);
    handle_parsed_account_events(
        client,
        parsed,
        channel,
        account,
        account_store,
        crypto_store,
        gateway_channel,
        session_store,
    )
    .await;

    if outcome.limited {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            realm_id = %realm_id.as_str(),
            pages = outcome.pages,
            "cokret: scan catch-up stopped before exhausting limited account timeline"
        );
    }
}

fn parse_realm_update_for_account(
    update: cokret::RealmUpdate,
    account: &CokretAccountConfig,
) -> CokretInboundParseResult {
    let Some(timeline) = update.timeline else {
        return CokretInboundParseResult::default();
    };
    let mut realms = serde_json::Map::new();
    realms.insert(
        update.realm_id.as_str().to_owned(),
        json!({ "timeline": timeline }),
    );
    parse_delta_frame_for_account(&Value::Object(realms), account)
}

fn parse_backfill_events_for_account(
    realm_id: &cokret::RealmId,
    events: Vec<cokret::Event>,
    account: &CokretAccountConfig,
) -> CokretInboundParseResult {
    let events = events
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();
    let update = cokret::RealmUpdate {
        realm_id: realm_id.clone(),
        timeline: Some(cokret::SyncTimeline {
            events,
            limited: false,
            prev_cursor: None,
        }),
        state: Vec::new(),
        summary: Value::Null,
    };
    parse_realm_update_for_account(update, account)
}

fn record_account_mls_welcomes_from_realm_update(
    update: &cokret::RealmUpdate,
    crypto_store: &FileCokretCryptoStore,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
) -> usize {
    let mut recorded = 0;
    if let Some(timeline) = &update.timeline {
        for event in &timeline.events {
            recorded += record_account_mls_welcome_from_value_tree(
                crypto_store,
                event,
                channel,
                account,
                "realm_timeline",
            );
        }
    }
    for state_event in &update.state {
        recorded += record_account_mls_welcome_from_value_tree(
            crypto_store,
            state_event,
            channel,
            account,
            "realm_state",
        );
    }
    recorded
}

fn record_account_mls_welcome_from_value_tree(
    crypto_store: &FileCokretCryptoStore,
    value: &Value,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    source: &'static str,
) -> usize {
    record_account_mls_welcome_from_value_tree_inner(
        crypto_store,
        value,
        channel,
        account,
        source,
        6,
    )
}

fn record_account_mls_welcome_from_value_tree_inner(
    crypto_store: &FileCokretCryptoStore,
    value: &Value,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    source: &'static str,
    remaining_depth: usize,
) -> usize {
    match crypto_store.record_mls_welcome_from_value(value) {
        Ok(Some(welcome)) => {
            debug!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                group_id = %welcome.group_id,
                epoch = welcome.epoch,
                recipient_principal_id = %welcome.recipient_principal_id.as_str(),
                recipient_device_id = %welcome.recipient_device_id.as_str(),
                "cokret: recorded MLS Welcome from account inbound event"
            );
            return 1;
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                "cokret: failed to persist MLS Welcome from account inbound event: {err:#}"
            );
            return 1;
        }
    }
    if remaining_depth == 0 {
        return 0;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| {
                record_account_mls_welcome_from_value_tree_inner(
                    crypto_store,
                    item,
                    channel,
                    account,
                    source,
                    remaining_depth - 1,
                )
            })
            .sum(),
        Value::Object(object) => object
            .values()
            .map(|item| {
                record_account_mls_welcome_from_value_tree_inner(
                    crypto_store,
                    item,
                    channel,
                    account,
                    source,
                    remaining_depth - 1,
                )
            })
            .sum(),
        _ => 0,
    }
}

fn notification_delta_to_value(notification: cokret::NotificationDelta) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "notification_id".to_owned(),
        Value::String(notification.id.clone()),
    );
    object.insert("id".to_owned(), Value::String(notification.id));
    object.insert(
        "notification_type".to_owned(),
        Value::String(notification.notification_type.clone()),
    );
    object.insert(
        "type".to_owned(),
        Value::String(notification.notification_type),
    );
    object.insert("action".to_owned(), Value::String(notification.action));
    if let Some(data) = notification.data {
        if let Value::Object(data_object) = &data {
            for (key, value) in data_object {
                object.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        object.insert("data".to_owned(), data);
    }
    Value::Array(vec![Value::Object(object)])
}

async fn handle_parsed_account_events(
    client: &CokretHttpClient,
    parsed: CokretInboundParseResult,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    account_store: &FileCokretAccountStore,
    crypto_store: &FileCokretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    for skipped in parsed.skipped {
        match skipped.reason {
            CokretInboundSkipReason::EncryptedContent => {
                let decrypted = try_handle_encrypted_account_skip(
                    client,
                    &skipped,
                    crypto_store,
                    channel,
                    account,
                    gateway_channel,
                    session_store,
                )
                .await;
                if decrypted {
                    continue;
                }
                warn!(
                    account_id = %skipped.account_id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    realm_id = skipped.realm_id.as_deref().unwrap_or("<unknown>"),
                    "cokret: encrypted account message skipped; crypto session decrypt is not wired"
                );
            }
            reason => {
                debug!(
                    account_id = %skipped.account_id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    realm_id = skipped.realm_id.as_deref().unwrap_or("<unknown>"),
                    ?reason,
                    "cokret: account event skipped"
                );
            }
        }
    }
    for event in parsed.events {
        if !remember_account_event(account_store, channel, account, &event.event_id).await {
            continue;
        }
        dispatch_to_agent(
            event,
            channel,
            account,
            Arc::clone(gateway_channel),
            Arc::clone(session_store),
        )
        .await;
    }
}

async fn dispatch_to_agent(
    event: CokretInboundEvent,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let config_id = channel.id.clone();
    let sender = event.sender_did.clone();
    let realm_id = event.realm_id.clone();
    let flow_id = event.flow_id.clone();
    let thread_id = event.thread_root_id.clone();
    let body = event.body;
    // DIDs carry no external-bot localpart convention, but we can at least mark
    // the account's own DID as SelfBot so the runtime never replies to its own
    // echoed messages.
    let sender_kind = if sender.eq_ignore_ascii_case(account.principal_id.trim()) {
        runtime::SenderKind::SelfBot
    } else {
        runtime::SenderKind::Human
    };

    tokio::spawn(async move {
        runtime::spawn_start_thread_pipeline_with_meta_coordinated(
            gateway_channel,
            session_store,
            "cokret",
            realm_id.clone(),
            body,
            Some(sender.clone()),
            Some(runtime::StartThreadMeta {
                peer_id: Some(sender),
                group_id: Some(realm_id),
                thread_id,
                reply_target: flow_id,
                chat_type: Some("group".to_owned()),
                saved_channel_config_id: Some(config_id),
                sender_kind,
                ..runtime::StartThreadMeta::default()
            }),
        )
        .await;
    });
}

async fn try_handle_encrypted_account_skip(
    client: &CokretHttpClient,
    skipped: &CokretInboundSkippedEvent,
    crypto_store: &FileCokretCryptoStore,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> bool {
    let Some(payload) = skipped.encrypted_payload.as_ref() else {
        return false;
    };
    if !account_allows_event_read(account) {
        warn!(
            account_id = %account.id,
            event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
            "cokret: encrypted account event skipped because ck.event.read is not granted"
        );
        return false;
    }
    match crypto_store.plan_bootstrap_for_payload(
        &account.principal_id,
        &account.device_id,
        payload,
    ) {
        Ok(plan) => debug!(
            account_id = %account.id,
            group_id = %plan.group_id,
            required_epoch = plan.required_epoch,
            local_epoch = ?plan.local_epoch,
            action = ?plan.action,
            "cokret: planned crypto bootstrap for encrypted account event"
        ),
        Err(err) => warn!(
            account_id = %account.id,
            "cokret: failed to plan crypto bootstrap for encrypted account event: {err:#}"
        ),
    }

    match crypto_store.try_decrypt_content_block_detailed(payload) {
        Ok(CokretDecryptDetailedOutcome::Decrypted {
            content,
            consume_bindings,
        }) => {
            consume_account_mls_key_packages(
                client,
                channel,
                account,
                crypto_store,
                &consume_bindings,
            )
            .await;
            let Some(body) = decrypted_text_body(&content) else {
                warn!(
                    account_id = %account.id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    "cokret: decrypted encrypted account event but content is not displayable text"
                );
                return false;
            };
            let Some(event_id) = skipped.event_id.clone() else {
                return false;
            };
            let Some(realm_id) = skipped.realm_id.clone() else {
                return false;
            };
            let Some(sender_did) = skipped.sender_did.clone() else {
                return false;
            };
            dispatch_to_agent(
                CokretInboundEvent {
                    account_id: skipped.account_id.clone(),
                    event_id,
                    realm_id,
                    flow_id: None,
                    sender_did,
                    body,
                    thread_root_id: None,
                },
                channel,
                account,
                Arc::clone(gateway_channel),
                Arc::clone(session_store),
            )
            .await;
            true
        }
        Ok(CokretDecryptDetailedOutcome::MissingGroupState) => {
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                cokret::crypto_protocol::UnableToDecryptReason::NoSession,
            );
            false
        }
        Ok(CokretDecryptDetailedOutcome::UnsupportedScheme(scheme)) => {
            warn!(
                account_id = %account.id,
                event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                scheme,
                "cokret: encrypted account event uses unsupported encrypted payload scheme"
            );
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                cokret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            false
        }
        Err(err) => {
            warn!(
                account_id = %account.id,
                event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                "cokret: failed to decrypt encrypted account event: {err:#}"
            );
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                cokret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            false
        }
    }
}

fn record_account_unable_to_decrypt(
    crypto_store: &FileCokretCryptoStore,
    skipped: &CokretInboundSkippedEvent,
    payload: cokret::EncryptedPayload,
    reason: cokret::crypto_protocol::UnableToDecryptReason,
) {
    let (Some(event_id), Some(realm_id), Some(sender)) = (
        skipped.event_id.as_deref(),
        skipped.realm_id.as_deref(),
        skipped.sender_did.as_deref(),
    ) else {
        return;
    };
    if let Err(err) =
        crypto_store.record_unable_to_decrypt(event_id, realm_id, sender, payload, reason)
    {
        warn!(
            event_id,
            realm_id, "cokret: failed to persist unable-to-decrypt record: {err:#}"
        );
    }
}

fn decrypted_text_body(content: &Value) -> Option<String> {
    let block = content
        .get("content")
        .filter(|inner| inner.get("kind").is_some())
        .unwrap_or(content);
    let kind = block.get("kind").and_then(Value::as_str)?;
    if kind != "ck.content.text" {
        return None;
    }
    block
        .get("body")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(str::to_owned)
}

async fn sleep_with_backoff(backoff: &mut Duration) {
    tokio::time::sleep(*backoff).await;
    let next = (backoff.as_secs() * 2).min(60);
    *backoff = Duration::from_secs(next.max(1));
}

/// Build an authenticated DPoP-bound `CokretHttpClient` for one agent runtime.
async fn construct_account_client(
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
) -> anyhow::Result<(CokretHttpClient, Option<DateTime<Utc>>)> {
    let key_ref = account
        .key_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cokret agent '{}' missing keyRef", account.id))?;
    let verification_method = account.verification_method.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret agent '{}' missing authorized verificationMethod",
            account.id
        )
    })?;
    let authorization_ref = account.authorized_event_ref.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Cokret agent '{}' missing authorizedEventRef", account.id)
    })?;
    let audience = account
        .cokret_server_did
        .clone()
        .or_else(|| channel.service_did.clone())
        .or_else(|| {
            account
                .inkson_bootstrap
                .as_ref()
                .map(|bootstrap| bootstrap.service_did.to_string())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cokret agent '{}' missing serviceDid/cokretServerDid for agent_key_proof audience",
                account.id
            )
        })?;
    let principal = Did::new(account.principal_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid principal_id: {err}"))?;
    let (client, session) = CokretHttpClient::login_agent(
        &channel.base_url,
        key_ref,
        principal,
        verification_method,
        authorization_ref,
        account.requested_scope.clone(),
        &audience,
        None,
    )
    .await?;
    info!(
        "cokret: agent '{}' obtained DPoP-bound session; audience='{}' expires_at='{}'",
        account.id, audience, session.expires_at
    );
    Ok((client, Some(session.expires_at)))
}

/// Build the restart-safe monotonic `actor_seq` allocator for an outbound
/// account, mirroring the applet allocator. The backing file store lives under
/// `{savfox_home}/gateway/cokret-account-seq/{account_id}.seq`, keyed
/// `account:{account_id}:actor_seq`, so each account has an independent
/// monotonic counter that survives restarts (the previous `timestamp_millis()`
/// source was neither monotonic across rapid sends nor restart-safe).
fn build_account_seq_allocator(
    savfox_home: &std::path::Path,
    account_id: &str,
) -> anyhow::Result<cokret_bridge_runtime::SeqAllocator> {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("cokret-account-seq");
    let safe_id: String = account_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe_id}.seq"));
    let store = savfox_channels::cokret::FileSeqStore::shared(path)
        .map_err(|e| anyhow::anyhow!("cokret account seq store: {e}"))?;
    Ok(cokret_bridge_runtime::SeqAllocator::new(
        store,
        format!("account:{account_id}:actor_seq"),
    ))
}

/// Send a `ck.message.create` event as one of the channel's configured
/// outbound accounts.
///
/// `realm_id` selects the outbound Cokret realm. `flow_id` must come from the
/// inbound Cokret context that triggered the reply; it is not configured on the
/// channel.
pub(crate) async fn send_to_cokret_account(
    savfox_home: &std::path::PathBuf,
    realm_id: &str,
    flow_id: Option<&str>,
    body: &str,
) -> anyhow::Result<()> {
    let Some((channel, account)) = resolve_cokret_outbound_account(savfox_home, realm_id).await?
    else {
        anyhow::bail!("no Cokret channel configured for realm {realm_id}");
    };
    if !account.has_requested_scope("ck.self.events.command.submit") {
        anyhow::bail!(
            "Cokret account '{}' send=true but missing service scope ck.self.events.command.submit; refusing to call submit endpoint",
            account.id
        );
    }
    let flow = flow_id.map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret account '{}' cannot send without an inbound Cokret flow id",
            account.id
        )
    })?;

    // One-shot send uses the same agent_key_proof + DPoP session exchange as
    // the listener. The short grant is not cached on this path.
    let (client, _session_expiry) = construct_account_client(&channel, &account).await?;
    // Monotonic, restart-safe actor sequence (parity with the applet path).
    let actor_seq = build_account_seq_allocator(savfox_home, &account.id)?
        .alloc()
        .map_err(|e| anyhow::anyhow!("cokret account seq alloc: {e}"))?;
    let request = MessageCreateRequest {
        realm_id: realm_id.to_owned(),
        flow_id: flow,
        body: body.to_owned(),
        principal_id: account.principal_id.clone(),
        actor_seq,
        thread_root_id: None,
    };
    let mut event = build_message_create_event(&request)?;
    let crypto_store = FileCokretCryptoStore::for_account(savfox_home, &channel.id, &account.id);
    apply_account_outbound_encryption(&crypto_store, realm_id, &mut event)?;

    // Phase 8 (T8.E): attach capability grant event_id when configured.
    if let Some(grant_path) = &account.grant_event_path {
        let grant =
            savfox_channels::cokret::load_and_verify_grant(grant_path, &account.principal_id, None)
                .await
                .with_context(|| {
                    format!(
                        "Cokret account '{}' failed to load capability grant {}",
                        account.id,
                        grant_path.display()
                    )
                })?;
        if !grant.covers_action("ck.message.create") {
            anyhow::bail!(
                "Cokret account '{}' capability grant {} does not cover ck.message.create",
                account.id,
                grant_path.display()
            );
        }
        event.authorization_ref = Some(grant.event_id);
    }

    // Phase 8 (T8.C): sign with the account's ed25519 key when key_ref is set.
    if let Some(key_ref) = &account.key_ref {
        let vm = account
            .verification_method
            .clone()
            .unwrap_or_else(|| format!("{}#key-1", account.principal_id));
        let signer =
            savfox_channels::cokret::load_ed25519_signer(key_ref, &account.principal_id, &vm)?;
        savfox_channels::cokret::sign_outbound_event(&mut event, &signer, &vm)?;
    }

    let response = client.submit_event(&event).await?;
    // A transport-level 200 does not mean the event was accepted: the server
    // may reject it at the business layer (e.g. missing proofs, unsigned).
    // Surface that as an error so the caller can retry/alert instead of
    // silently treating a dropped event as delivered.
    if !response.rejected.is_empty() {
        anyhow::bail!(
            "cokret: server rejected event for realm '{realm_id}': {:?}",
            response.rejected
        );
    }
    if response.accepted.is_empty() && response.duplicate.is_empty() {
        anyhow::bail!(
            "cokret: server accepted no events for realm '{realm_id}' (status={:?})",
            response.status
        );
    }
    debug!(
        "cokret: submitted event to '{}': status={:?} accepted={} duplicate={}",
        realm_id,
        response.status,
        response.accepted.len(),
        response.duplicate.len()
    );
    Ok(())
}

fn apply_account_outbound_encryption(
    crypto_store: &FileCokretCryptoStore,
    realm_id: &str,
    event: &mut cokret::Event,
) -> anyhow::Result<()> {
    let Some(content_block) = event.payload.get("content").cloned() else {
        return Ok(());
    };
    match crypto_store.encrypt_content_block_for_realm(realm_id, &content_block)? {
        CokretEncryptOutcome::PlaintextAllowed => Ok(()),
        CokretEncryptOutcome::Encrypted(encrypted_content) => {
            let object = event
                .payload
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Cokret message content is not an object"))?;
            object.remove("content");
            object.insert("encrypted_content".to_owned(), encrypted_content);
            Ok(())
        }
        CokretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
            anyhow::bail!(
                "Cokret realm '{realm_id}' requires E2EE but no local MLS group state exists for group '{group_id}'"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn make_account() -> CokretAccountConfig {
        CokretAccountConfig {
            mode: savfox_channels::cokret::CokretAccountMode::Agent,
            id: "support".into(),
            principal_id: "did:webvh:z6mkfixture:agent.example".into(),
            device_id: "ck:device:01904100-0000-7000-8000-000000000001".into(),
            access_token: String::new(),
            key_ref: None,
            verification_method: None,
            cokret_server_did: None,
            login_challenge: None,
            grant_event_path: None,
            inkson_bootstrap: None,
            authorized_event_ref: None,
            requested_scope: vec![
                "ck.self.events.stream.subscribe".into(),
                "ck.self.events.query.scan".into(),
                "ck.event.read".into(),
            ],
            listen: true,
            send: true,
        }
    }

    fn realm_id() -> cokret::RealmId {
        cokret::RealmId::new("ck:realm:01904100-0000-7000-8000-000000000001").unwrap()
    }

    fn actor_id() -> Did {
        Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap()
    }

    fn message_event(body: &str) -> cokret::Event {
        message_event_with_seq(body, 1)
    }

    fn message_event_with_seq(body: &str, actor_seq: u64) -> cokret::Event {
        cokret::Event::new(
            "ck.message.create",
            realm_id(),
            actor_id(),
            actor_seq,
            cokret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            json!({
                "strand_id": "ck:strand:01904100-0000-7000-8000-000000000002",
                "track_name": "discussion",
                "content": {
                    "kind": "ck.content.text",
                    "body": body,
                    "thread_root_id": "ck:strand:01904100-0000-7000-8000-000000000003"
                }
            }),
        )
        .unwrap()
    }

    fn mls_welcome_value(group_id: &str) -> Value {
        serde_json::to_value(cokret::MlsWelcomeEnvelope {
            group_id: group_id.to_owned(),
            epoch: 7,
            recipient_principal_id: Did::new("did:webvh:z6mkfixture:bob.example".to_owned())
                .unwrap(),
            recipient_device_id: cokret::DeviceId::new(
                "ck:device:01904100-0000-7000-8000-00000000000e".to_owned(),
            )
            .unwrap(),
            welcome: "AA".to_owned(),
            welcome_hash: cokret::Hash::new(format!("sha256:{}", "ab".repeat(32))).unwrap(),
            ratchet_tree: None,
        })
        .unwrap()
    }

    #[test]
    fn session_grant_refreshes_when_expired_or_inside_skew() {
        let now = Utc.with_ymd_and_hms(2026, 7, 7, 12, 0, 0).unwrap();

        assert!(!session_grant_needs_refresh(None, now));
        assert!(!session_grant_needs_refresh(
            Some(now + chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS + 1)),
            now
        ));
        assert!(session_grant_needs_refresh(
            Some(now + chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS)),
            now
        ));
        assert!(session_grant_needs_refresh(
            Some(now - chrono::Duration::seconds(1)),
            now
        ));
    }

    #[test]
    fn session_refresh_delay_targets_skew_boundary() {
        let now = Utc.with_ymd_and_hms(2026, 7, 7, 12, 0, 0).unwrap();

        assert_eq!(session_refresh_delay(None, now), None);
        assert_eq!(
            session_refresh_delay(
                Some(now + chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS + 1)),
                now
            ),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            session_refresh_delay(
                Some(now + chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS)),
                now
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn sync_realm_delta_parses_dispatchable_messages() {
        let account = make_account();
        let realm_id = realm_id();
        let update = cokret::RealmUpdate {
            realm_id: realm_id.clone(),
            timeline: Some(cokret::SyncTimeline {
                events: vec![serde_json::to_value(message_event("hello from engine")).unwrap()],
                limited: false,
                prev_cursor: None,
            }),
            state: Vec::new(),
            summary: Value::Null,
        };

        let parsed = parse_realm_update_for_account(update, &account);

        assert_eq!(parsed.skipped, Vec::new());
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].account_id, account.id);
        assert_eq!(parsed.events[0].body, "hello from engine");
        assert_eq!(parsed.events[0].sender_did, actor_id().as_str());
        assert_eq!(
            parsed.events[0].flow_id.as_deref(),
            Some("ck:strand:01904100-0000-7000-8000-000000000002")
        );
    }

    #[test]
    fn sync_notification_flattens_data_for_existing_parser() {
        let account = make_account();
        let notification = cokret::NotificationDelta {
            id: "ck:notification:01904100-0000-7000-8000-000000000001".to_owned(),
            notification_type: "assignment".to_owned(),
            action: "add".to_owned(),
            data: Some(json!({
                "realm_id": realm_id().as_str(),
                "strand_id": "ck:strand:01904100-0000-7000-8000-000000000004",
                "source_event_id": "ck:event:01904100-0000-7000-8000-000000000005",
                "source_actor_id": actor_id().as_str(),
                "body": "Assigned to you"
            })),
        };

        let parsed = parse_notification_delta_for_account(
            &notification_delta_to_value(notification),
            &account,
        );

        assert_eq!(parsed.skipped, Vec::new());
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].realm_id, realm_id().as_str());
        assert_eq!(
            parsed.events[0].thread_root_id.as_deref(),
            Some("ck:strand:01904100-0000-7000-8000-000000000004")
        );
        assert!(parsed.events[0].body.contains("assignment notification"));
    }

    #[test]
    fn account_to_device_ack_plan_acks_direct_subscribe_batch() {
        assert_eq!(
            account_to_device_ack_plan(true, false, Some("ack-1"), false, None),
            AccountToDeviceAckPlan::Ack {
                ack_token: "ack-1".to_owned(),
                followup: None,
            }
        );
    }

    #[test]
    fn account_to_device_ack_plan_continues_limited_batch_from_next_cursor() {
        assert_eq!(
            account_to_device_ack_plan(
                true,
                false,
                Some("ack-1"),
                true,
                Some("ck:cursor:device-next")
            ),
            AccountToDeviceAckPlan::Ack {
                ack_token: "ack-1".to_owned(),
                followup: Some(AccountDeviceMessagesPull {
                    initial_cursor: Some("ck:cursor:device-next".to_owned()),
                    reason: "to_device_sync_limited",
                }),
            }
        );
    }

    #[test]
    fn account_to_device_ack_plan_falls_back_without_ack_token() {
        assert_eq!(
            account_to_device_ack_plan(true, false, None, false, None),
            AccountToDeviceAckPlan::Pull(AccountDeviceMessagesPull {
                initial_cursor: None,
                reason: "to_device_sync_missing_ack_token",
            })
        );
    }

    #[test]
    fn account_to_device_ack_plan_prioritizes_loss_recovery() {
        assert_eq!(
            account_to_device_ack_plan(true, true, Some("ack-1"), false, None),
            AccountToDeviceAckPlan::Pull(AccountDeviceMessagesPull {
                initial_cursor: None,
                reason: "to_device_lost",
            })
        );
    }

    #[test]
    fn limited_account_timeline_builds_scan_catchup_request() {
        let update = cokret::RealmUpdate {
            realm_id: realm_id(),
            timeline: Some(cokret::SyncTimeline {
                events: vec![serde_json::to_value(message_event("window head")).unwrap()],
                limited: true,
                prev_cursor: Some("ck:cursor:older-1".to_owned()),
            }),
            state: Vec::new(),
            summary: Value::Null,
        };

        let request = account_scan_catchup_request_for_update(&update).unwrap();

        assert_eq!(request.realm_id, realm_id());
        assert_eq!(request.before.as_deref(), Some("ck:cursor:older-1"));
    }

    #[tokio::test]
    async fn scan_catchup_pages_older_history_and_reuses_account_parser() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let responses =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from([
                cokret::SyncBackfillOutcome {
                    events: vec![message_event_with_seq("older one", 2)],
                    snapshot_bootstrap: None,
                    prev_cursor: Some("ck:cursor:older-2".to_owned()),
                    next_cursor: None,
                    limited: true,
                },
                cokret::SyncBackfillOutcome {
                    events: vec![message_event_with_seq("older two", 3)],
                    snapshot_bootstrap: None,
                    prev_cursor: None,
                    next_cursor: None,
                    limited: false,
                },
            ])));
        let requests_for_fetch = std::sync::Arc::clone(&requests);
        let responses_for_fetch = std::sync::Arc::clone(&responses);
        let request = AccountScanCatchupRequest {
            realm_id: realm_id(),
            before: Some("ck:cursor:older-1".to_owned()),
        };

        let outcome = collect_account_scan_catchup(request, move |realm_id, before, limit| {
            requests_for_fetch
                .lock()
                .unwrap()
                .push((realm_id.as_str().to_owned(), before, limit));
            let responses_for_fetch = std::sync::Arc::clone(&responses_for_fetch);
            async move {
                Ok::<_, anyhow::Error>(responses_for_fetch.lock().unwrap().pop_front().unwrap())
            }
        })
        .await
        .unwrap();

        assert!(!outcome.limited);
        assert_eq!(outcome.pages, 2);
        assert_eq!(
            requests.lock().unwrap().as_slice(),
            &[
                (
                    realm_id().as_str().to_owned(),
                    Some("ck:cursor:older-1".to_owned()),
                    ACCOUNT_SCAN_CATCHUP_LIMIT
                ),
                (
                    realm_id().as_str().to_owned(),
                    Some("ck:cursor:older-2".to_owned()),
                    ACCOUNT_SCAN_CATCHUP_LIMIT
                )
            ]
        );

        let parsed =
            parse_backfill_events_for_account(&realm_id(), outcome.events, &make_account());
        assert_eq!(
            parsed
                .events
                .iter()
                .map(|event| event.body.as_str())
                .collect::<Vec<_>>(),
            vec!["older one", "older two"]
        );
        assert_eq!(parsed.skipped, Vec::new());
    }

    #[test]
    fn account_realm_update_records_nested_mls_welcome() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let channel = CokretChannelConfig {
            id: "c1".to_owned(),
            base_url: "https://cokret.example".to_owned(),
            service_did: None,
            accounts: Vec::new(),
        };
        let account = make_account();
        let crypto_store = FileCokretCryptoStore::for_account(tmp.path(), &channel.id, &account.id);
        let group_id = "group-account-welcome";
        let update = cokret::RealmUpdate {
            realm_id: realm_id(),
            timeline: Some(cokret::SyncTimeline {
                events: vec![json!({
                    "kind": "ck.mls.welcome",
                    "event_id": "ck:event:01904100-0000-7000-8000-000000000007",
                    "realm_id": realm_id().as_str(),
                    "actor_id": actor_id().as_str(),
                    "payload": {
                        "kind": "ck.mls.welcome",
                        "content": mls_welcome_value(group_id)
                    }
                })],
                limited: false,
                prev_cursor: None,
            }),
            state: Vec::new(),
            summary: Value::Null,
        };

        let recorded = record_account_mls_welcomes_from_realm_update(
            &update,
            &crypto_store,
            &channel,
            &account,
        );

        assert_eq!(recorded, 1);
        let state = crypto_store.load().expect("crypto state should load");
        assert!(state.bootstrap.contains_key(group_id));
    }
}
