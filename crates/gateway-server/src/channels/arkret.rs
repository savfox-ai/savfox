//! Arkret personal-agent channel runtime.
//!
//! Owns one async task per (channel, account) pair. Each task:
//!
//! 1. Mints a short-lived `agent_key_proof` session grant bound to DPoP.
//! 2. Opens `/_arkret/self/account/subscribe` for the owning user account.
//! 3. Extracts dispatchable `ak.message.create` events.
//! 4. Dispatches each event to the agent pipeline.
//!
//! Outbound sends go through [`send_to_arkret_account`].

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use arkret::{
    DeviceId, DeviceMessagesAckRequestBody, Did, KeyPackageUploadEntry,
    KeyPackagesConsumeRequestBody, KeyPackagesUploadRequestBody, MlsKeyPackageRecord, RealmId,
    StrandId,
};
use chrono::Utc;
use garth::{
    ClientEvent, CursorStore, DurableInboxStore, EventCacheStore, OutboundEngine,
    OutboundEngineOutcome, OutboundSubmitOutcome, OutboundSubmitter, RunOptions, RunStopReason,
    SyncLoopControl, TransportProvider,
};
use savfox_channels::arkret::{
    ArkretAccountConfig, ArkretAgentSessionProvider, ArkretChannelConfig,
    ArkretDecryptDetailedOutcome, ArkretEncryptOutcome, ArkretHttpClient, ArkretInboundEvent,
    ArkretInboundParseResult, ArkretInboundSkipReason, ArkretInboundSkippedEvent,
    ArkretMlsWelcomeConsumeBinding, FileArkretCryptoStore, MessageCreateRequest,
    account_allows_event_read, build_message_create_event, device_messages_scope,
    open_account_store, parse_delta_frame_for_account, resolve_arkret_outbound_account,
    sign_key_operation_value,
};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use super::{ChannelRegistry, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// Per-(channel, account) runtime handles. Indexed by `{channel_id}::{account_id}`.
#[derive(Default)]
struct ArkretRuntimeState {
    handles: HashMap<String, tokio::task::JoinHandle<()>>,
    diagnostics: HashMap<String, ArkretListenerDiagnostic>,
}

#[derive(Debug, Clone)]
struct ArkretListenerDiagnostic {
    channel_id: String,
    account_id: String,
    principal_id: String,
    phase: &'static str,
    attempt: u64,
    last_error: Option<String>,
    last_event_id: Option<String>,
    last_realm_id: Option<String>,
    last_local_agent_id: Option<String>,
    received_events: u64,
    dispatched_events: u64,
    skipped_events: u64,
    updated_at: chrono::DateTime<Utc>,
}

impl ArkretListenerDiagnostic {
    fn new(channel: &ArkretChannelConfig, account: &ArkretAccountConfig) -> Self {
        Self {
            channel_id: channel.id.clone(),
            account_id: account.id.clone(),
            principal_id: account.principal_id.clone(),
            phase: "scheduled",
            attempt: 0,
            last_error: None,
            last_event_id: None,
            last_realm_id: None,
            last_local_agent_id: None,
            received_events: 0,
            dispatched_events: 0,
            skipped_events: 0,
            updated_at: Utc::now(),
        }
    }

    fn to_value(&self, running: bool) -> Value {
        json!({
            "channel_id": self.channel_id,
            "account_id": self.account_id,
            "principal_id": self.principal_id,
            "phase": self.phase,
            "running": running,
            "attempt": self.attempt,
            "last_error": self.last_error,
            "last_event_id": self.last_event_id,
            "last_realm_id": self.last_realm_id,
            "last_local_agent_id": self.last_local_agent_id,
            "received_events": self.received_events,
            "dispatched_events": self.dispatched_events,
            "skipped_events": self.skipped_events,
            "updated_at": self.updated_at,
        })
    }
}

const ACCOUNT_EVENT_DEDUPE_MAX: usize = 4096;
const ACCOUNT_SCAN_CATCHUP_LIMIT: u32 = 100;
const ACCOUNT_SCAN_CATCHUP_MAX_PAGES: usize = 64;
const DEVICE_MESSAGES_PULL_LIMIT: u32 = 100;
const DEVICE_MESSAGES_PULL_MAX_PAGES: usize = 16;

const KEYPACKAGES_UPLOAD_SCOPE: &str = "ak.self.keys.keypackages.upload.create";
const KEYPACKAGES_CONSUME_SCOPE: &str = "ak.self.keys.keypackages.command.consume";
const DEVICE_MESSAGES_LIST_SCOPE: &str = "ak.self.device_messages.query.list";
const DEVICE_MESSAGES_ACK_SCOPE: &str = "ak.self.device_messages.command.ack";

fn runtime_state() -> &'static StdMutex<ArkretRuntimeState> {
    static STATE: OnceLock<StdMutex<ArkretRuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| StdMutex::new(ArkretRuntimeState::default()))
}

fn task_key(channel_id: &str, account_id: &str) -> String {
    format!("{channel_id}::{account_id}")
}

fn update_listener_diagnostic(
    channel_id: &str,
    account_id: &str,
    update: impl FnOnce(&mut ArkretListenerDiagnostic),
) {
    let key = task_key(channel_id, account_id);
    let Ok(mut state) = runtime_state().lock() else {
        warn!(
            channel_id,
            account_id, "arkret: runtime state mutex poisoned while updating diagnostics"
        );
        return;
    };
    if let Some(diagnostic) = state.diagnostics.get_mut(&key) {
        update(diagnostic);
        diagnostic.updated_at = Utc::now();
    }
}

fn record_listener_failure(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    phase: &'static str,
    error: impl std::fmt::Display,
) {
    let error = error.to_string();
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.phase = phase;
        diagnostic.last_error = Some(error);
    });
}

fn record_listener_phase(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    phase: &'static str,
) {
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.phase = phase;
        diagnostic.last_error = None;
    });
}

pub(crate) fn arkret_account_runtime_diagnostics(channel_id: &str) -> Vec<Value> {
    let prefix = format!("{channel_id}::");
    let Ok(state) = runtime_state().lock() else {
        return Vec::new();
    };
    let mut values = state
        .diagnostics
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(key, diagnostic)| {
            let running = state
                .handles
                .get(key)
                .is_some_and(|handle| !handle.is_finished());
            diagnostic.to_value(running)
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        left.get("account_id")
            .and_then(Value::as_str)
            .cmp(&right.get("account_id").and_then(Value::as_str))
    });
    values
}

pub(crate) async fn start_arkret_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let arkret_config = ArkretChannelConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Arkret channel config must be an object"))?;
    arkret_config
        .validate()
        .with_context(|| format!("Arkret channel '{}' config validation failed", config.id))?;

    info!(
        "arkret: channel '{}' validated; {} account(s), base_url='{}'",
        arkret_config.id,
        arkret_config.accounts.len(),
        arkret_config.base_url,
    );

    for account in &arkret_config.accounts {
        if account.listen {
            spawn_account_listener(
                gateway_channel.config().savfox_home.clone(),
                arkret_config.clone(),
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
/// Called when an Arkret channel is disabled/deleted so the long-poll tasks
/// (and their `JoinHandle`s) don't leak and keep dispatching events for a
/// channel the operator already removed. Returns the number of tasks stopped.
pub(crate) fn stop_arkret_account_listeners(channel_id: &str) -> usize {
    let prefix = format!("{channel_id}::");
    let Ok(mut state) = runtime_state().lock() else {
        warn!("arkret: runtime state mutex poisoned; cannot stop listeners for '{channel_id}'");
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
        if let Some(diagnostic) = state.diagnostics.get_mut(&key) {
            diagnostic.phase = "stopped";
            diagnostic.updated_at = Utc::now();
        }
    }
    stopped
}

pub(crate) fn arkret_account_listener_count(channel_id: &str) -> usize {
    let prefix = format!("{channel_id}::");
    let Ok(state) = runtime_state().lock() else {
        warn!("arkret: runtime state mutex poisoned; cannot inspect listeners for '{channel_id}'");
        return 0;
    };
    state
        .handles
        .iter()
        .filter(|(key, handle)| {
            key.starts_with(&prefix)
                && !handle.is_finished()
                && state.diagnostics.get(*key).is_some_and(|diagnostic| {
                    matches!(diagnostic.phase, "subscribing" | "dispatching")
                })
        })
        .count()
}

fn spawn_account_listener(
    savfox_home: PathBuf,
    channel: ArkretChannelConfig,
    account: ArkretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let key = task_key(&channel.id, &account.id);
    if let Ok(mut state) = runtime_state().lock() {
        state.diagnostics.insert(
            key.clone(),
            ArkretListenerDiagnostic::new(&channel, &account),
        );
    }
    let diagnostic_channel_id = channel.id.clone();
    let diagnostic_account_id = account.id.clone();
    let handle = tokio::spawn(async move {
        let mut attempt = 0_u64;
        loop {
            attempt = attempt.saturating_add(1);
            update_listener_diagnostic(
                &diagnostic_channel_id,
                &diagnostic_account_id,
                |diagnostic| {
                    diagnostic.phase = "starting";
                    diagnostic.attempt = attempt;
                },
            );
            run_account_listener(
                savfox_home.clone(),
                channel.clone(),
                account.clone(),
                Arc::clone(&gateway_channel),
                Arc::clone(&session_store),
            )
            .await;
            let retry_delay = Duration::from_secs(attempt.min(6).pow(2));
            update_listener_diagnostic(
                &diagnostic_channel_id,
                &diagnostic_account_id,
                |diagnostic| diagnostic.phase = "retry_wait",
            );
            warn!(
                channel_id = %diagnostic_channel_id,
                account_id = %diagnostic_account_id,
                attempt,
                retry_delay_ms = retry_delay.as_millis(),
                "arkret: listener attempt ended; retrying instead of leaving a stale connected task"
            );
            tokio::time::sleep(retry_delay).await;
        }
    });
    let Ok(mut state) = runtime_state().lock() else {
        warn!("arkret: runtime state mutex poisoned; aborting listener task '{key}'");
        handle.abort();
        return;
    };
    if let Some(prev) = state.handles.insert(key, handle) {
        prev.abort();
    }
}

async fn run_account_listener(
    savfox_home: PathBuf,
    channel: ArkretChannelConfig,
    account: ArkretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    if !account.has_requested_scope("ak.self.events.stream.subscribe") {
        record_listener_failure(
            &channel,
            &account,
            "scope_rejected",
            "missing ak.self.events.stream.subscribe",
        );
        warn!(
            "arkret: account '{}' listen=true but missing ak.self.events.stream.subscribe; refusing to open subscribe endpoint",
            account.id
        );
        runtime::record_channel_probe("arkret", "error").await;
        return;
    }

    let account_store = match open_account_store(
        &savfox_home,
        &channel.id,
        &account.id,
        ACCOUNT_EVENT_DEDUPE_MAX,
    ) {
        Ok(store) => {
            if let Err(err) = store.ensure_created().await {
                let path = store
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "<unavailable>".to_owned());
                warn!(
                    "arkret: account '{}' durable subscribe state unavailable at {path}: {err}",
                    account.id
                );
                record_listener_failure(&channel, &account, "store_error", &err);
                runtime::record_channel_probe("arkret", "error").await;
                return;
            } else {
                store
            }
        }
        Err(err) => {
            warn!(
                "arkret: account '{}' failed to open durable subscribe state: {err}",
                account.id
            );
            record_listener_failure(&channel, &account, "store_error", &err);
            runtime::record_channel_probe("arkret", "error").await;
            return;
        }
    };
    let crypto_store = FileArkretCryptoStore::for_account(&savfox_home, &channel.id, &account.id);
    if let Err(err) =
        FileArkretCryptoStore::feature_report().and_then(|_| crypto_store.ensure_created())
    {
        warn!(
            "arkret: account '{}' crypto state unavailable at {}: {err:#}",
            account.id,
            crypto_store.path().display()
        );
    }

    let provider = match construct_account_provider(&channel, &account).await {
        Ok(provider) => provider,
        Err(err) => {
            warn!(
                "arkret: account '{}' on channel '{}' failed to construct session provider: {err:#}",
                account.id, channel.id
            );
            record_listener_failure(
                &channel,
                &account,
                "session_provider_error",
                format!("{err:#}"),
            );
            runtime::record_channel_probe("arkret", "error").await;
            return;
        }
    };
    let client = match provider.provide().await {
        Ok(client) => ArkretHttpClient::from_inner(client),
        Err(error) => {
            warn!(
                "arkret: account '{}' failed to build authenticated HTTP client: {error}",
                account.id
            );
            record_listener_failure(&channel, &account, "authentication_error", &error);
            runtime::record_channel_probe("arkret", "error").await;
            return;
        }
    };
    record_listener_phase(&channel, &account, "subscribing");
    runtime::record_channel_probe("arkret", "ok").await;
    run_account_key_lifecycle_maintenance(
        &client,
        &channel,
        &account,
        &account_store,
        &crypto_store,
        "startup",
    )
    .await;

    runtime::record_channel_probe("arkret", "ok").await;
    match drive_account_subscription_engine(
        &provider,
        &channel,
        &account,
        account_store,
        crypto_store,
        gateway_channel,
        session_store,
    )
    .await
    {
        AccountEngineOutcome::Unauthorized { reason } => {
            let detail = reason.as_deref().unwrap_or("unspecified");
            record_listener_failure(&channel, &account, "unauthorized", detail);
            warn!(
                account_id = %account.id,
                reason = detail,
                "arkret: shared session provider could not recover authorization"
            );
        }
        AccountEngineOutcome::Cancelled => {
            debug!(
                "arkret: account '{}' subscription engine stopped",
                account.id
            );
        }
        AccountEngineOutcome::Retry { error } => {
            record_listener_failure(&channel, &account, "subscribe_error", format!("{error:#}"));
            warn!(
                "arkret: subscribe engine for '{}/{}' failed: {error:#}",
                channel.id, account.id
            );
            runtime::record_channel_probe("arkret", "error").await;
        }
    }
}

#[derive(Debug)]
enum AccountEngineOutcome {
    Unauthorized { reason: Option<String> },
    Cancelled,
    Retry { error: anyhow::Error },
}

async fn drive_account_subscription_engine(
    provider: &ArkretAgentSessionProvider,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: garth::FileStore,
    crypto_store: FileArkretCryptoStore,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> AccountEngineOutcome {
    let actor_id = match Did::new(account.principal_id.clone()) {
        Ok(actor_id) => actor_id,
        Err(err) => {
            return AccountEngineOutcome::Retry {
                error: anyhow::anyhow!("invalid Arkret account principal id: {err}"),
            };
        }
    };
    let device_id = match DeviceId::new(account.device_id.clone()) {
        Ok(device_id) => device_id,
        Err(err) => {
            return AccountEngineOutcome::Retry {
                error: anyhow::anyhow!("invalid Arkret account device id: {err}"),
            };
        }
    };
    let service_id = match account_subscription_service_id(channel, account) {
        Ok(service_id) => service_id,
        Err(error) => return AccountEngineOutcome::Retry { error },
    };
    let client_core = garth::ArkretClient::new(
        garth::NativeExecutor,
        account_store.clone(),
        account_store.clone(),
    );
    let control = SyncLoopControl::new();
    let run = client_core.run_account_to_inbox(
        actor_id,
        device_id,
        service_id,
        provider,
        &control,
        RunOptions {
            beat: Duration::from_millis(250),
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            jitter_ratio: 0.2,
        },
    );
    tokio::pin!(run);
    let mut delivery_poll = tokio::time::interval(Duration::from_millis(50));
    delivery_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            result = &mut run => {
                process_durable_account_work(
                    provider,
                    channel,
                    account,
                    &account_store,
                    &crypto_store,
                    &gateway_channel,
                    &session_store,
                )
                .await;
                return account_engine_outcome_from_result(result);
            }
            _ = delivery_poll.tick() => {
                process_durable_account_work(
                    provider,
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
}

#[allow(clippy::too_many_arguments)]
async fn process_durable_account_work(
    provider: &ArkretAgentSessionProvider,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    let client = match provider.provide().await {
        Ok(client) => ArkretHttpClient::from_inner(client),
        Err(error) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: cannot drain durable inbox without an authenticated client: {error}"
            );
            return;
        }
    };
    process_durable_account_inbox(
        &client,
        channel,
        account,
        account_store,
        crypto_store,
        gateway_channel,
        session_store,
    )
    .await;
    drain_pending_account_outbound(&client, account_store, channel, account).await;
}

async fn drain_pending_account_outbound(
    client: &ArkretHttpClient,
    account_store: &garth::FileStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) {
    let outbound = OutboundEngine::new(account_store.clone());
    let submitter = AccountOutboundSubmitter {
        client: client.clone(),
    };
    loop {
        match outbound.submit_next(&submitter, Utc::now()).await {
            Ok(OutboundEngineOutcome::Accepted(item) | OutboundEngineOutcome::Duplicate(item)) => {
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    transaction_id = %item.transaction_id,
                    "arkret: durable outbound worker completed queued event"
                );
            }
            Ok(OutboundEngineOutcome::Rejected(item) | OutboundEngineOutcome::Terminal(item)) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    transaction_id = %item.transaction_id,
                    "arkret: durable outbound worker reached terminal event state"
                );
            }
            Ok(OutboundEngineOutcome::Idle | OutboundEngineOutcome::RetryAt { .. }) => return,
            Err(error) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    "arkret: durable outbound worker deferred after error: {error}"
                );
                return;
            }
        }
    }
}

/// Drain crash-safe deliveries committed atomically with the account cursor.
/// A process exit before `ack` leaves the batch pending for the next listener.
#[allow(clippy::too_many_arguments)]
async fn process_durable_account_inbox(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    loop {
        let deliveries = match account_store.pending(32).await {
            Ok(deliveries) => deliveries,
            Err(error) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    "arkret: failed to read durable account inbox: {error}"
                );
                return;
            }
        };
        if deliveries.is_empty() {
            return;
        }
        for delivery in deliveries {
            if delivery
                .next_attempt_at_ms
                .is_some_and(|next_at| next_at > Utc::now().timestamp_millis())
            {
                // Preserve delivery order; the poll timer will revisit this
                // head item after its persisted retry deadline.
                return;
            }
            let mut processing_error = None;
            for event in delivery.events {
                if let Err(error) = handle_account_client_event(
                    client,
                    event,
                    channel,
                    account,
                    account_store,
                    crypto_store,
                    gateway_channel,
                    session_store,
                )
                .await
                {
                    processing_error = Some(error);
                    break;
                }
            }
            if let Some(error) = processing_error {
                let delay_secs = 1_u64
                    .checked_shl(delivery.attempts.min(6))
                    .unwrap_or(60)
                    .min(60);
                let next_at = Utc::now() + chrono::Duration::seconds(delay_secs as i64);
                if let Err(store_error) = account_store
                    .retry(
                        delivery.id,
                        Some(next_at.timestamp_millis()),
                        garth::DeliveryErrorClass::Processing,
                        format!("{error:#}"),
                    )
                    .await
                {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        delivery_id = delivery.id.get(),
                        "arkret: failed to persist delivery retry: {store_error}"
                    );
                }
                return;
            }
            if let Err(error) = account_store.ack(delivery.id).await {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    delivery_id = delivery.id.get(),
                    "arkret: failed to acknowledge durable account delivery: {error}"
                );
                return;
            }
        }
    }
}

fn account_engine_outcome_from_result(
    result: garth::RunResult<RunStopReason>,
) -> AccountEngineOutcome {
    match result {
        Ok(RunStopReason::Cancelled | RunStopReason::LifecycleEnded) => {
            AccountEngineOutcome::Cancelled
        }
        Ok(RunStopReason::Unauthorized { reason }) => AccountEngineOutcome::Unauthorized { reason },
        Ok(RunStopReason::Failed { class }) => AccountEngineOutcome::Retry {
            error: anyhow::anyhow!("account subscription engine stopped: {class:?}"),
        },
        Err(err) => AccountEngineOutcome::Retry {
            error: anyhow::anyhow!("account subscription engine: {err}"),
        },
    }
}

async fn handle_account_client_event(
    client: &ArkretHttpClient,
    event: ClientEvent,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
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
            .await?;
        }
        other => {
            debug!(
                "arkret: account '{}/{}' ignored non-account subscription event: {:?}",
                channel.id, account.id, other
            );
        }
    }
    Ok(())
}

fn account_cursor_service_id(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> Option<String> {
    account
        .arkret_server_did
        .clone()
        .or_else(|| channel.service_id.clone())
        .or_else(|| {
            account
                .inkson_bootstrap
                .as_ref()
                .map(|bootstrap| bootstrap.service_id.to_string())
        })
}

fn account_subscription_service_id(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<Option<Did>> {
    account_cursor_service_id(channel, account)
        .map(|value| {
            Did::new(value.clone())
                .map_err(|err| anyhow::anyhow!("invalid Arkret service DID '{value}': {err}"))
        })
        .transpose()
}

async fn run_account_key_lifecycle_maintenance(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
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
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) {
    if !account.has_requested_scope(KEYPACKAGES_UPLOAD_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = KEYPACKAGES_UPLOAD_SCOPE,
            "arkret: cannot publish MLS KeyPackages without requested standard scope"
        );
        return;
    }
    let principal = match Did::new(account.principal_id.clone()) {
        Ok(principal) => principal,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: invalid principal id for MLS KeyPackage upload: {err}"
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
                "arkret: invalid device id for MLS KeyPackage upload: {err}"
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
                    "arkret: failed to ensure local MLS KeyPackage: {err:#}"
                );
                return;
            }
        }
    }

    let mut entries = match records
        .iter()
        .map(|record| key_package_upload_entry(record, None))
        .collect::<anyhow::Result<Vec<_>>>()
    {
        Ok(entries) => entries,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to encode MLS KeyPackage upload entry: {err:#}"
            );
            return;
        }
    };
    let signature_value = match key_package_upload_signature_value(&principal, &device, &entries) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to build MLS KeyPackage upload signature payload: {err:#}"
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
                    "arkret: failed to sign MLS KeyPackage upload request: {err:#}"
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
                    "arkret: MLS KeyPackage upload returned rejected entries"
                );
            } else {
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    accepted = outcome.accepted,
                    refs = outcome.key_package_refs.len(),
                    "arkret: published MLS KeyPackages"
                );
            }
        }
        Err(err) => warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: MLS KeyPackage upload failed: {err}"
        ),
    }
}

fn key_package_upload_entry(
    record: &MlsKeyPackageRecord,
    device_signature: Option<arkret::KeyOperationSignature>,
) -> anyhow::Result<KeyPackageUploadEntry> {
    Ok(KeyPackageUploadEntry {
        keypackage_id: record.keypackage_id.clone(),
        keypackage_ref: record.keypackage_ref.as_str().to_owned(),
        keypackage_digest: record.keypackage_ref.clone(),
        key_package: arkret::Base64UrlString::new(record.key_package.clone())
            .map_err(anyhow::Error::msg)
            .context("MLS KeyPackage must be unpadded base64url")?,
        cipher_suites: record.cipher_suites.clone(),
        capabilities: record.capabilities.clone(),
        expires_at: record
            .expires_at
            .unwrap_or_else(|| record.created_at + chrono::Duration::days(7)),
        created_at: record.created_at,
        device_signature,
        last_resort: Some(record.last_resort),
    })
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
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    initial_cursor: Option<String>,
    reason: &'static str,
) {
    if !account.has_requested_scope(DEVICE_MESSAGES_LIST_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = DEVICE_MESSAGES_LIST_SCOPE,
            reason,
            "arkret: cannot pull standard device messages without requested scope"
        );
        return;
    }
    let service_id = account_cursor_service_id(channel, account);
    let mut cursor = if let Some(cursor) = initial_cursor {
        Some(cursor)
    } else {
        let scope = device_messages_scope(
            service_id.as_deref(),
            &account.principal_id,
            &account.device_id,
        );
        match scope {
            Ok(scope) => match account_store.load(scope).await {
                Ok(cursor) => cursor,
                Err(err) => {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        reason,
                        "arkret: failed to load device-message cursor: {err}"
                    );
                    None
                }
            },
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    reason,
                    "arkret: invalid device-message cursor scope: {err}"
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
                    "arkret: standard device_messages pull failed: {err}"
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
                "arkret: device_messages reported cursor loss; clearing local cursor without ack"
            );
            let clear = device_messages_scope(
                service_id.as_deref(),
                &account.principal_id,
                &account.device_id,
            );
            if let Err(err) = match clear {
                Ok(scope) => account_store.clear(scope).await,
                Err(err) => Err(err),
            } {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    "arkret: failed to clear lost device-message cursor: {err}"
                );
            }
            return;
        }
        if !outcome.messages.is_empty() && outcome.ack_token.is_none() {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                "arkret: device_messages returned messages without ack_token; leaving cursor unchanged"
            );
            return;
        }
        if let Some(ack_token) = outcome.ack_token.as_deref()
            && !ack_account_device_messages(client, channel, account, ack_token, reason).await
        {
            return;
        }
        if let Some(next_cursor) = outcome.next_cursor {
            let save = device_messages_scope(
                service_id.as_deref(),
                &account.principal_id,
                &account.device_id,
            );
            if let Err(err) = match save {
                Ok(scope) => account_store.save(scope, next_cursor.clone()).await,
                Err(err) => Err(err),
            } {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    reason,
                    "arkret: failed to persist device-message cursor: {err}"
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
                "arkret: device_messages page was limited"
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
        "arkret: stopped device_messages pull after page cap"
    );
}

async fn ack_account_device_messages(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    ack_token: &str,
    reason: &'static str,
) -> bool {
    if !account.has_requested_scope(DEVICE_MESSAGES_ACK_SCOPE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            scope = DEVICE_MESSAGES_ACK_SCOPE,
            reason,
            "arkret: cannot ack standard device messages without requested scope"
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
                "arkret: device_messages ack returned ok=false"
            );
            false
        }
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                "arkret: device_messages ack failed: {err}"
            );
            false
        }
    }
}

async fn consume_account_mls_key_packages(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
    bindings: &[ArkretMlsWelcomeConsumeBinding],
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
            "arkret: cannot consume MLS KeyPackages without requested standard scope"
        );
        return;
    }
    let consumer_device = match DeviceId::new(account.device_id.clone()) {
        Ok(device) => device,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: invalid device id for MLS KeyPackage consume: {err}"
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
                    "arkret: invalid Realm id in MLS Welcome consume binding: {err}"
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
                    "arkret: invalid Strand id in MLS Welcome consume binding: {err}"
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
                        "arkret: failed to sign MLS KeyPackage consume request: {err:#}"
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
                        "arkret: failed to mark local MLS KeyPackage consumed after server ack: {err:#}"
                    );
                }
                if let Err(err) = crypto_store.mark_mls_welcome_consume_binding_acked(binding) {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        keypackage_ref = %binding.keypackage_ref,
                        "arkret: failed to clear MLS Welcome consume binding after server ack: {err:#}"
                    );
                }
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    consumed = outcome.consumed.len(),
                    "arkret: consumed MLS KeyPackage after Welcome decrypt"
                );
            }
            Ok(outcome) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    failures = ?outcome.failures,
                    "arkret: MLS KeyPackage consume returned failures"
                );
            }
            Err(err) => warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %binding.keypackage_ref,
                "arkret: MLS KeyPackage consume failed: {err}"
            ),
        }
    }
}

fn key_package_consume_signature_value(
    binding: &ArkretMlsWelcomeConsumeBinding,
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
    account: &ArkretAccountConfig,
    context: &str,
    value: &Value,
) -> anyhow::Result<arkret::KeyOperationSignature> {
    let key_ref = account
        .key_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Arkret account '{}' missing keyRef", account.id))?;
    let verification_method = account.verification_method.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret account '{}' missing authorized verificationMethod",
            account.id
        )
    })?;
    sign_key_operation_value(key_ref, verification_method, context, value)
}

async fn account_event_seen(
    account_store: &garth::FileStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    event_id: &str,
) -> anyhow::Result<bool> {
    let event_id = match arkret::EventId::new(event_id.to_owned()) {
        Ok(event_id) => event_id,
        Err(err) => {
            warn!(
                "arkret: account '{}/{}' rejected invalid event id '{}': {err}",
                channel.id, account.id, event_id
            );
            return Ok(true);
        }
    };
    match account_store.seen(event_id.clone()).await {
        Ok(seen) => Ok(seen),
        Err(err) => Err(anyhow::anyhow!(
            "arkret account '{}/{}' durable event cache read for '{}': {err}",
            channel.id,
            account.id,
            event_id
        )),
    }
}

async fn remember_account_event(
    account_store: &garth::FileStore,
    event_id: &str,
) -> anyhow::Result<()> {
    let event_id = arkret::EventId::new(event_id.to_owned())?;
    account_store.remember(event_id).await?;
    Ok(())
}

async fn handle_sync_updates_for_account(
    client: &ArkretHttpClient,
    updates: arkret::SyncUpdates,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
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
        .await?;
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
            .await?;
        }
    }
    // Typed account notifications carry Agent runtime-approval state. They are
    // not Realm events and must not wake the channel's chat agent.
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
                    "arkret: account sync reported lost to-device messages; pulling standard device_messages queue"
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
                return Ok(());
            }
            if let Some(pull) = followup {
                if pull.initial_cursor.is_none() {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        "arkret: account subscribe to-device batch was limited without next_cursor; falling back to stored device_messages cursor"
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
    Ok(())
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
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
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
    realm_id: arkret::RealmId,
    before: Option<String>,
}

#[derive(Debug)]
struct AccountScanCatchupOutcome {
    events: Vec<arkret::Event>,
    limited: bool,
    pages: usize,
}

fn account_scan_catchup_request_for_update(
    update: &arkret::RealmUpdate,
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
    F: FnMut(arkret::RealmId, Option<String>, u32) -> Fut,
    Fut: Future<Output = anyhow::Result<arkret::SyncBackfillOutcome>>,
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
    client: &ArkretHttpClient,
    request: AccountScanCatchupRequest,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    if !account.has_requested_scope("ak.self.events.query.scan") {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            realm_id = %request.realm_id.as_str(),
            "arkret: limited account timeline cannot be scan-backfilled without ak.self.events.query.scan"
        );
        return Ok(());
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
                    "arkret: scan catch-up failed for limited account timeline: {err:#}"
                );
                return Err(err);
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
    .await?;

    if outcome.limited {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            realm_id = %realm_id.as_str(),
            pages = outcome.pages,
            "arkret: scan catch-up stopped before exhausting limited account timeline"
        );
    }
    Ok(())
}

fn parse_realm_update_for_account(
    update: arkret::RealmUpdate,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let Some(timeline) = update.timeline else {
        return ArkretInboundParseResult::default();
    };
    let mut realms = serde_json::Map::new();
    realms.insert(
        update.realm_id.as_str().to_owned(),
        json!({ "timeline": timeline }),
    );
    parse_delta_frame_for_account(&Value::Object(realms), account)
}

fn parse_backfill_events_for_account(
    realm_id: &arkret::RealmId,
    events: Vec<arkret::Event>,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let events = events
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();
    let update = arkret::RealmUpdate {
        realm_id: realm_id.clone(),
        timeline: Some(arkret::SyncTimeline {
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
    update: &arkret::RealmUpdate,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
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
    crypto_store: &FileArkretCryptoStore,
    value: &Value,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
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
    crypto_store: &FileArkretCryptoStore,
    value: &Value,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
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
                "arkret: recorded MLS Welcome from account inbound event"
            );
            return 1;
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                "arkret: failed to persist MLS Welcome from account inbound event: {err:#}"
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

async fn handle_parsed_account_events(
    client: &ArkretHttpClient,
    parsed: ArkretInboundParseResult,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    for skipped in parsed.skipped {
        update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
            diagnostic.skipped_events = diagnostic.skipped_events.saturating_add(1);
            diagnostic.last_event_id = skipped.event_id.clone();
            diagnostic.last_realm_id = skipped.realm_id.clone();
        });
        match skipped.reason {
            ArkretInboundSkipReason::EncryptedContent => {
                if let Some(event_id) = skipped.event_id.as_deref()
                    && account_event_seen(account_store, channel, account, event_id).await?
                {
                    continue;
                }
                let decrypted = try_handle_encrypted_account_skip(
                    client,
                    &skipped,
                    crypto_store,
                    channel,
                    account,
                    gateway_channel,
                    session_store,
                )
                .await?;
                if decrypted {
                    if let Some(event_id) = skipped.event_id.as_deref() {
                        remember_account_event(account_store, event_id).await?;
                    }
                    continue;
                }
                warn!(
                    account_id = %skipped.account_id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    realm_id = skipped.realm_id.as_deref().unwrap_or("<unknown>"),
                    "arkret: encrypted account message skipped; crypto session decrypt is not wired"
                );
            }
            reason => {
                debug!(
                    account_id = %skipped.account_id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    realm_id = skipped.realm_id.as_deref().unwrap_or("<unknown>"),
                    ?reason,
                    "arkret: account event skipped"
                );
            }
        }
    }
    for event in parsed.events {
        update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
            diagnostic.received_events = diagnostic.received_events.saturating_add(1);
            diagnostic.last_event_id = Some(event.event_id.clone());
            diagnostic.last_realm_id = Some(event.realm_id.clone());
        });
        debug!(
            channel_id = %channel.id,
            account_id = %account.id,
            event_id = %event.event_id,
            realm_id = %event.realm_id,
            sender_did = %event.sender_did,
            mentioned_actor_ids = ?event.mentioned_actor_ids,
            "arkret: parsed dispatchable account event"
        );
        if account_event_seen(account_store, channel, account, &event.event_id).await? {
            debug!(
                channel_id = %channel.id,
                account_id = %account.id,
                event_id = %event.event_id,
                "arkret: event already acknowledged in durable dedupe store"
            );
            continue;
        }
        let event_id = event.event_id.clone();
        dispatch_to_agent(
            event,
            channel,
            account,
            Arc::clone(gateway_channel),
            Arc::clone(session_store),
        )
        .await?;
        remember_account_event(account_store, &event_id).await?;
    }
    Ok(())
}

async fn dispatch_to_agent(
    event: ArkretInboundEvent,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> anyhow::Result<()> {
    let config_id = channel.id.clone();
    let sender = event.sender_did.clone();
    let realm_id = event.realm_id.clone();
    let flow_id = event.flow_id.clone();
    let thread_id = event.thread_root_id.clone();
    let event_id = event.event_id.clone();
    let mentioned_actor_ids = event.mentioned_actor_ids.clone();
    let body = event.body;
    // DIDs carry no external-bot localpart convention, but we can at least mark
    // the account's own DID as SelfBot so the runtime never replies to its own
    // echoed messages.
    let sender_kind = if sender.eq_ignore_ascii_case(account.principal_id.trim()) {
        runtime::SenderKind::SelfBot
    } else {
        runtime::SenderKind::Human
    };

    let mut start_meta = runtime::StartThreadMeta {
        peer_id: Some(sender.clone()),
        group_id: Some(realm_id.clone()),
        thread_id,
        reply_target: flow_id,
        chat_type: Some("group".to_owned()),
        saved_channel_config_id: Some(config_id.clone()),
        sender_kind,
        ..runtime::StartThreadMeta::default()
    };
    let local_agent_id = runtime::resolve_start_thread_agent(
        &gateway_channel,
        &session_store,
        "arkret",
        &realm_id,
        Some(&sender),
        &start_meta,
    )
    .await;
    start_meta.forced_agent_id = Some(local_agent_id.clone());
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.phase = "dispatching";
        diagnostic.last_event_id = Some(event_id.clone());
        diagnostic.last_realm_id = Some(realm_id.clone());
        diagnostic.last_local_agent_id = Some(local_agent_id.clone());
    });
    info!(
        channel_id = %channel.id,
        account_id = %account.id,
        arkret_principal_id = %account.principal_id,
        event_id = %event_id,
        realm_id = %realm_id,
        local_agent_id = %local_agent_id,
        mentioned_actor_ids = ?mentioned_actor_ids,
        "arkret: dispatching inbound event to resolved Savfox agent"
    );

    let accepted = runtime::spawn_start_thread_pipeline_with_meta_coordinated(
        gateway_channel,
        session_store,
        "arkret",
        realm_id.clone(),
        body,
        Some(sender.clone()),
        Some(start_meta),
    )
    .await;
    anyhow::ensure!(
        accepted,
        "Arkret inbound task was not accepted by the coordinator"
    );
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.phase = "subscribing";
        diagnostic.dispatched_events = diagnostic.dispatched_events.saturating_add(1);
    });
    Ok(())
}

async fn try_handle_encrypted_account_skip(
    client: &ArkretHttpClient,
    skipped: &ArkretInboundSkippedEvent,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<bool> {
    let Some(payload) = skipped.encrypted_payload.as_ref() else {
        return Ok(false);
    };
    if !account_allows_event_read(account) {
        warn!(
            account_id = %account.id,
            event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
            "arkret: encrypted account event skipped because ak.event.read is not granted"
        );
        return Ok(false);
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
            "arkret: planned crypto bootstrap for encrypted account event"
        ),
        Err(err) => warn!(
            account_id = %account.id,
            "arkret: failed to plan crypto bootstrap for encrypted account event: {err:#}"
        ),
    }

    match crypto_store.try_decrypt_content_block_detailed(payload) {
        Ok(ArkretDecryptDetailedOutcome::Decrypted {
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
                    "arkret: decrypted encrypted account event but content is not displayable text"
                );
                return Ok(false);
            };
            let Some(event_id) = skipped.event_id.clone() else {
                return Ok(false);
            };
            let Some(realm_id) = skipped.realm_id.clone() else {
                return Ok(false);
            };
            let Some(sender_did) = skipped.sender_did.clone() else {
                return Ok(false);
            };
            dispatch_to_agent(
                ArkretInboundEvent {
                    account_id: skipped.account_id.clone(),
                    event_id,
                    realm_id,
                    flow_id: None,
                    sender_did,
                    body,
                    thread_root_id: None,
                    mentioned_actor_ids: Vec::new(),
                },
                channel,
                account,
                Arc::clone(gateway_channel),
                Arc::clone(session_store),
            )
            .await?;
            Ok(true)
        }
        Ok(ArkretDecryptDetailedOutcome::MissingGroupState) => {
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                arkret::crypto_protocol::UnableToDecryptReason::NoSession,
            );
            Ok(false)
        }
        Ok(ArkretDecryptDetailedOutcome::UnsupportedScheme(scheme)) => {
            warn!(
                account_id = %account.id,
                event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                scheme,
                "arkret: encrypted account event uses unsupported encrypted payload scheme"
            );
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                arkret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            Ok(false)
        }
        Err(err) => {
            warn!(
                account_id = %account.id,
                event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                "arkret: failed to decrypt encrypted account event: {err:#}"
            );
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                arkret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            Ok(false)
        }
    }
}

fn record_account_unable_to_decrypt(
    crypto_store: &FileArkretCryptoStore,
    skipped: &ArkretInboundSkippedEvent,
    payload: arkret::EncryptedPayload,
    reason: arkret::crypto_protocol::UnableToDecryptReason,
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
            realm_id, "arkret: failed to persist unable-to-decrypt record: {err:#}"
        );
    }
}

fn decrypted_text_body(content: &Value) -> Option<String> {
    let block = content
        .get("content")
        .filter(|inner| inner.get("kind").is_some())
        .unwrap_or(content);
    let kind = block.get("kind").and_then(Value::as_str)?;
    if kind != "ak.content.text" {
        return None;
    }
    block
        .get("body")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(str::to_owned)
}

/// Build a shared session-backed transport provider for one agent runtime.
async fn construct_account_provider(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<ArkretAgentSessionProvider> {
    let key_ref = account
        .key_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Arkret agent '{}' missing keyRef", account.id))?;
    let verification_method = account.verification_method.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret agent '{}' missing authorized verificationMethod",
            account.id
        )
    })?;
    let authorization_ref = account.authorized_event_ref.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Arkret agent '{}' missing authorizedEventRef", account.id)
    })?;
    let audience = account
        .arkret_server_did
        .clone()
        .or_else(|| channel.service_id.clone())
        .or_else(|| {
            account
                .inkson_bootstrap
                .as_ref()
                .map(|bootstrap| bootstrap.service_id.to_string())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret agent '{}' missing serviceId/arkretServerDid for agent_key_proof audience",
                account.id
            )
        })?;
    let principal = Did::new(account.principal_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid principal_id: {err}"))?;
    let device_id = DeviceId::new(account.device_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid Arkret device_id: {err}"))?;
    let runtime_public_key_digest =
        savfox_channels::arkret::ed25519_runtime_public_key_digest(key_ref, verification_method)?;
    info!(
        channel_id = %channel.id,
        account_id = %account.id,
        verification_method,
        runtime_public_key_digest,
        "arkret: constructing agent session provider"
    );
    let (provider, session) = ArkretHttpClient::login_agent_provider(
        &channel.base_url,
        key_ref,
        principal,
        verification_method,
        authorization_ref,
        account.requested_scope.clone(),
        &audience,
        Some(device_id),
        None,
    )
    .await?;
    info!(
        "arkret: agent '{}' obtained DPoP-bound session; audience='{}' expires_at='{}'",
        account.id, audience, session.expires_at
    );
    Ok(provider)
}

/// Obtain a current authenticated client for one-shot outbound operations.
async fn construct_account_client(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<ArkretHttpClient> {
    let provider = construct_account_provider(channel, account).await?;
    let inner = provider
        .provide()
        .await
        .map_err(|error| anyhow::anyhow!("build authenticated Arkret client: {error}"))?;
    Ok(ArkretHttpClient::from_inner(inner))
}

/// Build the restart-safe monotonic `actor_seq` allocator for an outbound
/// account, mirroring the applet allocator. The backing file store lives under
/// `{savfox_home}/gateway/arkret-account-seq/{account_id}.seq`, keyed
/// `account:{account_id}:actor_seq`, so each account has an independent
/// monotonic counter that survives restarts (the previous `timestamp_millis()`
/// source was neither monotonic across rapid sends nor restart-safe).
fn build_account_seq_allocator(
    savfox_home: &std::path::Path,
    account_id: &str,
) -> anyhow::Result<arkret_bridge_runtime::SeqAllocator> {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("arkret-account-seq");
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
    let store = arkret_bridge_runtime::FileSeqStore::shared(path)
        .map_err(|e| anyhow::anyhow!("arkret account seq store: {e}"))?;
    Ok(arkret_bridge_runtime::SeqAllocator::new(
        store,
        format!("account:{account_id}:actor_seq"),
    ))
}

/// Maps one durable Garth queue item onto the Arkret submit endpoint.
struct AccountOutboundSubmitter {
    client: ArkretHttpClient,
}

impl OutboundSubmitter for AccountOutboundSubmitter {
    fn submit<'a>(
        &'a self,
        item: arkret::sync_client::SendQueueItem,
    ) -> garth::outbound::BoxOutboundFuture<'a, OutboundSubmitOutcome> {
        Box::pin(async move {
            let event: arkret::Event = serde_json::from_value(item.content).map_err(|error| {
                arkret::Error::Protocol(format!("decode queued Arkret event: {error}"))
            })?;
            let response = match self.client.submit_event(&event).await {
                Ok(response) => response,
                Err(error) => {
                    return Ok(OutboundSubmitOutcome::RetryAfter {
                        delay: Duration::from_secs(1),
                        reason: error.to_string(),
                    });
                }
            };
            if let Some(event_id) = response.accepted.into_iter().next() {
                return Ok(OutboundSubmitOutcome::Accepted { event_id });
            }
            if let Some(event_id) = response.duplicate.into_iter().next() {
                return Ok(OutboundSubmitOutcome::Duplicate { event_id });
            }
            if !response.rejected.is_empty() {
                return Ok(OutboundSubmitOutcome::Rejected {
                    reason: format!("{:?}", response.rejected),
                });
            }
            Ok(OutboundSubmitOutcome::Terminal {
                reason: format!("server accepted no events (status={:?})", response.status),
            })
        })
    }
}

/// Send a `ak.message.create` event as one of the channel's configured
/// outbound accounts.
///
/// `realm_id` selects the outbound Arkret realm. `flow_id` must come from the
/// inbound Arkret context that triggered the reply; it is not configured on the
/// channel.
pub(crate) async fn send_to_arkret_account(
    savfox_home: &std::path::PathBuf,
    realm_id: &str,
    flow_id: Option<&str>,
    body: &str,
) -> anyhow::Result<()> {
    let Some((channel, account)) = resolve_arkret_outbound_account(savfox_home, realm_id).await?
    else {
        anyhow::bail!("no Arkret channel configured for realm {realm_id}");
    };
    if !account.has_requested_scope("ak.self.events.command.submit") {
        anyhow::bail!(
            "Arkret account '{}' send=true but missing service scope ak.self.events.command.submit; refusing to call submit endpoint",
            account.id
        );
    }
    let flow = flow_id.map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret account '{}' cannot send without an inbound Arkret flow id",
            account.id
        )
    })?;

    // One-shot send restores the same keyring-backed session grant as the
    // listener and participates in the shared refresh/client rebuild path.
    let client = construct_account_client(&channel, &account).await?;
    let outbound_store = open_account_store(
        savfox_home,
        &channel.id,
        &account.id,
        ACCOUNT_EVENT_DEDUPE_MAX,
    )?;
    outbound_store.ensure_created().await?;
    // Monotonic, restart-safe actor sequence (parity with the applet path).
    let actor_seq = build_account_seq_allocator(savfox_home, &account.id)?
        .alloc()
        .map_err(|e| anyhow::anyhow!("arkret account seq alloc: {e}"))?;
    let request = MessageCreateRequest {
        realm_id: realm_id.to_owned(),
        flow_id: flow,
        body: body.to_owned(),
        principal_id: account.principal_id.clone(),
        actor_seq,
        thread_root_id: None,
    };
    let mut event = build_message_create_event(&request)?;
    let crypto_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &account.id);
    apply_account_outbound_encryption(&crypto_store, realm_id, &mut event)?;

    // Phase 8 (T8.E): attach capability grant event_id when configured.
    if let Some(grant_path) = &account.grant_event_path {
        let grant =
            savfox_channels::arkret::load_and_verify_grant(grant_path, &account.principal_id, None)
                .await
                .with_context(|| {
                    format!(
                        "Arkret account '{}' failed to load capability grant {}",
                        account.id,
                        grant_path.display()
                    )
                })?;
        if !grant.covers_action("ak.message.create") {
            anyhow::bail!(
                "Arkret account '{}' capability grant {} does not cover ak.message.create",
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
            savfox_channels::arkret::load_ed25519_signer(key_ref, &account.principal_id, &vm)?;
        savfox_channels::arkret::sign_outbound_event(&mut event, &signer, &vm)?;
    }

    let transaction_id = event.event_id.to_string();
    let realm_id_typed = RealmId::new(realm_id.to_owned())?;
    let outbound = OutboundEngine::new(outbound_store);
    outbound
        .enqueue(
            Some(transaction_id.clone()),
            realm_id_typed,
            arkret::sync_client::SendQueueItemKind::Message,
            serde_json::to_value(event)?,
            Vec::new(),
        )
        .await?;
    let submitter = AccountOutboundSubmitter { client };
    loop {
        match outbound.submit_next(&submitter, Utc::now()).await? {
            OutboundEngineOutcome::Accepted(item) | OutboundEngineOutcome::Duplicate(item)
                if item.transaction_id == transaction_id =>
            {
                debug!(
                    realm_id,
                    transaction_id, "arkret: durable outbound event accepted"
                );
                return Ok(());
            }
            OutboundEngineOutcome::Accepted(_) | OutboundEngineOutcome::Duplicate(_) => continue,
            OutboundEngineOutcome::RetryAt { item, at }
                if item.transaction_id == transaction_id =>
            {
                anyhow::bail!(
                    "arkret: outbound event queued for retry at {at} (transaction={transaction_id})"
                );
            }
            OutboundEngineOutcome::RetryAt { .. } => continue,
            OutboundEngineOutcome::Rejected(item) | OutboundEngineOutcome::Terminal(item)
                if item.transaction_id == transaction_id =>
            {
                anyhow::bail!("arkret: outbound event rejected (transaction={transaction_id})");
            }
            OutboundEngineOutcome::Rejected(_) | OutboundEngineOutcome::Terminal(_) => continue,
            OutboundEngineOutcome::Idle => {
                let snapshot = outbound.snapshot().await?;
                if snapshot.items.iter().any(|item| {
                    item.transaction_id == transaction_id && item.remote_event_id.is_some()
                }) {
                    return Ok(());
                }
                anyhow::bail!(
                    "arkret: outbound queue became idle before transaction {transaction_id} completed"
                );
            }
        }
    }
}

fn apply_account_outbound_encryption(
    crypto_store: &FileArkretCryptoStore,
    realm_id: &str,
    event: &mut arkret::Event,
) -> anyhow::Result<()> {
    let Some(content_block) = event.payload.get("content").cloned() else {
        return Ok(());
    };
    match crypto_store.encrypt_content_block_for_realm(realm_id, &content_block)? {
        ArkretEncryptOutcome::PlaintextAllowed => Ok(()),
        ArkretEncryptOutcome::Encrypted(encrypted_content) => {
            let object = event
                .payload
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Arkret message content is not an object"))?;
            object.remove("content");
            object.insert("encrypted_content".to_owned(), encrypted_content);
            Ok(())
        }
        ArkretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
            anyhow::bail!(
                "Arkret realm '{realm_id}' requires E2EE but no local MLS group state exists for group '{group_id}'"
            );
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn make_account() -> ArkretAccountConfig {
        ArkretAccountConfig {
            mode: savfox_channels::arkret::ArkretAccountMode::Agent,
            id: "support".into(),
            principal_id: "did:webvh:z6mkfixture:agent.example".into(),
            device_id: "ak:device:01904100-0000-7000-8000-000000000001".into(),
            access_token: String::new(),
            key_ref: None,
            verification_method: None,
            arkret_server_did: None,
            login_challenge: None,
            grant_event_path: None,
            inkson_bootstrap: None,
            authorized_event_ref: None,
            requested_scope: vec![
                "ak.self.events.stream.subscribe".into(),
                "ak.self.events.query.scan".into(),
                "ak.event.read".into(),
            ],
            listen: true,
            send: true,
        }
    }

    fn realm_id() -> arkret::RealmId {
        arkret::RealmId::new("ak:realm:01904100-0000-7000-8000-000000000001").unwrap()
    }

    fn actor_id() -> Did {
        Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap()
    }

    fn message_event(body: &str) -> arkret::Event {
        message_event_with_seq(body, 1)
    }

    fn message_event_with_seq(body: &str, actor_seq: u64) -> arkret::Event {
        arkret::Event::new(
            "ak.message.create",
            realm_id(),
            actor_id(),
            actor_seq,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            json!({
                "strand_id": "ak:strand:01904100-0000-7000-8000-000000000002",
                "track_name": "discussion",
                "content": {
                    "kind": "ak.content.text",
                    "body": body,
                    "thread_root_id": "ak:strand:01904100-0000-7000-8000-000000000003"
                }
            }),
        )
        .unwrap()
    }

    fn mls_welcome_value(group_id: &str) -> Value {
        serde_json::to_value(arkret::MlsWelcomeEnvelope {
            group_id: group_id.to_owned(),
            epoch: 7,
            recipient_principal_id: Did::new("did:webvh:z6mkfixture:bob.example".to_owned())
                .unwrap(),
            recipient_device_id: arkret::DeviceId::new(
                "ak:device:01904100-0000-7000-8000-00000000000e".to_owned(),
            )
            .unwrap(),
            welcome: "AA".to_owned(),
            welcome_hash: arkret::Hash::new(format!("sha256:{}", "ab".repeat(32))).unwrap(),
            ratchet_tree: None,
        })
        .unwrap()
    }

    #[test]
    fn sync_realm_delta_parses_dispatchable_messages() {
        let account = make_account();
        let realm_id = realm_id();
        let update = arkret::RealmUpdate {
            realm_id: realm_id.clone(),
            timeline: Some(arkret::SyncTimeline {
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
            Some("ak:strand:01904100-0000-7000-8000-000000000002")
        );
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
                Some("ak:cursor:device-next")
            ),
            AccountToDeviceAckPlan::Ack {
                ack_token: "ack-1".to_owned(),
                followup: Some(AccountDeviceMessagesPull {
                    initial_cursor: Some("ak:cursor:device-next".to_owned()),
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
        let update = arkret::RealmUpdate {
            realm_id: realm_id(),
            timeline: Some(arkret::SyncTimeline {
                events: vec![serde_json::to_value(message_event("window head")).unwrap()],
                limited: true,
                prev_cursor: Some("ak:cursor:older-1".to_owned()),
            }),
            state: Vec::new(),
            summary: Value::Null,
        };

        let request = account_scan_catchup_request_for_update(&update).unwrap();

        assert_eq!(request.realm_id, realm_id());
        assert_eq!(request.before.as_deref(), Some("ak:cursor:older-1"));
    }

    #[tokio::test]
    async fn scan_catchup_pages_older_history_and_reuses_account_parser() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let responses =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from([
                arkret::SyncBackfillOutcome {
                    events: vec![message_event_with_seq("older one", 2)],
                    snapshot_bootstrap: None,
                    prev_cursor: Some("ak:cursor:older-2".to_owned()),
                    next_cursor: None,
                    limited: true,
                },
                arkret::SyncBackfillOutcome {
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
            before: Some("ak:cursor:older-1".to_owned()),
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
                    Some("ak:cursor:older-1".to_owned()),
                    ACCOUNT_SCAN_CATCHUP_LIMIT
                ),
                (
                    realm_id().as_str().to_owned(),
                    Some("ak:cursor:older-2".to_owned()),
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
        let channel = ArkretChannelConfig {
            id: "c1".to_owned(),
            base_url: "https://arkret.example".to_owned(),
            service_id: None,
            accounts: Vec::new(),
        };
        let account = make_account();
        let crypto_store = FileArkretCryptoStore::for_account(tmp.path(), &channel.id, &account.id);
        let group_id = "group-account-welcome";
        let update = arkret::RealmUpdate {
            realm_id: realm_id(),
            timeline: Some(arkret::SyncTimeline {
                events: vec![json!({
                    "kind": "ak.mls.welcome",
                    "event_id": "ak:event:01904100-0000-7000-8000-000000000007",
                    "realm_id": realm_id().as_str(),
                    "actor_id": actor_id().as_str(),
                    "payload": {
                        "kind": "ak.mls.welcome",
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
