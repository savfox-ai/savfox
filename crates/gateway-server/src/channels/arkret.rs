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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use arkret::{
    DeviceId, DeviceMessagesAckRequestBody, Did, EventId, EventRef, EventsFrontierSelector,
    EventsFrontierView, KeyPackagesConsumeOutcome, KeyPackagesConsumeUnsignedRequest,
    KeyPackagesRevokeUnsignedRequest, KeyPackagesUploadRequestBody,
    KeyPackagesUploadUnsignedRequest, MlsKeyPackageRecord, PreparedDataEvent,
    PreparedStandardEvent, RealmId, StrandId,
};
use chrono::Utc;
use garth::{
    ClientEvent, CursorStore, DurableInboxStore, EventCacheStore, OutboundEngine,
    OutboundEngineOutcome, OutboundGenerationFence, OutboundGenerationFenceDecision,
    OutboundQueueStore, OutboundSubmitOutcome, OutboundSubmitter, RunOptions, RunStopReason,
    SyncLoopControl, TransportProvider,
};
use savfox_channels::arkret::{
    ArkretAccountConfig, ArkretAgentSessionProvider, ArkretChannelConfig,
    ArkretDecryptDetailedOutcome, ArkretEncryptOutcome, ArkretHttpClient, ArkretInboundEvent,
    ArkretInboundParseResult, ArkretInboundSkipReason, ArkretInboundSkippedEvent, ArkretKeyRef,
    ArkretMlsWelcomeConsumeBinding, EventInitialSubmission, FileArkretCryptoStore,
    MessageCreateRequest, SidecarExchangeAdmission, SidecarExchangeContext, SidecarExchangeStore,
    SidecarRequestGate, SidecarTerminalAdmission, UnableToDecryptReason, account_allows_event_read,
    apply_data_event_basis, build_message_create_event, build_user_facing_response_metadata,
    device_messages_scope, encode_sidecar_reply_target, gate_inbound_exchange_control,
    gate_inbound_request_binding, open_account_store, parse_delta_frame_for_account,
    resolve_arkret_outbound_account_for_binding, sidecar_binding_from_metadata_plaintext,
    sign_keypackages_consume_request, sign_keypackages_revoke_request,
    sign_keypackages_upload_request,
};
use serde::{Deserialize, Serialize};
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
    last_reason_code: Option<String>,
    last_event_id: Option<String>,
    last_realm_id: Option<String>,
    last_local_agent_id: Option<String>,
    last_presence_at: Option<chrono::DateTime<Utc>>,
    last_presence_error: Option<String>,
    presence_heartbeats: u64,
    received_events: u64,
    dispatched_events: u64,
    baselined_events: u64,
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
            last_reason_code: None,
            last_event_id: None,
            last_realm_id: None,
            last_local_agent_id: None,
            last_presence_at: None,
            last_presence_error: None,
            presence_heartbeats: 0,
            received_events: 0,
            dispatched_events: 0,
            baselined_events: 0,
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
            "last_reason_code": self.last_reason_code,
            "last_event_id": self.last_event_id,
            "last_realm_id": self.last_realm_id,
            "last_local_agent_id": self.last_local_agent_id,
            "last_presence_at": self.last_presence_at,
            "last_presence_error": self.last_presence_error,
            "presence_heartbeats": self.presence_heartbeats,
            "received_events": self.received_events,
            "dispatched_events": self.dispatched_events,
            "baselined_events": self.baselined_events,
            "skipped_events": self.skipped_events,
            "updated_at": self.updated_at,
        })
    }
}

const ACCOUNT_EVENT_DEDUPE_MAX: usize = 4096;
const ACCOUNT_SCAN_CATCHUP_LIMIT: u32 = 100;
const ACCOUNT_SCAN_CATCHUP_MAX_PAGES: usize = 64;
const ACCOUNT_DURABLE_WORK_POLL: Duration = Duration::from_millis(250);
const ACCOUNT_AUTH_WARNING_INTERVAL: Duration = Duration::from_secs(30);
/// v1 session Signals expire after 30 seconds. Twenty seconds leaves ten
/// seconds for scheduling, network jitter and an in-band session refresh.
const ACCOUNT_PRESENCE_REFRESH: Duration = Duration::from_secs(20);
const DEVICE_MESSAGES_PULL_LIMIT: u32 = 100;
const DEVICE_MESSAGES_PULL_MAX_PAGES: usize = 16;

const KEYPACKAGES_UPLOAD_SCOPE: &str = "ak.self.keys.keypackages.upload.create";
const KEYPACKAGES_CONSUME_SCOPE: &str = "ak.self.keys.keypackages.command.consume";
const KEYPACKAGES_REVOKE_SCOPE: &str = "ak.self.keys.keypackages.command.revoke";
const KEYPACKAGE_MIN_AVAILABLE: u64 = 8;
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
        diagnostic.last_reason_code = arkret_reason_code(&error);
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
        diagnostic.last_reason_code = None;
    });
}

fn arkret_reason_code(error: &str) -> Option<String> {
    let marker = "reason_code=";
    let start = error.find(marker)? + marker.len();
    let code = error[start..]
        .split(|ch: char| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'))
        .next()
        .unwrap_or_default();
    (!code.is_empty()).then(|| code.to_owned())
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
    let arkret_config = ArkretChannelConfig::from_strict_agent_config(config)?;
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

/// Remove the in-memory runtime record for an account whose binding was
/// explicitly erased. A later replacement pairing uses a different account id;
/// retaining the old terminal diagnostic would make channel-level health look
/// failed even while the replacement listener is healthy.
fn forget_arkret_account_runtime(channel_id: &str, account_id: &str) {
    let key = task_key(channel_id, account_id);
    let Ok(mut state) = runtime_state().lock() else {
        warn!(
            channel_id,
            account_id, "arkret: runtime state mutex poisoned while forgetting unbound account"
        );
        return;
    };
    if let Some(handle) = state.handles.remove(&key) {
        handle.abort();
    }
    state.diagnostics.remove(&key);
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

pub(crate) fn arkret_account_listener_task_count(channel_id: &str) -> usize {
    let prefix = format!("{channel_id}::");
    let Ok(state) = runtime_state().lock() else {
        warn!("arkret: runtime state mutex poisoned; cannot inspect tasks for '{channel_id}'");
        return 0;
    };
    state
        .handles
        .iter()
        .filter(|(key, handle)| key.starts_with(&prefix) && !handle.is_finished())
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
        run_account_listener_retry_loop(diagnostic_channel_id, diagnostic_account_id, move || {
            run_account_listener(
                savfox_home.clone(),
                channel.clone(),
                account.clone(),
                Arc::clone(&gateway_channel),
                Arc::clone(&session_store),
            )
        })
        .await;
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

async fn run_account_listener_retry_loop<F, Fut>(
    diagnostic_channel_id: String,
    diagnostic_account_id: String,
    mut run: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
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
        run().await;
        let migration_required = runtime_state()
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .diagnostics
                    .get(&task_key(&diagnostic_channel_id, &diagnostic_account_id))
                    .and_then(|diagnostic| diagnostic.last_reason_code.as_deref())
                    .map(|reason| reason == "agent_requested_scope_commitment_invalid")
            })
            .unwrap_or(false);
        if migration_required {
            update_listener_diagnostic(
                &diagnostic_channel_id,
                &diagnostic_account_id,
                |diagnostic| diagnostic.phase = "migration_required",
            );
            warn!(
                channel_id = %diagnostic_channel_id,
                account_id = %diagnostic_account_id,
                "arkret: immutable requested-scope commitment is invalid; listener stopped until the Agent is re-provisioned"
            );
            break;
        }
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
    if let Err(err) = crypto_store.ensure_created() {
        warn!(
            "arkret: account '{}' crypto state unavailable at {}: {err:#}",
            account.id,
            crypto_store.path().display()
        );
    }

    let provider = match construct_account_provider(&savfox_home, &channel, &account).await {
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
    if let Err(err) = run_account_key_lifecycle_maintenance(
        &client,
        &savfox_home,
        &channel,
        &account,
        &account_store,
        &crypto_store,
        "startup",
    )
    .await
    {
        record_listener_failure(
            &channel,
            &account,
            "key_lifecycle_migration_error",
            format!("{err:#}"),
        );
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: refusing to subscribe while pairing-scoped key migration is incomplete: {err:#}"
        );
        runtime::record_channel_probe("arkret", "error").await;
        return;
    }

    match crate::arkret_delivery::resume_pending_checkpoints(&savfox_home, &channel.id, &account.id)
        .await
    {
        Ok(count) if count > 0 => info!(
            channel_id = %channel.id,
            account_id = %account.id,
            published = count,
            "arkret: resumed durable checkpoint deliveries before subscribing"
        ),
        Ok(_) => {}
        Err(error) => warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: could not inspect pending checkpoint deliveries: {error:#}"
        ),
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccountInboundMode {
    Baseline,
    Hydrate,
    Trigger,
}

impl AccountInboundMode {
    fn suppresses_agent_dispatch(self) -> bool {
        self != Self::Trigger
    }
}

fn account_inbound_mode(events: &[ClientEvent]) -> AccountInboundMode {
    match events.first() {
        Some(ClientEvent::AccountUpdates(updates)) if updates.initial_catchup => {
            AccountInboundMode::Baseline
        }
        _ => AccountInboundMode::Trigger,
    }
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
    let mut delivery_poll = tokio::time::interval(ACCOUNT_DURABLE_WORK_POLL);
    delivery_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut presence_refresh = tokio::time::interval(ACCOUNT_PRESENCE_REFRESH);
    presence_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_auth_warning = None;

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
                    &mut last_auth_warning,
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
                    &mut last_auth_warning,
                )
                .await;
            }
            _ = presence_refresh.tick() => {
                refresh_account_presence(
                    provider,
                    channel,
                    account,
                    &crypto_store,
                )
                .await;
            }
        }
    }
}

async fn refresh_account_presence(
    provider: &ArkretAgentSessionProvider,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) {
    let ready_realms = match crypto_store.presence_ready_realm_ids() {
        Ok(realms) => realms,
        Err(error) => {
            record_presence_failure(
                channel,
                account,
                format!("load MLS presence scopes: {error:#}"),
            );
            return;
        }
    };
    if ready_realms.is_empty() {
        return;
    }
    let key_ref = match account.key_ref.as_ref() {
        Some(key_ref) => key_ref,
        None => {
            record_presence_failure(channel, account, "missing runtime keyRef");
            return;
        }
    };
    let verification_method = match account.verification_method.as_deref() {
        Some(method) => method,
        None => {
            record_presence_failure(channel, account, "missing runtime verificationMethod");
            return;
        }
    };
    let client = match provider.provide().await {
        Ok(client) => ArkretHttpClient::from_inner(client),
        Err(error) => {
            record_presence_failure(
                channel,
                account,
                format!("restore authenticated presence client: {error}"),
            );
            return;
        }
    };

    for realm in ready_realms {
        let realm_id = match RealmId::new(realm.clone()) {
            Ok(realm_id) => realm_id,
            Err(error) => {
                record_presence_failure(
                    channel,
                    account,
                    format!("invalid presence Realm id '{realm}': {error}"),
                );
                continue;
            }
        };
        let selector = arkret::EventsFrontierSelector::RealmSeal {
            realm_id: realm_id.clone(),
        };
        let frontier = match client.inner().events_frontier(&selector).await {
            Ok(frontier) => frontier,
            Err(error) => {
                record_presence_failure(
                    channel,
                    account,
                    format!("fetch current Seal for presence Realm '{realm}': {error}"),
                );
                continue;
            }
        };
        let seal_ref = match frontier.frontier {
            arkret::EventsFrontierView::RealmSeal(frontier) => frontier.seal_id,
            _ => {
                record_presence_failure(
                    channel,
                    account,
                    format!("frontier selector returned a non-Realm Seal for '{realm}'"),
                );
                continue;
            }
        };
        let envelope = match crypto_store.seal_online_presence_signal(
            realm_id.as_str(),
            &account.principal_id,
            &account.device_id,
            verification_method,
            key_ref,
            seal_ref.as_str(),
            Utc::now(),
        ) {
            Ok(envelope) => envelope,
            Err(error) => {
                record_presence_failure(
                    channel,
                    account,
                    format!("seal encrypted presence for Realm '{realm}': {error:#}"),
                );
                continue;
            }
        };
        match client.inner().signal_send(&envelope).await {
            Ok(outcome) if outcome.accepted && outcome.realm_id == realm_id => {
                update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
                    diagnostic.last_presence_at = Some(Utc::now());
                    diagnostic.last_presence_error = None;
                    diagnostic.presence_heartbeats =
                        diagnostic.presence_heartbeats.saturating_add(1);
                });
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    realm_id = %realm,
                    "arkret: encrypted presence heartbeat accepted"
                );
            }
            Ok(outcome) => record_presence_failure(
                channel,
                account,
                format!(
                    "presence submit for Realm '{realm}' returned accepted={} realm_id={}",
                    outcome.accepted, outcome.realm_id
                ),
            ),
            Err(error) => record_presence_failure(
                channel,
                account,
                format!("submit presence for Realm '{realm}': {error}"),
            ),
        }
    }
}

fn record_presence_failure(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    error: impl Into<String>,
) {
    let error = error.into();
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.last_presence_error = Some(error.clone());
    });
    warn!(
        channel_id = %channel.id,
        account_id = %account.id,
        "arkret: presence heartbeat unavailable: {error}"
    );
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
    last_auth_warning: &mut Option<tokio::time::Instant>,
) {
    let now_ms = Utc::now().timestamp_millis();
    let inbox_due = match account_store.pending(1).await {
        Ok(deliveries) => deliveries.first().is_some_and(|delivery| {
            delivery
                .next_attempt_at_ms
                .is_none_or(|next_at| next_at <= now_ms)
        }),
        Err(error) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to preflight durable account inbox: {error}"
            );
            return;
        }
    };
    let outbound_active = match account_store.has_active_outbound() {
        Ok(active) => active,
        Err(error) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to preflight durable account outbound queue: {error}"
            );
            return;
        }
    };
    if !inbox_due && !outbound_active {
        return;
    }

    let client = match provider.provide().await {
        Ok(client) => {
            *last_auth_warning = None;
            ArkretHttpClient::from_inner(client)
        }
        Err(error) => {
            let now = tokio::time::Instant::now();
            let warning_due = last_auth_warning
                .is_none_or(|last| now.duration_since(last) >= ACCOUNT_AUTH_WARNING_INTERVAL);
            if warning_due {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    inbox_due,
                    outbound_active,
                    "arkret: cannot drain durable account work without an authenticated client: {error}"
                );
                *last_auth_warning = Some(now);
            }
            return;
        }
    };
    process_durable_account_inbox(
        provider,
        &client,
        channel,
        account,
        account_store,
        crypto_store,
        gateway_channel,
        session_store,
    )
    .await;
    drain_pending_account_outbound(&client, account_store, channel, account, crypto_store).await;
}

async fn drain_pending_account_outbound(
    client: &ArkretHttpClient,
    account_store: &garth::FileStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) {
    let savfox_home = match savfox_utils::home_dir::find_savfox_home() {
        Ok(path) => path,
        Err(error) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: cannot inspect the outbound actor chain without SAVFOX_HOME: {error}"
            );
            return;
        }
    };
    let outbound = OutboundEngine::new(account_store.clone());
    let submitter = AccountOutboundSubmitter {
        client: client.clone(),
    };
    let fence = AccountOutboundEncryptionFence {
        crypto_store: crypto_store.clone(),
        actor_chain_path: account_actor_chain_path(&savfox_home, &account.id),
    };
    loop {
        match outbound
            .submit_next_with_fence(&submitter, &fence, Utc::now())
            .await
        {
            Ok(OutboundEngineOutcome::Accepted(item) | OutboundEngineOutcome::Duplicate(item)) => {
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    transaction_id = %item.transaction_id,
                    "arkret: durable outbound worker completed queued event"
                );
            }
            Ok(
                OutboundEngineOutcome::Rejected { item, .. }
                | OutboundEngineOutcome::Terminal { item, .. }
                | OutboundEngineOutcome::Quarantined { item, .. },
            ) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    transaction_id = %item.transaction_id,
                    "arkret: durable outbound worker reached terminal event state"
                );
            }
            Ok(OutboundEngineOutcome::Prepared(_) | OutboundEngineOutcome::Superseded { .. }) => {}
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
    provider: &ArkretAgentSessionProvider,
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
            let inbound_mode = account_inbound_mode(&delivery.events);
            if inbound_mode == AccountInboundMode::Baseline {
                info!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    delivery_id = delivery.id.get(),
                    events = delivery.events.len(),
                    "arkret: processing initial account catch-up as a history baseline"
                );
            }
            let mut processing_error = None;
            for event in delivery.events {
                if let Err(error) = handle_account_client_event(
                    provider,
                    client,
                    event,
                    inbound_mode,
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
    provider: &ArkretAgentSessionProvider,
    client: &ArkretHttpClient,
    event: ClientEvent,
    inbound_mode: AccountInboundMode,
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
            )
            .await?;
        }
        ClientEvent::RealmDelta { update, .. } => {
            let scan_request = account_scan_catchup_request_for_update(&update);
            record_account_realm_crypto_policy_from_update(&update, crypto_store, channel, account);
            record_account_mls_welcomes_from_realm_update(&update, crypto_store, channel, account);
            let parsed = parse_realm_update_for_account(update, account);
            handle_parsed_account_events(
                provider,
                client,
                parsed,
                inbound_mode,
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
                    provider,
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
        ClientEvent::ToDevice(message) => {
            let content = Value::Object(message.content.into_iter().collect());
            record_account_mls_welcome_from_value_tree(
                crypto_store,
                &content,
                channel,
                account,
                "to_device",
            );
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
    channel.service_id.clone().or_else(|| {
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
    savfox_home: &Path,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    reason: &'static str,
) -> anyhow::Result<()> {
    retire_legacy_account_keypackages(client, savfox_home, channel, account)
        .await
        .context("retire the legacy KeyPackage pool")?;
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
    repair_and_consume_pending_mls_welcomes(client, channel, account, crypto_store).await;
    Ok(())
}

async fn repair_and_consume_pending_mls_welcomes(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) {
    let pending = match crypto_store.pending_mls_welcome_consume_bindings() {
        Ok(pending) => pending,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to load pending MLS Welcome consume bindings: {err:#}"
            );
            return;
        }
    };
    let realm_ids = pending
        .iter()
        .filter(|binding| binding.welcome_ref.is_none() || binding.strand_id.is_none())
        .filter_map(|binding| binding.realm_id.as_deref())
        .collect::<HashSet<_>>();

    if !realm_ids.is_empty() && !account.has_requested_scope("ak.self.events.read.scan") {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            pending_realms = realm_ids.len(),
            "arkret: cannot repair pending Direct Conversation Welcome bindings without ak.self.events.read.scan"
        );
    } else {
        for realm_id in realm_ids {
            let realm_id = match RealmId::new(realm_id.to_owned()) {
                Ok(realm_id) => realm_id,
                Err(err) => {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        "arkret: invalid pending Welcome Realm id: {err}"
                    );
                    continue;
                }
            };
            let outcome = collect_account_scan_catchup(
                AccountScanCatchupRequest {
                    realm_id: realm_id.clone(),
                    before: None,
                },
                |realm_id, before, limit| async move {
                    client
                        .inner()
                        .events_read_outcome(
                            realm_id.as_str(),
                            before.as_deref(),
                            None,
                            None,
                            Some(limit),
                            None,
                        )
                        .await
                        .map_err(anyhow::Error::from)
                },
            )
            .await;
            match outcome {
                Ok(outcome) => {
                    let event_kinds = outcome
                        .events
                        .iter()
                        .map(|event| event.kind.as_str())
                        .collect::<Vec<_>>();
                    let repaired = crypto_store
                        .repair_pending_direct_conversation_bindings_from_accepted_events(
                            &outcome.events,
                        )
                        .unwrap_or_else(|err| {
                            warn!(
                                channel_id = %channel.id,
                                account_id = %account.id,
                                realm_id = %realm_id,
                                "arkret: failed to repair pending consume binding from accepted Realm history: {err:#}"
                            );
                            0
                        });
                    for event in &outcome.events {
                        if let Ok(value) = serde_json::to_value(event) {
                            record_account_mls_welcome_from_value_tree(
                                crypto_store,
                                &value,
                                channel,
                                account,
                                "startup_pending_welcome_repair",
                            );
                            apply_account_mls_commits_from_value_tree(
                                crypto_store,
                                &value,
                                channel,
                                account,
                                "startup_pending_welcome_repair",
                            );
                        }
                    }
                    debug!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        realm_id = %realm_id,
                        events = outcome.events.len(),
                        pages = outcome.pages,
                        limited = outcome.limited,
                        event_kinds = ?event_kinds,
                        repaired,
                        "arkret: scanned Realm history to repair pending MLS Welcome binding"
                    );
                }
                Err(err) => warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    realm_id = %realm_id,
                    "arkret: failed to scan Realm history for pending MLS Welcome binding: {err:#}"
                ),
            }
        }
    }

    match crypto_store.pending_mls_welcome_consume_bindings() {
        Ok(pending) => {
            consume_account_mls_key_packages(client, channel, account, crypto_store, &pending)
                .await;
        }
        Err(err) => warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: failed to reload repaired MLS Welcome consume bindings: {err:#}"
        ),
    }
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
    let Some(key_ref) = account.key_ref.as_ref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: Agent MLS KeyPackage upload requires the authorized runtime key"
        );
        return;
    };
    let Some(verification_method) = account.verification_method.as_deref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: Agent MLS KeyPackage upload requires the authorized verification method"
        );
        return;
    };
    for last_resort in [false, true] {
        match crypto_store.ensure_agent_mls_key_package(
            &account.principal_id,
            &account.device_id,
            last_resort,
            key_ref,
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

    let Some(available_count) = upload_account_mls_key_packages(
        client,
        channel,
        account,
        principal.clone(),
        device.clone(),
        &records,
        key_ref,
        verification_method,
    )
    .await
    else {
        return;
    };

    let Some(deficit) = keypackage_replenishment_deficit(Some(available_count)) else {
        return;
    };
    let fresh = match crypto_store.create_fresh_agent_mls_key_packages(
        &account.principal_id,
        &account.device_id,
        deficit,
        key_ref,
    ) {
        Ok(records) => records,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                available_count,
                deficit,
                "arkret: failed to replenish local MLS KeyPackage pool: {err:#}"
            );
            return;
        }
    };
    let replenished_count = upload_account_mls_key_packages(
        client,
        channel,
        account,
        principal,
        device,
        &fresh,
        key_ref,
        verification_method,
    )
    .await;
    if replenished_count.is_none_or(|count| count < KEYPACKAGE_MIN_AVAILABLE) {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            available_count = ?replenished_count,
            minimum = KEYPACKAGE_MIN_AVAILABLE,
            "arkret: MLS KeyPackage pool remains below its required maintenance low-watermark"
        );
    }
}

fn keypackage_replenishment_deficit(available_count: Option<u64>) -> Option<usize> {
    let available_count = available_count?;
    (available_count < KEYPACKAGE_MIN_AVAILABLE)
        .then(|| (KEYPACKAGE_MIN_AVAILABLE - available_count) as usize)
}

#[allow(clippy::too_many_arguments)]
async fn upload_account_mls_key_packages(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    principal: Did,
    device: DeviceId,
    records: &[MlsKeyPackageRecord],
    key_ref: &ArkretKeyRef,
    verification_method: &str,
) -> Option<u64> {
    let request = match build_signed_keypackage_upload_request(
        principal,
        device,
        records,
        key_ref,
        verification_method,
    ) {
        Ok(request) => request,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to build canonical MLS KeyPackage upload request: {err:#}"
            );
            return None;
        }
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
            match outcome.available_count {
                Some(available_count) => Some(available_count),
                None => {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        "arkret: owning-device KeyPackage upload omitted available_count"
                    );
                    None
                }
            }
        }
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: MLS KeyPackage upload failed: {err}"
            );
            None
        }
    }
}

/// Revoke this account's published MLS KeyPackage pool on the Principal Server.
///
/// Invoked during unbind while the Agent's `ak.agent.key.authorize` is still
/// current, so the old runtime key can sign the canonical revoke and the pool
/// fails closed before the binding is replaced (spec §3: quiesce old session,
/// revoke old pool). Any inability to prove complete remote revocation aborts
/// the unbind before local signing material or the persisted binding is erased.
pub(crate) async fn revoke_account_mls_key_packages(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) -> anyhow::Result<Option<usize>> {
    if !account.has_requested_scope(KEYPACKAGES_REVOKE_SCOPE) {
        anyhow::bail!(
            "unbind cannot revoke MLS KeyPackages without scope {KEYPACKAGES_REVOKE_SCOPE}"
        );
    }
    let key_package_refs = crypto_store
        .revocable_keypackage_refs()
        .context("enumerate local MLS KeyPackages to revoke")?;
    revoke_account_mls_key_package_refs(
        client,
        channel,
        account,
        crypto_store,
        key_package_refs,
        "agent runtime unbind",
        false,
    )
    .await
}

/// Retire claimable KeyPackages left in the pre-pairing-scoped crypto file.
///
/// The account id for a flat Agent binding used to equal the channel id.  The
/// pairing-scoped id deliberately changes when that binding is replaced so
/// cursors, Realm policies and MLS group state cannot leak into the new Agent.
/// KeyPackages published before that migration are still claimable remotely,
/// though, and their private init keys remain only in the legacy file.  Revoke
/// just the exact current principal/device rows before publishing the new pool;
/// never import the legacy Realm or cursor state.
async fn retire_legacy_account_keypackages(
    client: &ArkretHttpClient,
    savfox_home: &Path,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<Option<usize>> {
    if account.id == channel.id {
        return Ok(None);
    }
    let legacy_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &channel.id);
    let refs = legacy_store
        .revocable_keypackage_refs_for_agent(&account.principal_id, &account.device_id)
        .context("enumerate current Agent KeyPackages in legacy crypto scope")?;
    revoke_account_mls_key_package_refs(
        client,
        channel,
        account,
        &legacy_store,
        refs,
        "pairing-scoped storage migration",
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn revoke_account_mls_key_package_refs(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
    key_package_refs: Vec<String>,
    reason: &str,
    accept_terminally_unclaimable: bool,
) -> anyhow::Result<Option<usize>> {
    let device = DeviceId::new(account.device_id.clone())
        .context("invalid device id for MLS KeyPackage revoke")?;
    let key_ref = account
        .key_ref
        .as_ref()
        .context("Agent MLS KeyPackage revoke requires the authorized runtime key")?;
    let verification_method = account
        .verification_method
        .as_deref()
        .context("Agent MLS KeyPackage revoke requires the authorized verification method")?;
    if key_package_refs.is_empty() {
        debug!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: no local pool MLS KeyPackages to revoke"
        );
        return Ok(None);
    }
    let unsigned = KeyPackagesRevokeUnsignedRequest {
        owner_account_id: arkret::Did::new(account.id.clone())
            .map_err(|error| anyhow::anyhow!("invalid Arkret account id: {error}"))?,
        key_package_refs: key_package_refs.clone(),
        device_id: device,
        reason: Some(
            arkret::NonEmptyString::new(reason.to_owned())
                .map_err(|error| anyhow::anyhow!("invalid KeyPackage revoke reason: {error}"))?,
        ),
    };
    let signature = sign_keypackages_revoke_request(key_ref, verification_method, &unsigned)
        .context("sign canonical MLS KeyPackage revoke request")?;
    let request = unsigned.into_signed(signature);
    let outcome = client
        .inner()
        .keypackages_revoke(&request)
        .await
        .context("revoke MLS KeyPackage pool during unbind")?;
    let safely_retired_failures = outcome
        .failures
        .iter()
        .filter(|failure| {
            accept_terminally_unclaimable
                && keypackage_retirement_failure_is_terminal(&failure.reason_code)
        })
        .filter_map(|failure| failure.keypackage_ref.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    let fully_accounted = key_package_refs.iter().all(|keypackage_ref| {
        outcome.revoked.contains(keypackage_ref)
            || safely_retired_failures.contains(keypackage_ref.as_str())
    });
    if !fully_accounted
        || outcome.failures.iter().any(|failure| {
            !safely_retired_failures.contains(failure.keypackage_ref.as_deref().unwrap_or_default())
        })
    {
        anyhow::bail!(
            "MLS KeyPackage revoke did not acknowledge the complete pool: revoked={:?}, failures={:?}",
            outcome.revoked,
            outcome.failures
        );
    }
    for keypackage_ref in &key_package_refs {
        if let Err(err) = crypto_store.mark_mls_key_package_revoked(keypackage_ref) {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %keypackage_ref,
                "arkret: failed to mark local MLS KeyPackage revoked after server ack: {err:#}"
            );
        }
    }
    info!(
        channel_id = %channel.id,
        account_id = %account.id,
        revoked = outcome.revoked.len(),
        terminally_unclaimable = safely_retired_failures.len(),
        reason,
        "arkret: retired Agent MLS KeyPackage pool"
    );
    Ok(Some(outcome.revoked.len() + safely_retired_failures.len()))
}

fn keypackage_retirement_failure_is_terminal(reason_code: &arkret::ReasonCode) -> bool {
    matches!(
        reason_code.as_str(),
        arkret::ErrorCode::KEYPACKAGE_ALREADY_CONSUMED | arkret::ErrorCode::KEYPACKAGE_UNKNOWN
    )
}

fn consume_outcome_acknowledges_binding(
    outcome: &KeyPackagesConsumeOutcome,
    keypackage_ref: &str,
) -> bool {
    outcome.failures.is_empty()
        || (outcome.failures.len() == 1
            && outcome.failures[0].keypackage_ref.as_deref() == Some(keypackage_ref)
            && keypackage_retirement_failure_is_terminal(&outcome.failures[0].reason_code))
}

/// Outcome of an explicit Agent runtime unbind, surfaced to the RPC caller.
pub(crate) struct ArkretUnbindReport {
    pub principal_id: String,
    pub device_id: String,
    pub listeners_stopped: usize,
    pub revoke_attempted: bool,
}

/// Explicitly unbind the Agent currently bound to this channel/account.
///
/// Fail-closed teardown, in order: (1) stop the listener so no in-flight task
/// races the teardown, (2) revoke the published KeyPackage pool with the still
/// current runtime key, (3) purge the local Agent MLS identity / private
/// KeyPackage material and durable subscribe state. The caller is responsible
/// for clearing the persisted binding fields from the channel config afterward.
pub(crate) async fn unbind_arkret_account(
    savfox_home: &std::path::Path,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<ArkretUnbindReport> {
    let listeners_stopped = stop_arkret_account_listeners(&channel.id);

    let crypto_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &account.id);
    let revoke_attempted = match construct_account_provider(savfox_home, channel, account).await {
        Ok(provider) => {
            let inner = provider
                .provide()
                .await
                .context("build Agent session client for unbind")?;
            let client = ArkretHttpClient::from_inner(inner);
            revoke_account_mls_key_packages(&client, channel, account, &crypto_store)
                .await?
                .is_some()
        }
        Err(error) if terminal_agent_authorization_makes_pool_unclaimable(&error) => {
            // The Principal Server already made KeyPackages bound to this
            // authorization permanently unclaimable. Requiring a new session
            // from that dead key is impossible and would strand local pairing
            // state forever. It is now safe to finish the local purge.
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason = savfox_channels::arkret::agent_session_exchange_reason(&error)
                    .unwrap_or("unknown"),
                "arkret: remote authorization is terminal; skipping redundant KeyPackage revoke during unbind"
            );
            false
        }
        Err(error) => {
            return Err(error).context("construct Agent session provider for unbind");
        }
    };

    if let Err(err) = crypto_store.delete_persisted() {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: unbind failed to delete Agent crypto state: {err}"
        );
    }
    if let Err(err) =
        savfox_channels::arkret::delete_account_store(savfox_home, &channel.id, &account.id)
    {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: unbind failed to delete Agent account state: {err}"
        );
    }
    if let Err(err) = savfox_channels::arkret::delete_verified_runtime_scope(
        savfox_home,
        &channel.id,
        &account.id,
    )
    .await
    {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: unbind failed to delete verified runtime scope: {err:#}"
        );
    }

    forget_arkret_account_runtime(&channel.id, &account.id);

    info!(
        channel_id = %channel.id,
        account_id = %account.id,
        principal_id = %account.principal_id,
        listeners_stopped,
        revoke_attempted,
        "arkret: unbound Agent runtime; local state purged"
    );

    Ok(ArkretUnbindReport {
        principal_id: account.principal_id.clone(),
        device_id: account.device_id.clone(),
        listeners_stopped,
        revoke_attempted,
    })
}

fn terminal_agent_authorization_makes_pool_unclaimable(error: &anyhow::Error) -> bool {
    savfox_channels::arkret::agent_session_exchange_reason(error)
        .is_some_and(savfox_channels::arkret::agent_session_reason_is_irreversibly_terminal)
}

fn build_signed_keypackage_upload_request(
    principal_id: Did,
    device_id: DeviceId,
    records: &[MlsKeyPackageRecord],
    key_ref: &ArkretKeyRef,
    verification_method: &str,
) -> anyhow::Result<KeyPackagesUploadRequestBody> {
    let key_packages = records
        .iter()
        .map(arkret::mls_key_package_record_upload_entry)
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::msg)
        .context("project canonical MLS KeyPackage upload entries")?;
    let unsigned = KeyPackagesUploadUnsignedRequest {
        principal_id,
        device_id,
        key_packages,
        expires_at: None,
        strand_id: None,
        mls_group_id: None,
    };
    let device_signature =
        sign_keypackages_upload_request(key_ref, verification_method, &unsigned)?;
    Ok(unsigned.into_signed(device_signature))
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
            let content = Value::Object(message.content.clone().into_iter().collect());
            record_account_mls_welcome_from_value_tree(
                crypto_store,
                &content,
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
                Ok(scope) => account_store
                    .clear(scope)
                    .await
                    .map_err(anyhow::Error::from),
                Err(err) => Err(anyhow::Error::from(err)),
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
                Ok(scope) => account_store
                    .save(scope, next_cursor.clone())
                    .await
                    .map_err(anyhow::Error::from),
                Err(err) => Err(anyhow::Error::from(err)),
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
    let Some(key_ref) = account.key_ref.as_ref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: Agent MLS KeyPackage consume requires the authorized runtime key"
        );
        return;
    };
    let Some(verification_method) = account.verification_method.as_deref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: Agent MLS KeyPackage consume requires the authorized verification method"
        );
        return;
    };

    let mut consumed_any = false;
    for binding in bindings {
        if binding.welcome_ref.is_none()
            || binding.realm_id.is_none()
            || binding.strand_id.is_none()
        {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %binding.keypackage_ref,
                "arkret: deferring MLS KeyPackage consume until exact Direct Conversation binding context is available"
            );
            continue;
        }
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
        let Some(recipient_durable_receipt) = binding.recipient_durable_receipt.clone() else {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %binding.keypackage_ref,
                "arkret: deferring MLS KeyPackage consume until recipient durable receipt is available"
            );
            continue;
        };
        let owner_account_id = match arkret::Did::new(account.id.clone()) {
            Ok(value) => value,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id, "arkret: invalid account id for MLS KeyPackage consume: {error}");
                continue;
            }
        };
        let claim_id = match arkret::NonEmptyString::new(binding.claim_id.clone()) {
            Ok(value) => value,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id, keypackage_ref = %binding.keypackage_ref, "arkret: invalid claim id: {error}");
                continue;
            }
        };
        let welcome_ref = match arkret::NonEmptyString::new(binding.welcome_ref.clone().unwrap()) {
            Ok(value) => value,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id, keypackage_ref = %binding.keypackage_ref, "arkret: invalid welcome ref: {error}");
                continue;
            }
        };
        let mls_group_id = match arkret::NonEmptyString::new(binding.mls_group_id.clone()) {
            Ok(value) => value,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id, keypackage_ref = %binding.keypackage_ref, "arkret: invalid MLS group id: {error}");
                continue;
            }
        };
        let unsigned = KeyPackagesConsumeUnsignedRequest {
            owner_account_id,
            key_package_refs: vec![binding.keypackage_ref.clone()],
            consumer_device_id: consumer_device.clone(),
            claim_ids: vec![claim_id],
            welcome_ref,
            recipient_durable_receipt,
            realm_id,
            strand_id,
            mls_group_id: Some(mls_group_id),
            epoch: Some(binding.epoch),
        };
        let signature =
            match sign_keypackages_consume_request(key_ref, verification_method, &unsigned) {
                Ok(signature) => signature,
                Err(err) => {
                    warn!(
                        channel_id = %channel.id,
                        account_id = %account.id,
                        keypackage_ref = %binding.keypackage_ref,
                        "arkret: failed to sign canonical MLS KeyPackage consume request: {err:#}"
                    );
                    continue;
                }
            };
        let request = unsigned.into_signed(signature);
        match client.inner().keypackages_consume(&request).await {
            Ok(outcome)
                if consume_outcome_acknowledges_binding(&outcome, &binding.keypackage_ref) =>
            {
                consumed_any = true;
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
                    idempotent_terminal_replay = !outcome.failures.is_empty(),
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
    if consumed_any {
        // A successful Welcome consume necessarily reduced the ordinary
        // single-use pool. Replenish during this device-maintenance cycle;
        // the server-provided available_count remains the source of truth.
        publish_account_mls_key_packages(client, channel, account, crypto_store).await;
    }
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
    updates: garth::AccountUpdateContext,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
) -> anyhow::Result<()> {
    let to_device_lost = updates.to_device_lost;
    let saw_to_device_messages = updates.to_device_ack_token.is_some();
    let to_device_ack_token = updates.to_device_ack_token.clone();
    let to_device_limited = updates.to_device_limited;
    let to_device_next_cursor = updates.to_device_next_cursor.clone();
    for item in &updates.account_data {
        let payload = Value::Object(item.payload.clone().into_iter().collect());
        record_account_mls_welcome_from_value_tree(
            crypto_store,
            &payload,
            channel,
            account,
            "account_data",
        );
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
    let timeline = update.entry.timeline.as_ref()?;
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
    Fut: Future<Output = anyhow::Result<arkret::EventsQueryOutcome>>,
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
        limited = outcome.has_more;
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
    provider: &ArkretAgentSessionProvider,
    client: &ArkretHttpClient,
    request: AccountScanCatchupRequest,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    if !account.has_requested_scope("ak.self.events.read.scan") {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            realm_id = %request.realm_id.as_str(),
            "arkret: limited account timeline cannot be scan-backfilled without ak.self.events.read.scan"
        );
        return Ok(());
    }

    let realm_id = request.realm_id.clone();
    let outcome =
        match collect_account_scan_catchup(request, |realm_id, before, limit| async move {
            client
                .inner()
                .events_read_outcome(
                    realm_id.as_str(),
                    before.as_deref(),
                    None,
                    None,
                    Some(limit),
                    None,
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
            apply_account_mls_commits_from_value_tree(
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
        provider,
        client,
        parsed,
        AccountInboundMode::Hydrate,
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
    let realm_id = update.realm_id.as_str().to_owned();
    let chat_type = realm_sync_chat_type(&update.entry);
    let participant_count = realm_sync_participant_count(&update.entry);
    let entry = match serde_json::to_value(update.entry) {
        Ok(entry) => entry,
        Err(_) => return ArkretInboundParseResult::default(),
    };
    let mut realms = serde_json::Map::new();
    realms.insert(realm_id, entry);
    let mut parsed = parse_delta_frame_for_account(&Value::Object(realms), account);
    for event in &mut parsed.events {
        event.chat_type.clone_from(&chat_type);
        event.participant_count = participant_count;
    }
    for skipped in &mut parsed.skipped {
        skipped.chat_type.clone_from(&chat_type);
        skipped.participant_count = participant_count;
    }
    parsed
}

fn record_account_realm_crypto_policy_from_update(
    update: &arkret::RealmUpdate,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> usize {
    let entry = match serde_json::to_value(&update.entry) {
        Ok(entry) => entry,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                realm_id = %update.realm_id.as_str(),
                "arkret: failed to serialize account realm policy projection: {err}"
            );
            return 0;
        }
    };
    let realms = Value::Object(serde_json::Map::from_iter([(
        update.realm_id.as_str().to_owned(),
        entry,
    )]));
    match crypto_store.update_realm_policies_from_sync(&realms) {
        Ok(updated) => updated,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                realm_id = %update.realm_id.as_str(),
                "arkret: failed to persist account realm crypto policy: {err:#}"
            );
            0
        }
    }
}

fn realm_sync_chat_type(entry: &arkret::RealmSyncEntry) -> Option<String> {
    if let Some(window_start) = entry.state_at_window_start.as_ref() {
        let is_direct = window_start
            .realm_metadata
            .collaboration_role
            .as_ref()
            .is_some_and(|role| {
                serde_json::to_value(role).ok().is_some_and(|value| {
                    value
                        .as_str()
                        .is_some_and(|role| role.eq_ignore_ascii_case("direct_conversation"))
                })
            });
        return Some(if is_direct { "dm" } else { "group" }.to_owned());
    }

    let realm_create = entry
        .timeline
        .iter()
        .flat_map(|container| &container.events)
        .chain(
            entry
                .state
                .iter()
                .chain(entry.state_after.iter())
                .flat_map(|container| &container.events),
        )
        .find(|event| event.kind.as_str() == "ak.realm.create")?;
    Some(
        if event_declares_direct_conversation_realm(realm_create) {
            "dm"
        } else {
            "group"
        }
        .to_owned(),
    )
}

fn event_declares_direct_conversation_realm(event: &arkret::Event) -> bool {
    if event.kind.as_str() != "ak.realm.create" {
        return false;
    }
    let object = event.payload.get("object");
    object
        .and_then(|value| value.get("fields"))
        .and_then(|value| value.get("collaboration_role"))
        .and_then(Value::as_str)
        .is_some_and(|role| role.eq_ignore_ascii_case("direct_conversation"))
        || object
            .and_then(|value| value.get("schema_refs"))
            .and_then(Value::as_array)
            .is_some_and(|refs| {
                refs.iter().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|profile| profile == "ak.profile.direct_conversation_realm.v1")
                })
            })
}

fn realm_sync_participant_count(entry: &arkret::RealmSyncEntry) -> Option<u32> {
    entry
        .summary
        .as_ref()
        .and_then(|summary| summary.joined_member_count)
        .and_then(|count| u32::try_from(count).ok())
}

fn parse_backfill_events_for_account(
    realm_id: &arkret::RealmId,
    events: Vec<arkret::Event>,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let update = arkret::RealmUpdate {
        realm_id: realm_id.clone(),
        entry: arkret::RealmSyncEntry {
            timeline: Some(arkret::Timeline {
                events,
                limited: false,
                prev_cursor: None,
                preview_only: None,
                ordered_log_conflicts: Vec::new(),
                extra: Default::default(),
            }),
            ..Default::default()
        },
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
    if let Some(timeline) = &update.entry.timeline {
        for event in &timeline.events {
            if let Ok(event) = serde_json::to_value(event) {
                recorded += record_account_mls_welcome_from_value_tree(
                    crypto_store,
                    &event,
                    channel,
                    account,
                    "realm_timeline",
                );
                recorded += apply_account_mls_commits_from_value_tree(
                    crypto_store,
                    &event,
                    channel,
                    account,
                    "realm_timeline",
                );
            }
        }
    }
    for state in [
        update.entry.state.as_ref(),
        update.entry.state_after.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for state_event in &state.events {
            if let Ok(state_event) = serde_json::to_value(state_event) {
                recorded += record_account_mls_welcome_from_value_tree(
                    crypto_store,
                    &state_event,
                    channel,
                    account,
                    "realm_state",
                );
                recorded += apply_account_mls_commits_from_value_tree(
                    crypto_store,
                    &state_event,
                    channel,
                    account,
                    "realm_state",
                );
            }
        }
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
    match crypto_store.record_direct_conversation_binding_from_value(value) {
        Ok(0) => {}
        Ok(recorded) => {
            debug!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                recorded,
                "arkret: recorded Direct Conversation MLS binding from account inbound event"
            );
        }
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                "arkret: failed to persist Direct Conversation MLS binding: {err:#}"
            );
        }
    }
    let Some(authorized_event_ref) = account.authorized_event_ref.as_deref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            source,
            "arkret: refusing MLS Welcome without current Agent key authorization"
        );
        return 0;
    };
    match crypto_store.validate_agent_mls_welcome_value_tree(
        value,
        &account.principal_id,
        &account.device_id,
        authorized_event_ref,
    ) {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                source,
                "arkret: refusing MLS Welcome with invalid Agent claim binding: {err:#}"
            );
            return 0;
        }
    }
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

fn apply_account_mls_commits_from_value_tree(
    crypto_store: &FileArkretCryptoStore,
    value: &Value,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    source: &'static str,
) -> usize {
    let mut commits = Vec::new();
    collect_typed_mls_commit_events(value, 8, &mut commits);
    let mut applied = 0;
    for (event_ref, payload) in commits {
        match crypto_store.apply_mls_commit(&payload, &event_ref) {
            Ok(true) => {
                applied += 1;
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    source,
                    event_id = %event_ref,
                    group_id = %payload.mls_group_id(),
                    epoch = payload.next_epoch(),
                    "arkret: applied accepted MLS Commit from account inbound event"
                );
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    source,
                    event_id = %event_ref,
                    group_id = %payload.mls_group_id(),
                    base_epoch = payload.base_epoch(),
                    next_epoch = payload.next_epoch(),
                    "arkret: failed to apply accepted MLS Commit: {err:#}"
                );
            }
        }
    }
    applied
}

fn collect_typed_mls_commit_events(
    value: &Value,
    remaining_depth: usize,
    commits: &mut Vec<(arkret::EventId, arkret::MlsCommitPayload)>,
) {
    let Value::Object(object) = value else {
        if remaining_depth > 0
            && let Value::Array(items) = value
        {
            for item in items {
                collect_typed_mls_commit_events(item, remaining_depth - 1, commits);
            }
        }
        return;
    };
    let kind = object.get("kind").and_then(Value::as_str);
    let event_ref = object
        .get("event_id")
        .or_else(|| object.get("eventId"))
        .and_then(Value::as_str)
        .and_then(|event_id| arkret::EventId::new(event_id.to_owned()).ok());
    if kind == Some("ak.mls.commit")
        && let Some(event_ref) = event_ref
        && let Some(payload) = find_typed_mls_commit_payload(value, remaining_depth)
    {
        commits.push((event_ref, payload));
        return;
    }
    if remaining_depth == 0 {
        return;
    }
    for item in object.values() {
        collect_typed_mls_commit_events(item, remaining_depth - 1, commits);
    }
}

fn find_typed_mls_commit_payload(
    value: &Value,
    remaining_depth: usize,
) -> Option<arkret::MlsCommitPayload> {
    if let Ok(payload) = serde_json::from_value::<arkret::MlsCommitPayload>(value.clone()) {
        return Some(payload);
    }
    if remaining_depth == 0 {
        return None;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_typed_mls_commit_payload(item, remaining_depth - 1)),
        Value::Object(object) => object
            .values()
            .find_map(|item| find_typed_mls_commit_payload(item, remaining_depth - 1)),
        _ => None,
    }
}

async fn handle_parsed_account_events(
    provider: &ArkretAgentSessionProvider,
    client: &ArkretHttpClient,
    parsed: ArkretInboundParseResult,
    inbound_mode: AccountInboundMode,
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
                    provider,
                    client,
                    &skipped,
                    inbound_mode,
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
                    if inbound_mode.suppresses_agent_dispatch() {
                        update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
                            diagnostic.baselined_events =
                                diagnostic.baselined_events.saturating_add(1);
                        });
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
        if inbound_mode == AccountInboundMode::Trigger {
            hydrate_conversation_before_trigger(
                provider,
                client,
                &event,
                channel,
                account,
                account_store,
                crypto_store,
                gateway_channel,
                session_store,
            )
            .await?;
        }
        let event_id = event.event_id.clone();
        let conversation = event.strand_id.as_ref().map(|strand_id| {
            crate::arkret_delivery::RemoteConversationKey {
                channel_config_id: channel.id.clone(),
                account_id: account.id.clone(),
                realm_id: event.realm_id.clone(),
                strand_id: strand_id.clone(),
            }
        });
        if inbound_mode == AccountInboundMode::Hydrate {
            if let Some(conversation) = conversation {
                crate::arkret_delivery::ArkretExecutionBindingStore::new(
                    &gateway_channel.config().savfox_home,
                )
                .hydrate_event(
                    conversation,
                    crate::arkret_delivery::RemoteContextEvent {
                        event_id: event.event_id.clone(),
                        sender_did: event.sender_did.clone(),
                        sender_kind: if event.sender_did.eq_ignore_ascii_case(&account.principal_id)
                        {
                            "agent".to_owned()
                        } else {
                            "human".to_owned()
                        },
                        body: event.body.clone(),
                        received_at: Utc::now(),
                    },
                )
                .await?;
            }
        }
        if event
            .sender_did
            .eq_ignore_ascii_case(account.principal_id.trim())
        {
            if inbound_mode == AccountInboundMode::Trigger {
                let _ = crate::arkret_delivery::ArkretExecutionBindingStore::new(
                    &gateway_channel.config().savfox_home,
                )
                .acknowledge_echo(&event_id)
                .await?;
            }
            remember_account_event(account_store, &event_id).await?;
            continue;
        }
        if inbound_mode.suppresses_agent_dispatch() {
            remember_account_event(account_store, &event_id).await?;
            update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
                diagnostic.baselined_events = diagnostic.baselined_events.saturating_add(1);
            });
            debug!(
                channel_id = %channel.id,
                account_id = %account.id,
                event_id = %event_id,
                "arkret: recorded non-triggering history event without agent dispatch"
            );
            continue;
        }
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

#[allow(clippy::too_many_arguments)]
async fn hydrate_conversation_before_trigger(
    provider: &ArkretAgentSessionProvider,
    client: &ArkretHttpClient,
    trigger: &ArkretInboundEvent,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let Some(strand_id) = trigger.strand_id.as_ref() else {
        return Ok(());
    };
    let conversation = crate::arkret_delivery::RemoteConversationKey {
        channel_config_id: channel.id.clone(),
        account_id: account.id.clone(),
        realm_id: trigger.realm_id.clone(),
        strand_id: strand_id.clone(),
    };
    let delivery_store = crate::arkret_delivery::ArkretExecutionBindingStore::new(
        &gateway_channel.config().savfox_home,
    );
    let snapshot = delivery_store.remote_snapshot(&conversation).await?;
    if !snapshot.events.is_empty() || snapshot.history_unavailable.is_some() {
        return Ok(());
    }
    if !account.has_requested_scope("ak.self.events.read.scan") {
        delivery_store
            .mark_history_unavailable(
                conversation,
                "history_unavailable: account lacks ak.self.events.read.scan",
            )
            .await?;
        return Ok(());
    }
    let realm_id = RealmId::new(trigger.realm_id.clone())?;
    let outcome = match client
        .inner()
        .events_read_outcome(
            realm_id.as_str(),
            None,
            None,
            None,
            Some(ACCOUNT_SCAN_CATCHUP_LIMIT),
            None,
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            delivery_store
                .mark_history_unavailable(
                    conversation,
                    &format!("history_unavailable: bounded events_read failed: {error}"),
                )
                .await?;
            return Ok(());
        }
    };
    let parsed = parse_backfill_events_for_account(&realm_id, outcome.events, account);
    // Boxing is intentional: hydration reuses the exact signature/decryption
    // pipeline while the `Hydrate` mode prevents this recursive pass from
    // triggering an agent turn or another history query.
    Box::pin(handle_parsed_account_events(
        provider,
        client,
        parsed,
        AccountInboundMode::Hydrate,
        channel,
        account,
        account_store,
        crypto_store,
        gateway_channel,
        session_store,
    ))
    .await
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
    let strand_id = event.strand_id.clone();
    let thread_id = event.thread_root_id.clone();
    let event_id = event.event_id.clone();
    let mentioned_actor_ids = event.mentioned_actor_ids.clone();
    let sidecar_exchange = event.sidecar_exchange.clone();
    let chat_type = event.chat_type;
    let participant_count = event.participant_count;
    let body = event.body;
    // DIDs carry no external-bot localpart convention, but we can at least mark
    // the account's own DID as SelfBot so the runtime never replies to its own
    // echoed messages.
    let sender_kind = if sender.eq_ignore_ascii_case(account.principal_id.trim()) {
        runtime::SenderKind::SelfBot
    } else {
        runtime::SenderKind::Human
    };

    // A verified Sidecar exchange request addressed to this principal is an
    // explicit call: mark the runtime as mentioned and thread the exchange
    // identity through `reply_target` so the user-visible reply can carry the
    // `role=user_facing_response` binding (zh/models/sidecar.md §7.2.1).
    let reply_target = match (&sidecar_exchange, &strand_id) {
        (Some(context), Some(strand)) => Some(encode_sidecar_reply_target(strand, context)),
        _ => strand_id.clone(),
    };
    let mut start_meta = runtime::StartThreadMeta {
        peer_id: Some(sender.clone()),
        routing_channel_id: Some(format!("{}:{}:{}", config_id, account.id, realm_id)),
        routing_group_id: Some(realm_id.clone()),
        routing_thread_id: strand_id.clone(),
        group_id: (!matches!(chat_type.as_deref(), Some("dm"))).then(|| realm_id.clone()),
        thread_id,
        reply_target,
        account_id: Some(account.id.clone()),
        chat_type,
        saved_channel_config_id: Some(config_id.clone()),
        remote_realm_id: Some(realm_id.clone()),
        remote_strand_id: strand_id.clone(),
        remote_event_id: Some(event_id.clone()),
        remote_agent_did: Some(account.principal_id.clone()),
        delivery_mode: Some(channel.delivery_mode.clone()),
        sender_kind,
        is_mentioned: sidecar_exchange.is_some(),
        participant_count,
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
    provider: &ArkretAgentSessionProvider,
    client: &ArkretHttpClient,
    skipped: &ArkretInboundSkippedEvent,
    inbound_mode: AccountInboundMode,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<bool> {
    if arkret_sender_is_account_principal(skipped.sender_did.as_deref(), account)
        && inbound_mode != AccountInboundMode::Hydrate
    {
        if inbound_mode == AccountInboundMode::Trigger
            && let Some(event_id) = skipped.event_id.as_deref()
        {
            let _ = crate::arkret_delivery::ArkretExecutionBindingStore::new(
                &gateway_channel.config().savfox_home,
            )
            .acknowledge_echo(event_id)
            .await?;
        }
        debug!(
            account_id = %account.id,
            event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
            "arkret: ignored the Agent's own encrypted event echo before MLS decrypt"
        );
        return Ok(true);
    }
    if skipped.reason == ArkretInboundSkipReason::SidecarExchangeControl {
        // Controller-authored exchange control never reaches the agent; it only
        // folds durable terminal state (§7.2.3).
        fold_sidecar_exchange_control(skipped, crypto_store, channel, account, gateway_channel)
            .await?;
        return Ok(true);
    }
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
            if inbound_mode == AccountInboundMode::Baseline {
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    event_id = skipped.event_id.as_deref().unwrap_or("<unknown>"),
                    "arkret: decrypted initial catch-up event for MLS progression without agent dispatch"
                );
                return Ok(true);
            }
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
            if inbound_mode == AccountInboundMode::Hydrate {
                if let Some(strand_id) = skipped.strand_id.as_ref() {
                    crate::arkret_delivery::ArkretExecutionBindingStore::new(
                        &gateway_channel.config().savfox_home,
                    )
                    .hydrate_event(
                        crate::arkret_delivery::RemoteConversationKey {
                            channel_config_id: channel.id.clone(),
                            account_id: account.id.clone(),
                            realm_id: realm_id.clone(),
                            strand_id: strand_id.clone(),
                        },
                        crate::arkret_delivery::RemoteContextEvent {
                            event_id: event_id.clone(),
                            sender_did: sender_did.clone(),
                            sender_kind: if sender_did.eq_ignore_ascii_case(&account.principal_id) {
                                "agent".to_owned()
                            } else {
                                "human".to_owned()
                            },
                            body,
                            received_at: Utc::now(),
                        },
                    )
                    .await?;
                }
                return Ok(true);
            }
            let sidecar_exchange = match consume_sidecar_exchange_binding(
                provider,
                skipped,
                crypto_store,
                channel,
                account,
                gateway_channel,
                &event_id,
            )
            .await?
            {
                SidecarConsumeOutcome::NoBinding => None,
                SidecarConsumeOutcome::DropSilently => return Ok(true),
                SidecarConsumeOutcome::Execute(context) => Some(context),
            };
            dispatch_to_agent(
                ArkretInboundEvent {
                    account_id: skipped.account_id.clone(),
                    event_id,
                    realm_id,
                    chat_type: skipped.chat_type.clone(),
                    participant_count: skipped.participant_count,
                    strand_id: skipped.strand_id.clone(),
                    sender_did,
                    body,
                    thread_root_id: skipped.reply_to.clone(),
                    mentioned_actor_ids: Vec::new(),
                    sidecar_exchange,
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
                UnableToDecryptReason::NoSession,
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
                UnableToDecryptReason::BadCiphertext,
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
                UnableToDecryptReason::BadCiphertext,
            );
            Ok(false)
        }
    }
}

/// Disposition of one decrypted inbound Event with respect to the Agent
/// Sidecar exchange consumption gate (`zh/models/sidecar.md` §7.2.1–§7.2.2).
enum SidecarConsumeOutcome {
    /// No (valid) exchange binding: handle as an ordinary private message.
    NoBinding,
    /// The request must be treated as nonexistent or already observed: do not
    /// execute, do not surface an error (the Event stays acknowledged).
    DropSilently,
    /// Every §7.2.2 gate passed: dispatch the request with this verified
    /// exchange identity so the reply can carry the `user_facing_response`
    /// binding.
    Execute(SidecarExchangeContext),
}

fn refreshed_grant_matches_account(
    state: &garth::SessionGrantState,
    account: &ArkretAccountConfig,
    required_scope: &str,
) -> bool {
    state.principal_id.as_str() == account.principal_id
        && state
            .device_id
            .as_ref()
            .is_some_and(|device_id| device_id.as_str() == account.device_id)
        && state
            .granted_scope
            .iter()
            .any(|scope| scope == required_scope)
}

/// Re-prove this runtime's authorization immediately before a sensitive
/// action. This is an **agent-level** gate and has nothing to do with Sidecar.
///
/// A successful grant refresh is the registered protocol proof of the whole
/// authorization chain, not a convenience: the Auth Server re-fetches the
/// authoritative Agent view and refuses to issue for a `paused` or
/// `deactivated` agent (`zh/models/actor.md` "Lifecycle", which also binds
/// already-issued sessions inside a ≤60 s freshness window), and it revalidates
/// the proof against the current non-revoked runtime key
/// (`zh/identity/key-management.md` §3.6.1, where a replaced key's sessions
/// fail closed inside the revocation freshness window rather than living out
/// their TTL).
///
/// The identity/scope comparison afterwards is what makes the refreshed grant
/// evidence about *this* runtime: a grant that came back bound to another
/// principal, another device, or without the required scope proves nothing.
async fn ensure_fresh_runtime_authorization(
    provider: &ArkretAgentSessionProvider,
    account: &ArkretAccountConfig,
    required_scope: &str,
) -> anyhow::Result<()> {
    provider
        .refresh_for_sensitive_action()
        .await
        .map_err(|error| {
            anyhow::anyhow!("Arkret runtime authorization could not be revalidated: {error}")
        })?;
    let Some(state) = provider.session().current_state() else {
        anyhow::bail!("Arkret runtime authorization refresh produced no session grant");
    };
    anyhow::ensure!(
        refreshed_grant_matches_account(&state, account, required_scope),
        "refreshed Arkret runtime grant lost its identity binding or {required_scope} scope"
    );
    Ok(())
}

/// Decrypt and fold one `ak.agent.sidecar.exchange.control` Event.
///
/// The plaintext is readable here because this runtime is an MLS member of the
/// Sidecar backing Circle, which is exactly the population §7.2.3 addresses.
/// The Event is never dispatched to the agent: its only effect is durable
/// terminal state, which is what stops a later request or a cached reply
/// context from executing.
async fn fold_sidecar_exchange_control(
    skipped: &ArkretInboundSkippedEvent,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    gateway_channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    if !account_allows_event_read(account) {
        return Ok(());
    }
    let (Some(controller_id), Some(actor_id), Some(strand_id), Some(payload), Some(event_id)) = (
        account.controller_id.as_deref(),
        skipped.sender_did.as_deref(),
        skipped.strand_id.as_deref(),
        skipped.encrypted_payload.as_ref(),
        skipped.event_id.as_deref(),
    ) else {
        return Ok(());
    };
    let Ok(ArkretDecryptDetailedOutcome::Decrypted {
        content: plaintext, ..
    }) = crypto_store.try_decrypt_content_block_detailed(payload)
    else {
        debug!(
            account_id = %account.id,
            event_id,
            "arkret: Sidecar exchange control could not be decrypted; leaving exchange state unchanged"
        );
        return Ok(());
    };
    // The outer payload strand and the delivery strand are the same value here
    // because the delivery strand is read from that payload; passing both keeps
    // the §7.2.3 check owned by the gate instead of implied by the caller.
    let Some(control) =
        gate_inbound_exchange_control(&plaintext, strand_id, strand_id, actor_id, controller_id)
    else {
        debug!(
            account_id = %account.id,
            event_id,
            "arkret: Sidecar exchange control failed closed consumer validation; not folded"
        );
        return Ok(());
    };
    let store = SidecarExchangeStore::for_account(
        &gateway_channel.config().savfox_home,
        &channel.id,
        &account.id,
    );
    match store.record_terminal_control(controller_id, strand_id, &control, event_id)? {
        SidecarTerminalAdmission::Recorded => {
            info!(
                account_id = %account.id,
                event_id,
                exchange_id = %control.exchange_id.as_str(),
                "arkret: Sidecar exchange closed by controller control Event"
            );
        }
        SidecarTerminalAdmission::AlreadyTerminal { control_event_id } => {
            debug!(
                account_id = %account.id,
                event_id,
                terminal_control_event_id = %control_event_id,
                "arkret: Sidecar exchange already terminal; later control ignored"
            );
        }
        SidecarTerminalAdmission::NotCanonicalRequest => {
            debug!(
                account_id = %account.id,
                event_id,
                "arkret: Sidecar exchange control does not name the canonical request; not folded"
            );
        }
        SidecarTerminalAdmission::NotTerminal => {
            debug!(
                account_id = %account.id,
                event_id,
                "arkret: Sidecar coordinator reassignment does not change terminal state"
            );
        }
    }
    Ok(())
}

/// Decrypt the `encrypted_metadata` carrier (same MLS group as the content
/// carrier), extract the exchange binding fail-closed, and apply the §7.2.2
/// consumption gate. The runtime obtains exchange identity only from this
/// binding — never from `reply_to`, message bodies or arrival order.
///
/// The gate is the conjunction of five locally decidable facts. No
/// Agent-runtime-facing Sidecar read exists (§3.2 scopes get/list to the
/// controller) and none is needed, because §7.2.2 asks the runtime to
/// *re-verify*, not to fetch a single combined proof:
///
/// 1. **Controller authorship** — the Event actor is this runtime's controller. §7.2.1: a request
///    binding carried by a non-controller actor is wholly invalid, so a sibling Agent in the same
///    backing Circle cannot drive this runtime even though it can decrypt the Event.
/// 2. **Addressed** — this principal is in `addressed_agent_ids`.
/// 3. **Canonical request identity and local idempotency** — the scoped `(controller, private
///    strand, exchange)` audit admits exactly one request Event and fails closed on any other,
///    applying the `actor_seq` / `event_digest` canonical rule.
/// 4. **Non-terminal exchange** — no valid terminal control Event has closed it.
/// 5. **Runtime authorization freshness** — see [`ensure_fresh_runtime_authorization`].
///
/// Effective access and target-device MLS readiness are not separate fetches:
/// the request was decrypted with the group's current epoch key, and §7.3
/// requires the server to stop addressing, delivery and new write admission for
/// an Agent the moment revoke/pause/deactivate becomes accepted state. An old
/// key cannot open a newly delivered request, and a reply from a runtime that
/// lost access cannot pass write admission.
async fn consume_sidecar_exchange_binding(
    provider: &ArkretAgentSessionProvider,
    skipped: &ArkretInboundSkippedEvent,
    crypto_store: &FileArkretCryptoStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    gateway_channel: &Arc<GatewayChannel>,
    event_id: &str,
) -> anyhow::Result<SidecarConsumeOutcome> {
    let Some(metadata_payload) = skipped.encrypted_metadata_payload.as_ref() else {
        return Ok(SidecarConsumeOutcome::NoBinding);
    };
    let Ok(ArkretDecryptDetailedOutcome::Decrypted {
        content: metadata_plaintext,
        ..
    }) = crypto_store.try_decrypt_content_block_detailed(metadata_payload)
    else {
        // Fail closed to "no binding": the message is handled as an
        // ordinary private message and never as an exchange participant.
        debug!(
            account_id = %account.id,
            event_id,
            "arkret: encrypted_metadata carrier could not be decrypted; treating event as non-exchange"
        );
        return Ok(SidecarConsumeOutcome::NoBinding);
    };
    let Some(binding) = sidecar_binding_from_metadata_plaintext(&metadata_plaintext) else {
        return Ok(SidecarConsumeOutcome::NoBinding);
    };
    let Some(controller_id) = account.controller_id.as_deref() else {
        // Without a known controller the §7.2.1 authorship check cannot be
        // evaluated, so the request is indistinguishable from nonexistent.
        warn!(
            account_id = %account.id,
            event_id,
            "arkret: Sidecar request cannot be authenticated without a configured controllerId; failing closed"
        );
        return Ok(SidecarConsumeOutcome::DropSilently);
    };
    let Some(actor_id) = skipped.sender_did.as_deref() else {
        return Ok(SidecarConsumeOutcome::DropSilently);
    };
    match gate_inbound_request_binding(
        &binding,
        event_id,
        actor_id,
        controller_id,
        &account.principal_id,
    ) {
        SidecarRequestGate::NotARequest => Ok(SidecarConsumeOutcome::NoBinding),
        SidecarRequestGate::NotController => {
            // §7.2.1: a request binding carried by a non-controller actor is
            // wholly invalid. Another Agent of the same backing Circle can
            // decrypt it, and must still treat it as nonexistent.
            debug!(
                account_id = %account.id,
                event_id,
                "arkret: Sidecar request binding was not authored by this runtime's controller; treating request as nonexistent"
            );
            Ok(SidecarConsumeOutcome::DropSilently)
        }
        SidecarRequestGate::NotAddressed => {
            // §7.2.2: a non-addressed member treats the request as
            // nonexistent even though it can decrypt it.
            debug!(
                account_id = %account.id,
                event_id,
                reason = ?ArkretInboundSkipReason::SidecarNotAddressed,
                "arkret: Sidecar request not addressed to this principal; treating request as nonexistent"
            );
            Ok(SidecarConsumeOutcome::DropSilently)
        }
        SidecarRequestGate::Addressed(context) => {
            let Some(private_strand_id) = skipped.strand_id.as_deref() else {
                return Ok(SidecarConsumeOutcome::DropSilently);
            };
            let Some(ordering) = skipped.request_ordering.as_ref() else {
                // Without the envelope ordering keys the canonical-request rule
                // is undecidable, so the request is not admissible.
                warn!(
                    account_id = %account.id,
                    event_id,
                    "arkret: Sidecar request carries no canonical ordering keys; failing closed"
                );
                return Ok(SidecarConsumeOutcome::DropSilently);
            };
            let store = SidecarExchangeStore::for_account(
                &gateway_channel.config().savfox_home,
                &channel.id,
                &account.id,
            );
            match store.record_request_identity(
                controller_id,
                private_strand_id,
                &context.exchange_id,
                &context.request_event_id,
                ordering,
            )? {
                SidecarExchangeAdmission::Recorded => {}
                SidecarExchangeAdmission::AlreadyObserved => {
                    debug!(
                        account_id = %account.id,
                        event_id,
                        exchange_id = %context.exchange_id,
                        "arkret: Sidecar request identity already observed; skipping exact replay"
                    );
                    return Ok(SidecarConsumeOutcome::DropSilently);
                }
                SidecarExchangeAdmission::Conflict {
                    canonical_request_event_id,
                } => {
                    warn!(
                        account_id = %account.id,
                        event_id,
                        exchange_id = %context.exchange_id,
                        canonical_request_event_id = %canonical_request_event_id,
                        "arkret: scoped Sidecar exchange id maps to a different request event; failing closed"
                    );
                    return Ok(SidecarConsumeOutcome::DropSilently);
                }
                SidecarExchangeAdmission::Terminal { control_event_id } => {
                    debug!(
                        account_id = %account.id,
                        event_id,
                        exchange_id = %context.exchange_id,
                        terminal_control_event_id = %control_event_id,
                        "arkret: Sidecar exchange is already terminal; refusing to execute a new request"
                    );
                    return Ok(SidecarConsumeOutcome::DropSilently);
                }
            }

            if let Err(error) =
                ensure_fresh_runtime_authorization(provider, account, "ak.event.read").await
            {
                warn!(
                    account_id = %account.id,
                    event_id,
                    %error,
                    "arkret: Sidecar request authorization freshness failed; failing closed"
                );
                return Ok(SidecarConsumeOutcome::DropSilently);
            }
            Ok(SidecarConsumeOutcome::Execute(context))
        }
    }
}

fn arkret_sender_is_account_principal(
    sender_did: Option<&str>,
    account: &ArkretAccountConfig,
) -> bool {
    sender_did.is_some_and(|sender| sender.eq_ignore_ascii_case(account.principal_id.trim()))
}

fn record_account_unable_to_decrypt(
    crypto_store: &FileArkretCryptoStore,
    skipped: &ArkretInboundSkippedEvent,
    payload: arkret::EncryptedPayload,
    reason: UnableToDecryptReason,
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
    savfox_home: &std::path::Path,
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
        .inkson_bootstrap
        .as_ref()
        .map(|bootstrap| bootstrap.service_id.to_string())
        .or_else(|| channel.service_id.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret agent '{}' missing serviceId for agent_key_proof audience",
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
        principal.clone(),
        verification_method,
        authorization_ref,
        account.requested_scope.clone(),
        &audience,
        device_id.clone(),
        None,
    )
    .await?;
    savfox_channels::arkret::save_verified_runtime_scope(
        savfox_home,
        &channel.id,
        account,
        runtime_public_key_digest.clone(),
    )
    .await?;
    info!(
        "arkret: agent '{}' obtained DPoP-bound session; audience='{}' expires_at='{}'",
        account.id, audience, session.expires_at
    );
    Ok(provider)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountActorChains {
    #[serde(default)]
    realms: HashMap<String, AccountActorChainHead>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountActorChainHead {
    next_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_event_id: Option<String>,
}

fn account_actor_chain_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Arkret actor chains are Realm-scoped and contiguous: the first Event uses
/// sequence 0, and each later Event names the preceding Event in `prev_refs`.
/// Persist both pieces together so a restart cannot turn a valid sequence into
/// an unreferenced high-water mark.
fn account_actor_chain_path(savfox_home: &std::path::Path, account_id: &str) -> PathBuf {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("arkret-account-chain");
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
    dir.join(format!("{safe_id}.json"))
}

async fn load_account_actor_chains(path: &std::path::Path) -> anyhow::Result<AccountActorChains> {
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.is_empty() => Ok(AccountActorChains::default()),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse Arkret actor chain state {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(AccountActorChains::default())
        }
        Err(error) => {
            Err(error).with_context(|| format!("read Arkret actor chain state {}", path.display()))
        }
    }
}

async fn save_account_actor_chains(
    path: &std::path::Path,
    chains: &AccountActorChains,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(chains).context("serialize Arkret actor chain state")?;
    savfox_utils::fs::write_atomically_async(path, bytes, Some(0o600))
        .await
        .with_context(|| format!("persist Arkret actor chain state {}", path.display()))
}

fn load_account_actor_chain_head(
    path: &std::path::Path,
    realm_id: &str,
) -> anyhow::Result<Option<AccountActorChainHead>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => {
            let state: AccountActorChains = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse Arkret actor chain state {}", path.display()))?;
            Ok(state.realms.get(realm_id).cloned())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read Arkret actor chain state {}", path.display()))
        }
    }
}

/// Maps one durable Garth queue item onto the Arkret submit endpoint.
struct AccountOutboundEncryptionFence {
    crypto_store: FileArkretCryptoStore,
    actor_chain_path: PathBuf,
}

impl OutboundGenerationFence for AccountOutboundEncryptionFence {
    fn evaluate(
        &self,
        item: &garth::sync_client::SendQueueItem,
    ) -> garth::Result<OutboundGenerationFenceDecision> {
        let requires_e2ee = self
            .crypto_store
            .realm_requires_e2ee(item.realm_id.as_str())
            .map_err(|error| {
                garth::Error::Protocol(format!(
                    "load Arkret realm encryption policy before submit: {error:#}"
                ))
            })?;
        let Some(event) = item.content.get("event") else {
            return Ok(OutboundGenerationFenceDecision::Quarantine {
                reason: format!(
                    "queued Arkret Event for {} lacks an EventInitialSubmission wrapper",
                    item.realm_id.as_str()
                ),
            });
        };
        let is_message = event
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "ak.message.create");
        let is_encrypted = event
            .get("payload")
            .and_then(|payload| payload.get("encrypted_content"))
            .is_some();
        let has_agent_context = event
            .get("payload")
            .and_then(|payload| payload.get("agent_context"))
            .is_some();

        if is_message && !has_agent_context {
            return Ok(OutboundGenerationFenceDecision::Quarantine {
                reason: format!(
                    "queued Agent message lacks the required auditable agent_context for {}",
                    item.realm_id.as_str()
                ),
            });
        }

        let actor_seq = event.get("actor_seq").and_then(Value::as_u64);
        let chain_head = load_account_actor_chain_head(
            &self.actor_chain_path,
            item.realm_id.as_str(),
        )
        .map_err(|error| {
            garth::Error::Protocol(format!("load Arkret actor chain before submit: {error:#}"))
        })?;
        let belongs_to_current_actor_chain = match (actor_seq, chain_head) {
            (Some(0), None) => true,
            (Some(seq), Some(head)) => seq < head.next_seq,
            _ => false,
        };
        if !belongs_to_current_actor_chain {
            return Ok(OutboundGenerationFenceDecision::Quarantine {
                reason: format!(
                    "queued Event does not belong to the current contiguous actor chain for {}",
                    item.realm_id.as_str()
                ),
            });
        }

        if requires_e2ee && is_message && !is_encrypted {
            return Ok(OutboundGenerationFenceDecision::Quarantine {
                reason: format!(
                    "queued plaintext message violates the current E2EE-required policy for {}",
                    item.realm_id.as_str()
                ),
            });
        }
        Ok(OutboundGenerationFenceDecision::Current)
    }
}

struct AccountOutboundSubmitter {
    client: ArkretHttpClient,
}

impl OutboundSubmitter for AccountOutboundSubmitter {
    fn submit<'a>(
        &'a self,
        item: garth::sync_client::SendQueueItem,
    ) -> garth::outbound::BoxOutboundFuture<'a, OutboundSubmitOutcome> {
        Box::pin(async move {
            let submission: EventInitialSubmission =
                serde_json::from_value(item.content).map_err(|error| {
                    garth::Error::Protocol(format!(
                        "decode queued Arkret initial submission: {error}"
                    ))
                })?;
            if item.authorization_lease.as_ref() != submission.authorization_lease.as_ref() {
                return Err(garth::Error::Protocol(
                    "queued Arkret submission does not match its bound AuthorizationLease"
                        .to_owned(),
                ));
            }
            let response = match self.client.submit_initial(&submission).await {
                Ok(response) => response,
                Err(error) => {
                    warn!(
                        transaction_id = %item.transaction_id,
                        error = %error,
                        "arkret: outbound event submission failed; scheduling retry"
                    );
                    return Ok(OutboundSubmitOutcome::RetryAfter {
                        delay: Duration::from_secs(1),
                        reason: error.to_string(),
                    });
                }
            };
            if let Some(event_id) = response.accepted.into_iter().next() {
                return Ok(OutboundSubmitOutcome::Accepted {
                    event_id,
                    ingress_receipts: response.ingress_receipts,
                });
            }
            if let Some(event_id) = response.duplicate.into_iter().next() {
                return Ok(OutboundSubmitOutcome::Duplicate {
                    event_id,
                    ingress_receipts: response.ingress_receipts,
                });
            }
            if !response.rejected.is_empty() {
                warn!(
                    transaction_id = %item.transaction_id,
                    rejected = ?response.rejected,
                    "arkret: outbound event submission was rejected"
                );
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
/// `realm_id` selects the outbound Arkret realm. `strand_id` must come from the
/// inbound Arkret context that triggered the reply; it is not configured on the
/// channel.
pub(crate) async fn send_to_arkret_account(
    savfox_home: &std::path::PathBuf,
    realm_id: &str,
    strand_id: Option<&str>,
    body: &str,
    sidecar_exchange: Option<&SidecarExchangeContext>,
    saved_channel_config_id: Option<&str>,
    expected_account_id: Option<&str>,
    delivery: Option<&crate::arkret_delivery::DeliveryCorrelation>,
) -> anyhow::Result<String> {
    let Some((channel, account)) = resolve_arkret_outbound_account_for_binding(
        savfox_home,
        realm_id,
        saved_channel_config_id,
        expected_account_id,
    )
    .await?
    else {
        anyhow::bail!(
            "no Arkret channel configured for realm {realm_id} and routed config {saved_channel_config_id:?}"
        );
    };
    if !account.has_requested_scope("ak.self.events.command.submit") {
        anyhow::bail!(
            "Arkret account '{}' send=true but missing service scope ak.self.events.command.submit; refusing to call submit endpoint",
            account.id
        );
    }
    let strand_id = strand_id.map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret account '{}' cannot send without an inbound Arkret strand id",
            account.id
        )
    })?;

    // One-shot send restores the same keyring-backed session grant as the
    // listener and participates in the shared refresh/client rebuild path.
    if let Some(context) = sidecar_exchange {
        // Defense in depth for work already queued when the controller closed
        // the exchange: a cached context must not author a response into a
        // terminal exchange, and it must still be the canonical request
        // (§7.2.2/§7.2.3). The terminal fact is durable and controller-authored,
        // so this is decided locally, not inferred from elapsed time — and it
        // runs before the network, the session, the actor chain and the store.
        let controller_id = account.controller_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret account '{}' cannot answer a Sidecar exchange without a configured controllerId",
                account.id
            )
        })?;
        let store = SidecarExchangeStore::for_account(savfox_home, &channel.id, &account.id);
        anyhow::ensure!(
            store.exchange_accepts_new_response(
                controller_id,
                &strand_id,
                &context.exchange_id,
                &context.request_event_id,
            )?,
            "Arkret Sidecar exchange {} no longer accepts a response from this runtime",
            context.exchange_id
        );
    }
    let provider = construct_account_provider(savfox_home, &channel, &account).await?;
    // Authorization freshness is an agent-level precondition of writing at all,
    // Sidecar or not: work queued before a pause/revoke must not still be
    // publishable afterwards. It runs before session reuse, actor-sequence
    // mutation, encryption, queueing and submission.
    ensure_fresh_runtime_authorization(&provider, &account, "ak.self.events.command.submit")
        .await?;
    let inner = provider
        .provide()
        .await
        .map_err(|error| anyhow::anyhow!("build authenticated Arkret client: {error}"))?;
    let client = ArkretHttpClient::from_inner(inner);
    let outbound_store = open_account_store(
        savfox_home,
        &channel.id,
        &account.id,
        ACCOUNT_EVENT_DEDUPE_MAX,
    )?;
    outbound_store.ensure_created().await?;
    let actor_chain_path = account_actor_chain_path(savfox_home, &account.id);
    let actor_chain_guard = account_actor_chain_lock().lock().await;
    let mut actor_chains = load_account_actor_chains(&actor_chain_path).await?;
    let previous_actor_chains = actor_chains.clone();
    let actor_chain_head = actor_chains
        .realms
        .get(realm_id)
        .cloned()
        .unwrap_or_default();
    let actor_seq = actor_chain_head.next_seq;
    let request = MessageCreateRequest {
        realm_id: realm_id.to_owned(),
        strand_id,
        body: body.to_owned(),
        principal_id: account.principal_id.clone(),
        actor_seq,
        thread_root_id: None,
        sidecar_exchange: sidecar_exchange.cloned(),
    };
    let mut event = build_message_create_event(&request)?;
    if let Some(delivery) = delivery {
        // The checkpoint id is also the stable event id. Rebuilding a queued
        // checkpoint after a crash therefore reaches Arkret as a duplicate,
        // not as a second public delivery.
        event.event_id = EventId::new(format!("ak:event:{}", delivery.checkpoint_id))?;
        event.refs.push(EventRef::new(
            EventId::new(delivery.source_event_id.clone())?.to_string(),
            "after",
        ));
    }
    event.payload.insert(
        "agent_context".to_owned(),
        json!({
            "agent_id": account.principal_id,
            "operator_or_controller": "derived_from_direct_conversation_binding",
            "execution_purpose": if delivery.is_some() { "task_delivery_checkpoint" } else { "direct_conversation_reply" },
            "authorization_ref": realm_id,
        }),
    );
    if let Some(previous_event_id) = actor_chain_head.last_event_id {
        event.prev_refs.push(
            EventId::new(previous_event_id)
                .context("persisted Arkret actor chain contains an invalid Event id")?,
        );
    }
    let realm_id_typed = RealmId::new(realm_id.to_owned())?;
    let frontier = client
        .inner()
        .events_frontier(&EventsFrontierSelector::RealmSeal {
            realm_id: realm_id_typed.clone(),
        })
        .await
        .map_err(|error| anyhow::anyhow!("fetch current Arkret Realm Seal: {error}"))?;
    let EventsFrontierView::RealmSeal(frontier) = frontier.frontier else {
        anyhow::bail!("Arkret Realm Seal frontier response changed selector variant");
    };
    apply_data_event_basis(
        &mut event,
        frontier.seal_id,
        Did::new(account.principal_id.clone())?,
        account.device_id.clone(),
    )?;
    let crypto_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &account.id);
    apply_account_outbound_encryption(
        &crypto_store,
        realm_id,
        &mut event,
        sidecar_exchange,
        delivery,
    )?;

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

    let prepared_event = PreparedStandardEvent::from(
        PreparedDataEvent::try_from(event)
            .map_err(|error| anyhow::anyhow!("prepare outbound Arkret DataEvent: {error}"))?,
    );

    // Online publication uses a structurally validated wrapper without an
    // AuthorizationLease. A custom provider may still return a lease-bound
    // delayed-publication wrapper. In either case, validate the exact wrapper
    // before advancing the actor chain or enqueueing durable work.
    let submission = client.prepare_initial_submission(&prepared_event).await?;
    let authorization_lease = submission.authorization_lease.clone();
    let mut transaction_id = prepared_event.event().event_id.to_string();
    let next_actor_seq = actor_seq
        .checked_add(1)
        .context("Arkret actor sequence exhausted")?;
    actor_chains.realms.insert(
        realm_id.to_owned(),
        AccountActorChainHead {
            next_seq: next_actor_seq,
            last_event_id: Some(transaction_id.clone()),
        },
    );
    save_account_actor_chains(&actor_chain_path, &actor_chains).await?;
    let outbound = OutboundEngine::new(outbound_store.clone());
    let fence = AccountOutboundEncryptionFence {
        crypto_store: crypto_store.clone(),
        actor_chain_path: actor_chain_path.clone(),
    };
    let queued_transaction_id = transaction_id.clone();
    let submission_value = serde_json::to_value(&submission)?;
    if let Err(error) = outbound_store
        .mutate_outbound(move |queue| {
            let item = queue.enqueue(
                Some(queued_transaction_id),
                realm_id_typed,
                garth::sync_client::SendQueueItemKind::Message,
                submission_value,
                Vec::new(),
            )?;
            if let Some(authorization_lease) = authorization_lease {
                queue.bind_authorization_lease(&item.transaction_id, authorization_lease)?;
            }
            queue.get(&item.transaction_id).cloned().ok_or_else(|| {
                garth::Error::Protocol(
                    "Arkret outbound queue lost the item while binding its AuthorizationLease"
                        .to_owned(),
                )
            })
        })
        .await
    {
        save_account_actor_chains(&actor_chain_path, &previous_actor_chains).await?;
        return Err(error.into());
    }
    drop(actor_chain_guard);
    let submitter = AccountOutboundSubmitter { client };
    loop {
        match outbound
            .submit_next_with_fence(&submitter, &fence, Utc::now())
            .await?
        {
            OutboundEngineOutcome::Accepted(item) | OutboundEngineOutcome::Duplicate(item)
                if item.transaction_id == transaction_id =>
            {
                let remote_event_id = item
                    .remote_event_id
                    .map(|event_id| event_id.to_string())
                    .unwrap_or_else(|| transaction_id.clone());
                debug!(
                    realm_id,
                    transaction_id, "arkret: durable outbound event accepted"
                );
                return Ok(remote_event_id);
            }
            OutboundEngineOutcome::Accepted(_) | OutboundEngineOutcome::Duplicate(_) => {}
            OutboundEngineOutcome::Prepared(_) => {}
            OutboundEngineOutcome::Superseded {
                previous,
                replacement,
            } if previous.transaction_id == transaction_id => {
                debug!(
                    previous_transaction_id = %previous.transaction_id,
                    replacement_transaction_id = %replacement.transaction_id,
                    "arkret: durable outbound event superseded before acceptance"
                );
                transaction_id = replacement.transaction_id;
            }
            OutboundEngineOutcome::Superseded { .. } => {}
            OutboundEngineOutcome::RetryAt { item, at }
                if item.transaction_id == transaction_id =>
            {
                anyhow::bail!(
                    "arkret: outbound event queued for retry at {at} (transaction={transaction_id})"
                );
            }
            OutboundEngineOutcome::RetryAt { .. } => {}
            OutboundEngineOutcome::Rejected { item, .. }
            | OutboundEngineOutcome::Terminal { item, .. }
            | OutboundEngineOutcome::Quarantined { item, .. }
                if item.transaction_id == transaction_id =>
            {
                anyhow::bail!("arkret: outbound event rejected (transaction={transaction_id})");
            }
            OutboundEngineOutcome::Rejected { .. }
            | OutboundEngineOutcome::Terminal { .. }
            | OutboundEngineOutcome::Quarantined { .. } => {}
            OutboundEngineOutcome::Idle => {
                let snapshot = outbound.snapshot().await?;
                if snapshot.items.iter().any(|item| {
                    item.transaction_id == transaction_id && item.remote_event_id.is_some()
                }) {
                    let remote_event_id = snapshot
                        .items
                        .iter()
                        .find(|item| item.transaction_id == transaction_id)
                        .and_then(|item| item.remote_event_id.clone())
                        .map(|event_id| event_id.to_string())
                        .unwrap_or_else(|| transaction_id.clone());
                    return Ok(remote_event_id);
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
    sidecar_exchange: Option<&SidecarExchangeContext>,
    delivery: Option<&crate::arkret_delivery::DeliveryCorrelation>,
) -> anyhow::Result<()> {
    if let Some(content_block) = event.payload.get("content").cloned() {
        match crypto_store.encrypt_content_block_for_realm(realm_id, &content_block)? {
            ArkretEncryptOutcome::PlaintextAllowed => {}
            ArkretEncryptOutcome::Encrypted(encrypted_content) => {
                event.payload.remove("content");
                event.payload.insert(
                    "encrypted_content".to_owned(),
                    serde_json::to_value(encrypted_content.into_envelope())?,
                );
            }
            ArkretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
                anyhow::bail!(
                    "Arkret realm '{realm_id}' requires E2EE but no local MLS group state exists for group '{group_id}'"
                );
            }
        }
    }
    let mut metadata_plaintext = arkret::MessageMetadata::default();
    if let Some(delivery) = delivery {
        metadata_plaintext.extra.insert(
            "delivery".to_owned(),
            json!({
                "checkpoint_id": delivery.checkpoint_id,
                "sequence": delivery.sequence,
                "source_event_id": delivery.source_event_id,
                "initiated_by": delivery.initiated_by.as_str(),
            }),
        );
    }
    if let Some(context) = sidecar_exchange {
        let sidecar = build_user_facing_response_metadata(context)?;
        metadata_plaintext.fields.extend(sidecar.fields);
        metadata_plaintext.extra.extend(sidecar.extra);
    }
    if metadata_plaintext.fields.is_empty() && metadata_plaintext.extra.is_empty() {
        return Ok(());
    }
    // The `role=user_facing_response` exchange binding lives only in
    // `encrypted_metadata` plaintext, encrypted with the same MLS group as
    // `encrypted_content`; carrying it in plaintext `metadata` is a
    // `schema_violation` (zh/models/sidecar.md §7.2.1, forbidden-wire-fields
    // `sidecar_exchange_binding`). A realm without mandatory E2EE therefore
    // cannot carry an exchange reply at all — fail closed instead of leaking.
    match crypto_store.encrypt_message_metadata_for_realm(realm_id, &metadata_plaintext)? {
        ArkretEncryptOutcome::Encrypted(encrypted_metadata) => {
            // Defense in depth: the binding must never surface in plaintext
            // metadata alongside the encrypted carrier.
            event.payload.remove("metadata");
            event.payload.insert(
                "encrypted_metadata".to_owned(),
                serde_json::to_value(encrypted_metadata.into_envelope())?,
            );
            Ok(())
        }
        ArkretEncryptOutcome::PlaintextAllowed if sidecar_exchange.is_some() => {
            anyhow::bail!(
                "Arkret realm '{realm_id}' does not require E2EE; refusing to send a Sidecar exchange binding outside encrypted_metadata"
            );
        }
        ArkretEncryptOutcome::PlaintextAllowed => {
            event.payload.insert(
                "metadata".to_owned(),
                serde_json::to_value(metadata_plaintext)?,
            );
            Ok(())
        }
        ArkretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
            anyhow::bail!(
                "Arkret realm '{realm_id}' requires E2EE but no local MLS group state exists for group '{group_id}' (Sidecar exchange reply)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};

    use super::*;

    fn make_account() -> ArkretAccountConfig {
        ArkretAccountConfig {
            mode: savfox_channels::arkret::ArkretAccountMode::Agent,
            id: "support".into(),
            principal_id: "did:webvh:z6mkfixture:agent.example".into(),
            device_id: "ak:device:01904100-0000-7000-8000-000000000001".into(),
            key_ref: None,
            verification_method: None,
            inkson_bootstrap: None,
            authorized_event_ref: None,
            controller_id: Some("did:webvh:z6mkfixture:controller.example".into()),
            requested_scope: vec![
                "ak.self.events.stream.subscribe".into(),
                "ak.self.events.read.scan".into(),
                "ak.event.read".into(),
            ],
            listen: true,
            send: true,
        }
    }

    #[test]
    fn keypackage_retirement_accepts_only_current_terminal_reason_codes() {
        for reason in [
            arkret::ErrorCode::KEYPACKAGE_ALREADY_CONSUMED,
            arkret::ErrorCode::KEYPACKAGE_UNKNOWN,
        ] {
            assert!(keypackage_retirement_failure_is_terminal(
                &arkret::ReasonCode::from_wire(reason)
            ));
        }
        for reason in ["not_owner", "database_unavailable", "retry_later"] {
            assert!(!keypackage_retirement_failure_is_terminal(
                &arkret::ReasonCode::from_wire(reason)
            ));
        }
    }

    /// Signed consume receipt every `KeyPackagesConsumeOutcome` now carries.
    /// Only `failures` is read by the assertions below, so the receipt just has
    /// to be well-formed.
    fn consume_receipt_fixture(keypackage_ref: &str) -> arkret::KeyPackageConsumeReceipt {
        let device_verification_method = "did:webvh:example.org:service#key-1";
        arkret::KeyPackageConsumeReceipt {
            domain: arkret::NonEmptyString::new("ak.keypackage-consume-receipt.v1").unwrap(),
            claim_request_id: arkret::Base64UrlString::new("Y2xhaW0tcmVxdWVzdC0x").unwrap(),
            claim_ids: vec![arkret::NonEmptyString::new("ak:claim:direct-welcome").unwrap()],
            key_package_refs: vec![keypackage_ref.to_owned()],
            recipient_durable_receipt: arkret::RecipientMlsDurableReceipt {
                domain: arkret::NonEmptyString::new("ak.recipient-mls-durable-receipt.v1").unwrap(),
                claim_request_id: arkret::Base64UrlString::new("Y2xhaW0tcmVxdWVzdC0x").unwrap(),
                key_package_ref: arkret::NonEmptyString::new(keypackage_ref).unwrap(),
                recipient_principal_id: actor_id(),
                recipient_device_id: arkret::DeviceId::new(
                    "ak:device:01904100-0000-7000-8000-000000000006".to_owned(),
                )
                .unwrap(),
                recipient_service_id: arkret::Did::new("did:webvh:example.org:service".to_owned())
                    .unwrap(),
                realm_id: realm_id(),
                mls_group_id: arkret::NonEmptyString::new("mls-group-fixture").unwrap(),
                mls_epoch: 1,
                welcome_ref: arkret::NonEmptyString::new(
                    "ak:event:01904100-0000-8000-8000-000000000007",
                )
                .unwrap(),
                welcome_digest: arkret::Hash::new(format!("sha256:{}", "11".repeat(32))).unwrap(),
                durable_at: chrono::Utc::now(),
                device_verification_method: arkret::NonEmptyString::new(device_verification_method)
                    .unwrap(),
                signature: arkret::KeyOperationSignature {
                    kid: arkret::NonEmptyString::new(device_verification_method).unwrap(),
                    signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                    sig: arkret::Base64UrlString::new("c2lnbmF0dXJl").unwrap(),
                },
            },
            welcome_ref: arkret::NonEmptyString::new(
                "ak:event:01904100-0000-8000-8000-000000000007",
            )
            .unwrap(),
            realm_id: realm_id(),
            mls_group_id: arkret::NonEmptyString::new("mls-group-fixture").unwrap(),
            mls_epoch: 1,
            source_service_id: arkret::Did::new("did:webvh:example.org:service".to_owned())
                .unwrap(),
            consumed_at: chrono::Utc::now(),
            signature: arkret::KeyOperationSignature {
                kid: arkret::NonEmptyString::new(device_verification_method).unwrap(),
                signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                sig: arkret::Base64UrlString::new("c2lnbmF0dXJl").unwrap(),
            },
        }
    }

    #[test]
    fn pending_welcome_consume_accepts_only_its_own_terminal_replay() {
        let keypackage_ref = "sha256:direct-welcome-keypackage";
        let terminal = KeyPackagesConsumeOutcome {
            consumed: Vec::new(),
            consume_receipt: consume_receipt_fixture(keypackage_ref),
            failures: vec![arkret::Failure {
                keypackage_ref: Some(keypackage_ref.to_owned()),
                device_id: None,
                reason_code: arkret::ReasonCode::from_wire(
                    arkret::ErrorCode::KEYPACKAGE_ALREADY_CONSUMED,
                ),
                retry_after_ms: None,
            }],
        };
        assert!(consume_outcome_acknowledges_binding(
            &terminal,
            keypackage_ref
        ));
        assert!(!consume_outcome_acknowledges_binding(
            &terminal,
            "sha256:another-keypackage"
        ));

        let mut non_terminal = terminal;
        non_terminal.failures[0].reason_code = arkret::ReasonCode::ClaimInvalid;
        assert!(!consume_outcome_acknowledges_binding(
            &non_terminal,
            keypackage_ref
        ));
    }

    #[test]
    fn account_sync_extracts_strongly_typed_mls_commit_event() {
        let realm_id = realm_id();
        let identity = arkret::mls::ArkretMlsIdentity::new_basic(
            actor_id(),
            arkret::DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned())
                .unwrap(),
        )
        .unwrap();
        let mut group = identity.create_group(realm_id.as_str().as_bytes()).unwrap();
        let commit = group.self_update_commit().unwrap();
        let hash = |marker: char| {
            arkret::Hash::new(format!("sha256:{}", marker.to_string().repeat(64))).unwrap()
        };
        let governance_binding = arkret::MlsGovernanceBindingPayload::realm(
            realm_id,
            commit.group_id.clone(),
            0,
            commit.epoch,
            hash('c'),
            arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
            "arkret.reducer.v1",
        )
        .unwrap();
        let payload = arkret::MlsCommitPayload::new(
            0,
            "ak:event:01904100-0000-8000-8000-000000000001",
            Vec::new(),
            &commit,
            governance_binding,
        )
        .unwrap();
        let event_ref =
            arkret::EventId::new("ak:event:01904100-0000-8000-8000-000000000011".to_owned())
                .unwrap();
        let value = json!({
            "kind": "ak.mls.commit",
            "event_id": event_ref.to_string(),
            "payload": payload,
        });
        let mut commits = Vec::new();
        collect_typed_mls_commit_events(&value, 8, &mut commits);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].0, event_ref);
        assert_eq!(commits[0].1.next_epoch(), 1);
        assert_eq!(commits[0].1.commit_bytes_b64(), commit.commit);
    }

    #[test]
    fn forgetting_unbound_account_keeps_replacement_diagnostic() {
        let channel = ArkretChannelConfig {
            id: "diagnostic-replacement-channel".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            service_id: None,
            delivery_mode: "interactive_chat".to_owned(),
            accounts: Vec::new(),
        };
        let old = make_account();
        let mut replacement = make_account();
        replacement.id = "replacement".to_owned();
        {
            let mut state = runtime_state().lock().expect("runtime state lock");
            state.diagnostics.insert(
                task_key(&channel.id, &old.id),
                ArkretListenerDiagnostic::new(&channel, &old),
            );
            state.diagnostics.insert(
                task_key(&channel.id, &replacement.id),
                ArkretListenerDiagnostic::new(&channel, &replacement),
            );
        }

        forget_arkret_account_runtime(&channel.id, &old.id);

        let diagnostics = arkret_account_runtime_diagnostics(&channel.id);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].get("account_id").and_then(Value::as_str),
            Some("replacement")
        );
        forget_arkret_account_runtime(&channel.id, &replacement.id);
    }

    #[tokio::test]
    async fn controlled_listener_auth_failure_retries_and_recovers() {
        let channel = ArkretChannelConfig {
            id: "fake-recovery-channel".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            service_id: None,
            delivery_mode: "interactive_chat".to_owned(),
            accounts: Vec::new(),
        };
        let account = make_account();
        let key = task_key(&channel.id, &account.id);
        runtime_state()
            .lock()
            .expect("runtime state lock")
            .diagnostics
            .insert(
                key.clone(),
                ArkretListenerDiagnostic::new(&channel, &account),
            );
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_for_run = Arc::clone(&attempts);
        let channel_for_run = channel.clone();
        let account_for_run = account.clone();
        let task = tokio::spawn(run_account_listener_retry_loop(
            channel.id.clone(),
            account.id.clone(),
            move || {
                let attempt =
                    attempts_for_run.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let channel = channel_for_run.clone();
                let account = account_for_run.clone();
                async move {
                    if attempt == 1 {
                        record_listener_failure(
                            &channel,
                            &account,
                            "authentication_error",
                            "fake service rejected credentials",
                        );
                    } else {
                        record_listener_phase(&channel, &account, "subscribing");
                        std::future::pending::<()>().await;
                    }
                }
            },
        ));

        let retry_observed = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                if arkret_account_runtime_diagnostics(&channel.id)
                    .iter()
                    .any(|diagnostic| {
                        diagnostic.get("phase").and_then(Value::as_str) == Some("retry_wait")
                            && diagnostic
                                .get("last_error")
                                .and_then(Value::as_str)
                                .is_some_and(|error| error.contains("fake service"))
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            retry_observed.is_ok(),
            "authentication retry was not reported"
        );

        let recovered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if arkret_account_runtime_diagnostics(&channel.id)
                    .iter()
                    .any(|diagnostic| {
                        diagnostic.get("phase").and_then(Value::as_str) == Some("subscribing")
                            && diagnostic.get("attempt").and_then(Value::as_u64) == Some(2)
                            && diagnostic.get("last_error").is_some_and(Value::is_null)
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        task.abort();
        runtime_state()
            .lock()
            .expect("runtime state lock")
            .diagnostics
            .remove(&key);

        assert!(
            recovered.is_ok(),
            "listener did not recover on the next attempt"
        );
    }

    #[test]
    fn encrypted_account_self_echo_is_identified_before_mls_decrypt() {
        let account = make_account();

        assert!(arkret_sender_is_account_principal(
            Some("did:webvh:z6mkfixture:agent.example"),
            &account,
        ));
        assert!(arkret_sender_is_account_principal(
            Some("DID:WEBVH:Z6MKFIXTURE:AGENT.EXAMPLE"),
            &account,
        ));
        assert!(!arkret_sender_is_account_principal(
            Some("did:webvh:z6mkfixture:controller.example"),
            &account,
        ));
        assert!(!arkret_sender_is_account_principal(None, &account));
    }

    #[test]
    fn initial_account_catchup_selects_history_baseline_mode() {
        let initial = ClientEvent::AccountUpdates(garth::AccountUpdateContext {
            initial_catchup: true,
            malformed_realms: Vec::new(),
            to_device_ack_token: None,
            to_device_limited: false,
            to_device_next_cursor: None,
            to_device_lost: false,
            device_lists: arkret::AccountSubscribeDeviceListChanges {
                changed: Vec::new(),
                left: Vec::new(),
            },
            account_data: Vec::new(),
            agent_signer_evidence: Vec::new(),
            partial: false,
        });
        let ClientEvent::AccountUpdates(mut live_context) = initial.clone() else {
            unreachable!();
        };
        live_context.initial_catchup = false;
        let live = ClientEvent::AccountUpdates(live_context);

        let initial_mode = account_inbound_mode(&[initial]);
        assert_eq!(initial_mode, AccountInboundMode::Baseline);
        assert!(initial_mode.suppresses_agent_dispatch());
        assert_eq!(account_inbound_mode(&[live]), AccountInboundMode::Trigger);
    }

    #[test]
    fn outbound_fence_accepts_online_submission_without_authorization_lease() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-online-submit-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let crypto_store = FileArkretCryptoStore::for_account(&home, "c1", "support");
        let mut queue = garth::sync_client::SendQueue::new();
        let realm_id = realm_id();
        let item = queue
            .enqueue(
                Some("online-without-lease".to_owned()),
                realm_id.clone(),
                garth::sync_client::SendQueueItemKind::Message,
                json!({
                    "event": {
                        "kind": "ak.message.create",
                        "actor_seq": 0,
                        "payload": {
                            "agent_context": { "mode": "task_delivery" }
                        }
                    },
                    "authorization_lease": null,
                    "cba_proof_bundles": []
                }),
                Vec::new(),
            )
            .expect("queue online submission");
        assert!(item.authorization_lease.is_none());

        let fence = AccountOutboundEncryptionFence {
            crypto_store,
            actor_chain_path: home.join("actor-chains.json"),
        };
        assert_eq!(
            fence.evaluate(&item).expect("evaluate online submission"),
            OutboundGenerationFenceDecision::Current
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn refreshed_grant_must_preserve_exact_runtime_identity_and_required_scope() {
        let account = make_account();
        let mut state = garth::SessionGrantState {
            principal_id: Did::new(account.principal_id.clone()).unwrap(),
            device_id: Some(DeviceId::new(account.device_id.clone()).unwrap()),
            grant_id: arkret::GrantId::new(
                "ak:grant:01904100-0000-7000-8000-000000000001".to_owned(),
            )
            .unwrap(),
            grant_jwt: "redacted-test-grant".to_owned(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            audience: Did::new("did:webvh:z6mkfixture:service.example").unwrap(),
            granted_scope: vec!["ak.event.read".to_owned()],
            session_public_key: None,
            dpop_jkt: Some("test-jkt".to_owned()),
        };
        assert!(refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        // A grant that came back without the scope the action needs proves
        // nothing about that action.
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.self.events.command.submit"
        ));

        state.granted_scope.clear();
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.granted_scope.push("ak.event.read".to_owned());
        state.device_id =
            Some(DeviceId::new("ak:device:01904100-0000-7000-8000-000000000099").unwrap());
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.device_id = Some(DeviceId::new(account.device_id.clone()).unwrap());
        state.principal_id = Did::new("did:webvh:z6mkfixture:other.example").unwrap();
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
    }

    #[tokio::test]
    async fn sidecar_reply_fails_closed_before_any_outbound_side_effect() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-sidecar-runtime-gate-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let context = SidecarExchangeContext {
            exchange_id: "01904100-0000-7000-8000-0000000000aa".to_owned(),
            request_event_id: "ak:event:01904100-0000-8000-8000-000000000031".to_owned(),
            coordinator_assignment_event_id: None,
        };
        let error = send_to_arkret_account(
            &home,
            realm_id().as_str(),
            Some("ak:strand:01904100-0000-8000-8000-000000000011"),
            "must not be sent",
            Some(&context),
            None,
            None,
            None,
        )
        .await
        .expect_err("Sidecar reply must fail closed for an unresolvable runtime");

        assert!(
            error.to_string().contains("saved_channel_config_id"),
            "unexpected error: {error:#}"
        );
        assert!(
            !home.exists(),
            "the outbound path must resolve and gate before any store side effect"
        );
    }

    fn realm_id() -> arkret::RealmId {
        arkret::RealmId::new("ak:realm:01904100-0000-8000-8000-000000000001").unwrap()
    }

    /// A Sidecar exchange reply must never mount the binding outside
    /// `encrypted_metadata`: when the realm does not enforce E2EE the send
    /// fails closed instead of emitting the binding in plaintext
    /// (zh/models/sidecar.md §7.2.1, forbidden-wire-fields).
    #[test]
    fn sidecar_reply_fails_closed_without_e2ee_realm() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-sidecar-plaintext-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let crypto_store = FileArkretCryptoStore::for_account(&home, "c1", "support");
        let context = SidecarExchangeContext {
            exchange_id: "01904100-0000-7000-8000-0000000000aa".to_owned(),
            request_event_id: "ak:event:01904100-0000-8000-8000-000000000031".to_owned(),
            coordinator_assignment_event_id: None,
        };
        let request = MessageCreateRequest {
            realm_id: realm_id().to_string(),
            strand_id: "ak:strand:01904100-0000-8000-8000-000000000011".to_owned(),
            body: "final user-visible reply".to_owned(),
            principal_id: "did:webvh:z6mkfixture:agent.example".to_owned(),
            actor_seq: 1,
            thread_root_id: None,
            sidecar_exchange: Some(context.clone()),
        };
        let mut event = build_message_create_event(&request).expect("build");

        // No realm policy is registered, so content would be plaintext-allowed;
        // the Sidecar binding must fail closed rather than ship unencrypted.
        let err = apply_account_outbound_encryption(
            &crypto_store,
            realm_id().as_str(),
            &mut event,
            Some(&context),
            None,
        )
        .expect_err("plaintext realm must reject Sidecar exchange replies");
        assert!(
            err.to_string().contains("Sidecar exchange binding"),
            "unexpected error: {err:#}"
        );
        assert!(event.payload.get("encrypted_metadata").is_none());
        assert!(
            !serde_json::to_string(&event.payload)
                .unwrap()
                .contains("sidecar_exchange_binding"),
            "binding must never appear in plaintext payload"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn actor_id() -> Did {
        Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap()
    }

    fn keypackage_record(
        principal_id: &Did,
        device_id: &DeviceId,
        marker: u8,
        last_resort: bool,
    ) -> MlsKeyPackageRecord {
        let key_package_bytes = vec![marker; 16];
        MlsKeyPackageRecord {
            keypackage_id: format!("ak:mls:kp:01904100-0000-7000-8000-0000000000{marker:02x}"),
            principal_id: principal_id.clone(),
            device_id: device_id.clone(),
            key_package: URL_SAFE_NO_PAD.encode(&key_package_bytes),
            keypackage_ref: arkret::Hash::new(arkret::canonical::sha256_digest(&key_package_bytes))
                .unwrap(),
            cipher_suites: vec!["MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519".to_owned()],
            capabilities: vec!["ak.mls.rfc9420".to_owned()],
            state: arkret::MlsKeyPackageState::Published,
            claim_id: None,
            created_at: Utc::now(),
            expires_at: None,
            device_signature: None,
            last_resort,
        }
    }

    #[test]
    fn keypackage_pool_replenishes_to_spec_low_watermark() {
        assert_eq!(keypackage_replenishment_deficit(Some(0)), Some(8));
        assert_eq!(keypackage_replenishment_deficit(Some(1)), Some(7));
        assert_eq!(keypackage_replenishment_deficit(Some(7)), Some(1));
        assert_eq!(keypackage_replenishment_deficit(Some(8)), None);
        assert_eq!(keypackage_replenishment_deficit(Some(9)), None);
        assert_eq!(keypackage_replenishment_deficit(None), None);
    }

    #[test]
    fn keypackage_upload_uses_sdk_batch_transcript_without_entry_signatures() {
        let principal_id = Did::new("did:webvh:z6mkfixture:agent.example".to_owned()).unwrap();
        let device_id =
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000001".to_owned()).unwrap();
        let verification_method = format!("{}#runtime-1", principal_id.as_str());
        let seed = [7_u8; 32];
        let key_ref = ArkretKeyRef::InlineSeedBase64 {
            value: STANDARD_NO_PAD.encode(seed),
        };
        let records = [
            keypackage_record(&principal_id, &device_id, 1, false),
            keypackage_record(&principal_id, &device_id, 2, true),
        ];

        let request = build_signed_keypackage_upload_request(
            principal_id.clone(),
            device_id.clone(),
            &records,
            &key_ref,
            &verification_method,
        )
        .unwrap();

        assert!(
            request
                .key_packages
                .iter()
                .all(|entry| entry.device_signature.is_none())
        );
        assert_eq!(request.key_packages[0].last_resort, None);
        assert_eq!(request.key_packages[1].last_resort, Some(true));
        let unsigned = request.unsigned();
        let batch_input = arkret::keypackages_upload_signing_input(&unsigned).unwrap();
        let public_key = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        arkret::verify_keypackage_signing_input(
            &public_key,
            &verification_method,
            &batch_input,
            &request.device_signature,
        )
        .unwrap();

        let entry_input = arkret::keypackage_upload_entry_signing_input(
            &principal_id,
            &device_id,
            &request.key_packages[0],
        )
        .unwrap();
        assert_ne!(batch_input, entry_input);
        assert!(
            arkret::verify_keypackage_signing_input(
                &public_key,
                &verification_method,
                &entry_input,
                &request.device_signature,
            )
            .is_err()
        );
    }

    fn message_event(body: &str) -> arkret::Event {
        message_event_with_seq(body, 1)
    }

    fn message_event_with_seq(body: &str, actor_seq: u64) -> arkret::Event {
        arkret::Event::new(
            "ak.message.create",
            arkret::ScopeRef::Realm {
                realm_id: realm_id(),
            },
            actor_id(),
            actor_seq,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            json!({
                "strand_id": "ak:strand:01904100-0000-8000-8000-000000000002",
                "track_name": "discussion",
                "reply_to": "ak:message:01904100-0000-8000-8000-000000000003",
                "content": {
                    "kind": "ak.content.text",
                    "body": body
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
            entry: arkret::RealmSyncEntry {
                timeline: Some(arkret::Timeline {
                    events: vec![message_event("hello from engine")],
                    limited: false,
                    prev_cursor: None,
                    preview_only: None,
                    ordered_log_conflicts: Vec::new(),
                    extra: Default::default(),
                }),
                ..Default::default()
            },
        };

        let parsed = parse_realm_update_for_account(update, &account);

        assert_eq!(parsed.skipped, Vec::new());
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].account_id, account.id);
        assert_eq!(parsed.events[0].body, "hello from engine");
        assert_eq!(parsed.events[0].sender_did, actor_id().as_str());
        assert_eq!(
            parsed.events[0].strand_id.as_deref(),
            Some("ak:strand:01904100-0000-8000-8000-000000000002")
        );
    }

    #[test]
    fn sync_realm_delta_marks_direct_conversation_messages_as_dm() {
        let account = make_account();
        let update = arkret::RealmUpdate {
            realm_id: realm_id(),
            entry: arkret::RealmSyncEntry {
                timeline: Some(arkret::Timeline {
                    events: vec![message_event("hello direct agent")],
                    limited: false,
                    prev_cursor: None,
                    preview_only: None,
                    ordered_log_conflicts: Vec::new(),
                    extra: Default::default(),
                }),
                summary: Some(
                    serde_json::from_value(json!({
                        "joined_member_count": 2
                    }))
                    .unwrap(),
                ),
                state_at_window_start: Some(
                    serde_json::from_value(json!({
                        "actor_profiles": {},
                        "realm_metadata": {
                            "title": "Direct conversation",
                            "collaboration_role": "direct_conversation"
                        },
                        "e2ee_epoch": null
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            },
        };

        let parsed = parse_realm_update_for_account(update, &account);

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].chat_type.as_deref(), Some("dm"));
        assert_eq!(parsed.events[0].participant_count, Some(2));
    }

    #[test]
    fn sync_realm_delta_records_direct_conversation_e2ee_policy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let channel = ArkretChannelConfig {
            id: "c1".to_owned(),
            base_url: "https://arkret.example".to_owned(),
            service_id: None,
            delivery_mode: "interactive_chat".to_owned(),
            accounts: Vec::new(),
        };
        let account = make_account();
        let crypto_store = FileArkretCryptoStore::for_account(tmp.path(), &channel.id, &account.id);
        let update = arkret::RealmUpdate {
            realm_id: realm_id(),
            entry: arkret::RealmSyncEntry {
                state_at_window_start: Some(
                    serde_json::from_value(json!({
                        "actor_profiles": {},
                        "realm_metadata": {
                            "title": "Direct conversation",
                            "collaboration_role": "direct_conversation"
                        },
                        "e2ee_epoch": null
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            },
        };

        assert_eq!(
            record_account_realm_crypto_policy_from_update(
                &update,
                &crypto_store,
                &channel,
                &account,
            ),
            1
        );
        let state = crypto_store.load().expect("crypto state should load");
        let policy = state
            .realm_policies
            .get(realm_id().as_str())
            .expect("direct-conversation E2EE policy should persist");
        assert!(policy.requires_e2ee());
        assert_eq!(
            policy.group_id_for_realm(),
            URL_SAFE_NO_PAD.encode(realm_id().as_str())
        );
    }

    #[test]
    fn realm_create_profile_marks_scan_catchup_as_direct_conversation() {
        let direct_realm_create = arkret::Event::new(
            "ak.realm.create",
            arkret::ScopeRef::Realm {
                realm_id: realm_id(),
            },
            actor_id(),
            0,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            json!({
                "object": {
                    "id": realm_id().as_str(),
                    "schema": "ak.schema.realm.v1",
                    "schema_refs": ["ak.profile.direct_conversation_realm.v1"],
                    "fields": {"collaboration_role": "direct_conversation"}
                }
            }),
        )
        .unwrap();
        let entry = arkret::RealmSyncEntry {
            timeline: Some(arkret::Timeline {
                events: vec![direct_realm_create],
                limited: false,
                prev_cursor: None,
                preview_only: None,
                ordered_log_conflicts: Vec::new(),
                extra: Default::default(),
            }),
            ..Default::default()
        };

        assert_eq!(realm_sync_chat_type(&entry).as_deref(), Some("dm"));
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
            entry: arkret::RealmSyncEntry {
                timeline: Some(arkret::Timeline {
                    events: vec![message_event("window head")],
                    limited: true,
                    prev_cursor: Some("ak:cursor:older-1".to_owned()),
                    preview_only: None,
                    ordered_log_conflicts: Vec::new(),
                    extra: Default::default(),
                }),
                ..Default::default()
            },
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
                arkret::EventsQueryOutcome {
                    events: vec![message_event_with_seq("older one", 2)],
                    snapshot_bootstrap: None,
                    prev_cursor: Some("ak:cursor:older-2".to_owned()),
                    next_cursor: None,
                    has_more: true,
                    range_completeness: None,
                },
                arkret::EventsQueryOutcome {
                    events: vec![message_event_with_seq("older two", 3)],
                    snapshot_bootstrap: None,
                    prev_cursor: None,
                    next_cursor: None,
                    has_more: false,
                    range_completeness: None,
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
    fn account_realm_update_rejects_unbound_nested_mls_welcome() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let channel = ArkretChannelConfig {
            id: "c1".to_owned(),
            base_url: "https://arkret.example".to_owned(),
            service_id: None,
            delivery_mode: "interactive_chat".to_owned(),
            accounts: Vec::new(),
        };
        let account = make_account();
        let crypto_store = FileArkretCryptoStore::for_account(tmp.path(), &channel.id, &account.id);
        let group_id = "group-account-welcome";
        let welcome_event = arkret::Event::new(
            "ak.mls.welcome",
            arkret::ScopeRef::Realm {
                realm_id: realm_id(),
            },
            actor_id(),
            7,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            json!({
                "kind": "ak.mls.welcome",
                "content": mls_welcome_value(group_id)
            }),
        )
        .unwrap();
        let update = arkret::RealmUpdate {
            realm_id: realm_id(),
            entry: arkret::RealmSyncEntry {
                timeline: Some(arkret::Timeline {
                    events: vec![welcome_event],
                    limited: false,
                    prev_cursor: None,
                    preview_only: None,
                    ordered_log_conflicts: Vec::new(),
                    extra: Default::default(),
                }),
                ..Default::default()
            },
        };

        let recorded = record_account_mls_welcomes_from_realm_update(
            &update,
            &crypto_store,
            &channel,
            &account,
        );

        assert_eq!(recorded, 0);
        let state = crypto_store.load().expect("crypto state should load");
        assert!(!state.bootstrap.contains_key(group_id));
    }
}
