//! Arkret Agent channel runtime.
//!
//! Owns one async task per (channel, account) pair. Each task:
//!
//! 1. Mints a short-lived `agent_key_proof` session grant bound to DPoP.
//! 2. Pulls its own Agent delivery queue and scans authorized Commit streams.
//! 3. Extracts dispatchable `ak.message.create` events.
//! 4. Dispatches each event to the agent pipeline.
//!
//! Outbound sends go through [`send_to_arkret_account`].
//! Agent presence uses the v1 Agent sender branch: no device id is emitted and
//! the current runtime key supplies both the proof and sequence endpoint.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use arkret::{
    DeviceId, DeviceMessagesAckRequestBody, DidCoreId, EventId, KeyPackagesConsumeOutcome,
    KeyPackagesConsumeUnsignedRequest, KeyPackagesUploadRequestBody,
    KeyPackagesUploadUnsignedRequest, MlsKeyPackageRecord, RealmId, ServiceOperationId,
};
use chrono::Utc;
use garth::{
    ClientEvent, CursorStore, DurableInboxStore, EventCacheStore, OutboundEngine,
    OutboundEngineOutcome, TransportProvider,
};
use savfox_channels::arkret::{
    ArkretAccountConfig, ArkretAgentSessionProvider, ArkretChannelConfig,
    ArkretContentEncryptionFloor, ArkretDecryptDetailedOutcome, ArkretEncryptOutcome,
    ArkretHttpClient, ArkretInboundEvent, ArkretInboundParseResult, ArkretInboundSkipReason,
    ArkretInboundSkippedEvent, ArkretKeyRef, ArkretMlsWelcomeConsumeBinding,
    ArkretRealmCryptoPolicy, FileArkretCryptoStore, MessageCreateRequest, SidecarExchangeAdmission,
    SidecarExchangeContext, SidecarExchangeStore, SidecarRequestGate, SidecarTerminalAdmission,
    UnableToDecryptReason, account_allows_event_read, build_message_create_event,
    build_user_facing_response_metadata, encode_sidecar_reply_target,
    gate_inbound_exchange_control, gate_inbound_request_binding, open_account_store,
    parse_event_values_for_account, resolve_arkret_outbound_account_for_binding,
    sidecar_binding_from_metadata_plaintext, sign_keypackages_consume_request,
    sign_keypackages_upload_request,
};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use super::{ChannelRegistry, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

pub(crate) mod governance;
mod receive;

/// Per-(channel, account) runtime handles. Indexed by `{channel_id}::{account_id}`.
#[derive(Default)]
struct ArkretRuntimeState {
    handles: HashMap<String, tokio::task::JoinHandle<()>>,
    diagnostics: HashMap<String, ArkretListenerDiagnostic>,
}

fn known_revoked_authorizations() -> &'static StdMutex<HashSet<(String, String, String)>> {
    static REVOKED: OnceLock<StdMutex<HashSet<(String, String, String)>>> = OnceLock::new();
    REVOKED.get_or_init(|| StdMutex::new(HashSet::new()))
}

fn authorization_fence_key(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> Option<(String, String, String)> {
    Some((
        channel.id.clone(),
        account.id.clone(),
        account.authorized_event_ref.clone()?,
    ))
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
            "scope_recovery": self.last_reason_code.as_deref().and_then(savfox_gateway_shared::arkret::agent_scope_recovery),
            "received_events": self.received_events,
            "dispatched_events": self.dispatched_events,
            "baselined_events": self.baselined_events,
            "skipped_events": self.skipped_events,
            "updated_at": self.updated_at,
        })
    }
}

const ACCOUNT_EVENT_DEDUPE_MAX: usize = 4096;
const ACCOUNT_SCAN_CATCHUP_LIMIT: u16 = 100;
const ACCOUNT_SCAN_CATCHUP_MAX_PAGES: usize = 64;
const ACCOUNT_AUTH_WARNING_INTERVAL: Duration = Duration::from_secs(30);
/// v1 session Signals expire after 30 seconds. Twenty seconds leaves room for
/// scheduling, network jitter and an in-band session refresh.
const ACCOUNT_PRESENCE_REFRESH: Duration = Duration::from_secs(20);
const DEVICE_MESSAGES_PULL_LIMIT: u32 = 100;
const DEVICE_MESSAGES_PULL_MAX_PAGES: usize = 16;

const KEYPACKAGES_UPLOAD_SCOPE: &str = ServiceOperationId::SELF_KEYS_KEYPACKAGES_UPLOAD_CREATE_V1;
const KEYPACKAGES_CONSUME_SCOPE: &str =
    ServiceOperationId::SELF_KEYS_KEYPACKAGES_COMMAND_CONSUME_V1;
const KEYPACKAGE_MIN_AVAILABLE: usize = 8;
const DEVICE_MESSAGES_LIST_SCOPE: &str = ServiceOperationId::SELF_DEVICE_MESSAGES_READ_LIST_V1;
const DEVICE_MESSAGES_ACK_SCOPE: &str = ServiceOperationId::SELF_DEVICE_MESSAGES_COMMAND_ACK_V1;

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

fn record_listener_service_failure(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    phase: &'static str,
    error: impl std::fmt::Display,
    reason: Option<&str>,
) {
    update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
        diagnostic.phase = phase;
        diagnostic.last_reason_code = reason.map(str::to_owned);
        diagnostic.last_error = Some(error.to_string());
    });
}

fn transport_service_reason(error: &garth::Error) -> Option<&str> {
    match error {
        garth::Error::Api { error, .. } => error
            .extensions
            .get("reason_code")
            .and_then(Value::as_str)
            .or_else(|| reason_code_from_message(&error.detail)),
        _ => None,
    }
}

fn listener_service_reason(error: &anyhow::Error) -> Option<&str> {
    savfox_channels::arkret::agent_session_exchange_reason(error).or_else(|| {
        error.chain().find_map(|source| {
            source
                .downcast_ref::<garth::Error>()
                .and_then(transport_service_reason)
        })
    })
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
    reason_code_from_message(error).map(str::to_owned)
}

fn reason_code_from_message(error: &str) -> Option<&str> {
    let marker = "reason_code=";
    let start = error.find(marker)? + marker.len();
    let code = error[start..]
        .split(|ch: char| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'))
        .next()
        .unwrap_or_default();
    (!code.is_empty()).then_some(code)
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
        let migration_reason = runtime_state().lock().ok().and_then(|state| {
            state
                .diagnostics
                .get(&task_key(&diagnostic_channel_id, &diagnostic_account_id))
                .and_then(|diagnostic| diagnostic.last_reason_code.clone())
        });
        let recovery = migration_reason.as_deref().and_then(|reason| {
            if reason == "agent_requested_scope_commitment_invalid" {
                Some("provision_new_agent")
            } else {
                savfox_gateway_shared::arkret::agent_scope_recovery(reason)
            }
        });
        if let Some(recovery) = recovery
            .filter(|recovery| *recovery != "issue_session_within_provision_and_key_ceilings")
        {
            update_listener_diagnostic(
                &diagnostic_channel_id,
                &diagnostic_account_id,
                |diagnostic| diagnostic.phase = "migration_required",
            );
            warn!(
                channel_id = %diagnostic_channel_id,
                account_id = %diagnostic_account_id,
                recovery,
                "arkret: Agent scope requires an explicit authorization recovery; listener stopped without expanding scope"
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
    if !account.has_requested_scope(ServiceOperationId::SELF_COMMITTED_EVENT_STREAM_SUBSCRIBE_V1) {
        record_listener_failure(
            &channel,
            &account,
            "scope_rejected",
            "missing ak.self.committed_event.stream.subscribe.v1",
        );
        warn!(
            "arkret: account '{}' listen=true but missing ak.self.committed_event.stream.subscribe.v1; refusing to open subscribe endpoint",
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
        Ok(store) => store,
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
            if let Some(reason) = savfox_channels::arkret::agent_session_exchange_reason(&err) {
                update_listener_diagnostic(&channel.id, &account.id, |diagnostic| {
                    diagnostic.last_reason_code = Some(reason.to_owned());
                });
            }
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
            record_listener_service_failure(
                &channel,
                &account,
                "authentication_error",
                &error,
                transport_service_reason(&error),
            );
            runtime::record_channel_probe("arkret", "error").await;
            return;
        }
    };
    record_listener_phase(&channel, &account, "subscribing");
    runtime::record_channel_probe("arkret", "ok").await;
    if let Err(err) = run_account_key_lifecycle_maintenance(
        &client,
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
            "key_lifecycle_error",
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
    match receive::drive(
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
        AccountEngineOutcome::Retry { error } => {
            record_listener_service_failure(
                &channel,
                &account,
                "subscribe_error",
                format!("{error:#}"),
                listener_service_reason(&error),
            );
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
    let Some(key_ref) = account.key_ref.as_ref() else {
        record_presence_failure(channel, account, "missing runtime keyRef");
        return;
    };
    let Some(verification_method) = account.verification_method.as_deref() else {
        record_presence_failure(channel, account, "missing runtime verificationMethod");
        return;
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
        let authority_head = match verified_presence_authority_head(client.inner(), &realm_id).await
        {
            Ok(head) => head,
            Err(error) => {
                record_presence_failure(
                    channel,
                    account,
                    format!("verify current Commit head for presence Realm '{realm}': {error}"),
                );
                continue;
            }
        };
        let envelope = match crypto_store.seal_online_presence_signal(
            realm_id.as_str(),
            &account.actor_account_id,
            verification_method,
            key_ref,
            &authority_head,
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
                    "arkret: encrypted Agent presence heartbeat accepted"
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

async fn verified_presence_authority_head(
    http: &arkret::http_client::Client,
    realm_id: &RealmId,
) -> anyhow::Result<arkret::CommitStreamHead> {
    let request = arkret::AuthorityBundleRequest {
        realm_id: realm_id.clone(),
        nonce: arkret::Base64UrlString::new(arkret::base64url_encode(rand::random::<[u8; 32]>()))
            .map_err(anyhow::Error::msg)?,
    };
    let authority = garth::AuthorityClient::new(http.clone());
    let bundle = authority.resolve_authority(&request).await?;
    let keys = garth::fetch_historical_station_key_directory(http, &bundle, None, None).await?;
    let freshness =
        arkret::identity::RealmAuthorityFreshness::new(Utc::now(), request.nonce.clone());
    let mut replica = garth::RealmReplica::new(realm_id.clone());
    replica.install_verified_authority(&request, bundle.clone(), &freshness, &keys)?;
    anyhow::ensure!(
        bundle.realm_stream_head.stream_ref
            == arkret::CommitStreamRef::Realm {
                realm_id: realm_id.clone()
            },
        "presence authority bundle does not name the Realm stream"
    );
    Ok(bundle.realm_stream_head)
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

/// Revalidate locally queued producer bytes against the configured runtime
/// key and current encryption policy. Acceptance remains the Station's decision.
fn queued_message_matches_runtime(
    event: &arkret::Event,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) -> anyhow::Result<bool> {
    if event.actor_id != arkret::ActorId::account(account.actor_account_id.clone()) {
        return Ok(false);
    }
    let Some(proof) = event.producer_proof.as_ref() else {
        return Ok(false);
    };
    if account.verification_method.as_deref() != Some(proof.verification_method.as_str()) {
        return Ok(false);
    }
    let evidence = account
        .current_signer_evidence
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("runtime signer evidence is unavailable"))?;
    let reference = account
        .signer_resolution_evidence_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("runtime signer evidence reference is unavailable"))?;
    evidence.validate_against(reference)?;
    let root = &evidence.authenticated_signer_evidence;
    root.validate()?;
    if root.signer_kind != arkret::AuthenticatedSignerKind::Agent
        || root.subject_id.as_str() != account.principal_id
        || root.verification_method != proof.verification_method
    {
        return Ok(false);
    }
    let public_key = arkret::signatures::PublicKeyMaterial::Jwk {
        value: serde_json::to_value(&evidence.authenticated_signer_evidence.public_key_jwk)?,
    };
    let bytes = arkret::canonical::canonical_json_bytes(&event.digest_payload()?)?;
    if arkret::signatures::verify_ed25519_detached_jws_proof_with_digest_suite(
        proof,
        &bytes,
        event.actual_signer(),
        &public_key,
        event.realm_id.digest_suite_code().digest_suite(),
    )
    .is_err()
    {
        return Ok(false);
    }
    if event.kind == "ak.message.create"
        && crypto_store.realm_requires_e2ee(event.realm_id.as_str())?
        && event.payload.get("encrypted_content").is_none()
    {
        return Ok(false);
    }
    Ok(true)
}

async fn cancel_unusable_account_submissions(
    outbound: &OutboundEngine<garth::FileStore, garth::SystemClock>,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) -> anyhow::Result<()> {
    for item in outbound.snapshot().await?.items {
        if item.status == garth::SendQueueStatus::Queued
            && !queued_message_matches_runtime(
                item.submission.primary_event(),
                account,
                crypto_store,
            )?
        {
            // Cancellation cannot rewrite a producer Event or an in-flight
            // Station outcome. The queue serializes this state transition.
            outbound.cancel(item.event_id().clone()).await?;
        }
    }
    Ok(())
}

async fn drain_pending_account_outbound(
    client: &ArkretHttpClient,
    account_store: &garth::FileStore,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    crypto_store: &FileArkretCryptoStore,
) {
    let outbound = OutboundEngine::new(account_store.clone(), garth::SystemClock);
    if let Err(error) = cancel_unusable_account_submissions(&outbound, account, crypto_store).await
    {
        warn!(channel_id = %channel.id, account_id = %account.id,
            "arkret: durable outbound validation failed: {error}");
        return;
    }
    let authority = garth::AuthorityClient::new(client.inner().clone());
    let options = arkret::http_client::ClientRequestOptions::default();
    loop {
        match outbound.submit_next(&authority, &options).await {
            Ok(OutboundEngineOutcome::Committed { .. }) => {
                debug!(channel_id = %channel.id, account_id = %account.id,
                    "arkret: durable producer submission committed");
            }
            Ok(OutboundEngineOutcome::Rejected { .. } | OutboundEngineOutcome::Failed { .. }) => {
                warn!(channel_id = %channel.id, account_id = %account.id,
                    "arkret: durable producer submission reached a refusal");
            }
            Ok(OutboundEngineOutcome::Idle | OutboundEngineOutcome::Retry { .. }) => return,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id,
                    "arkret: durable outbound worker deferred: {error}");
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
            let inbound_mode = account_inbound_mode(&delivery.payload);
            if inbound_mode == AccountInboundMode::Baseline {
                info!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    delivery_id = delivery.id.get(),
                    events = delivery.payload.len(),
                    "arkret: processing initial account catch-up as a history baseline"
                );
            }
            let mut processing_error = None;
            for event in delivery.payload {
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
        ClientEvent::RealmProjection {
            realm_id, value, ..
        } => {
            if let Err(error) = crypto_store.record_station_mls_currents(&value) {
                warn!(channel_id = %channel.id, realm_id = %realm_id,
                    "arkret: Station MLS current result was not retained: {error:#}");
            }
        }
        ClientEvent::Committed(delta) => {
            let Some(event) = delta.event().cloned() else {
                return Ok(());
            };
            if event_declares_direct_conversation_realm(&event) {
                crypto_store.upsert_realm_policy(ArkretRealmCryptoPolicy {
                    realm_id: delta.realm_id.to_string(),
                    content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                    encryption_profile: Some("mls_rfc9420".to_owned()),
                    mls_group_id: None,
                    source: "accepted_realm_create_direct_conversation".to_owned(),
                    updated_at: Utc::now(),
                })?;
            }
            if let Ok(value) = serde_json::to_value(&event) {
                crypto_store.record_direct_conversation_binding_from_value(&value)?;
                apply_account_mls_commits_from_value_tree(
                    client,
                    crypto_store,
                    &value,
                    channel,
                    account,
                    "committed_stream",
                )
                .await;
            }
            let mut parsed =
                parse_backfill_events_for_account(&delta.realm_id, vec![event], account);
            if crypto_store
                .direct_conversation_binding_event_ref(delta.realm_id.as_str())?
                .is_some()
                || crypto_store
                    .load()?
                    .realm_policies
                    .get(delta.realm_id.as_str())
                    .is_some_and(|policy| {
                        policy.source == "accepted_realm_create_direct_conversation"
                    })
            {
                for event in &mut parsed.events {
                    event.chat_type = Some("dm".to_owned());
                }
            }
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
        }
        ClientEvent::Message(_) => {}
        ClientEvent::ToDevice(_) => {}
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
) -> anyhow::Result<Option<DidCoreId>> {
    account_cursor_service_id(channel, account)
        .map(|value| {
            DidCoreId::new(value.clone())
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
) -> anyhow::Result<()> {
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
    consume_pending_mls_welcomes(client, channel, account, crypto_store).await;
    Ok(())
}

async fn consume_pending_mls_welcomes(
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
    consume_account_mls_key_packages(client, channel, account, crypto_store, &pending).await;
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
    let principal = match DidCoreId::new(account.principal_id.clone()) {
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
    let mut records = Vec::with_capacity(1);
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
    let Some(authorized_event_ref) = account.authorized_event_ref.as_deref() else {
        warn!(
            channel_id = %channel.id,
            account_id = %account.id,
            "arkret: Agent MLS KeyPackage upload requires the current authorize Event"
        );
        return;
    };
    match crypto_store.ensure_agent_mls_key_package(
        &account.actor_account_id,
        key_ref,
        verification_method,
        authorized_event_ref,
    ) {
        Ok(record) => records.push(record),
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to ensure local MLS KeyPackage: {err:#}"
            );
            return;
        }
    }

    let deficit = match crypto_store
        .mls_key_package_maintenance_deficit(&records[0].endpoint, KEYPACKAGE_MIN_AVAILABLE)
    {
        Ok(deficit) => deficit,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to inspect local MLS KeyPackage inventory: {err:#}"
            );
            return;
        }
    };
    if deficit > 0 {
        match crypto_store.create_fresh_agent_mls_key_packages(
            &account.actor_account_id,
            deficit,
            key_ref,
            verification_method,
            authorized_event_ref,
        ) {
            Ok(fresh) => records.extend(fresh),
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    deficit,
                    "arkret: failed to replenish local MLS KeyPackage pool: {err:#}"
                );
                return;
            }
        }
    }
    let records = match crypto_store.pending_key_package_uploads(&records[0].endpoint) {
        Ok(records) if records.is_empty() => return,
        Ok(records) => records,
        Err(error) => {
            warn!(channel_id = %channel.id, account_id = %account.id, "arkret: pending KeyPackage uploads unavailable: {error:#}");
            return;
        }
    };
    upload_account_mls_key_packages(
        client,
        channel,
        account,
        principal,
        &records,
        crypto_store,
        key_ref,
        verification_method,
        authorized_event_ref,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn upload_account_mls_key_packages(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    principal: DidCoreId,
    records: &[MlsKeyPackageRecord],
    crypto_store: &FileArkretCryptoStore,
    key_ref: &ArkretKeyRef,
    verification_method: &str,
    authorized_event_ref: &str,
) {
    let request = match build_signed_keypackage_upload_request(
        arkret::ActorId::account(account.actor_account_id.clone()),
        principal,
        records,
        key_ref,
        verification_method,
        authorized_event_ref,
    ) {
        Ok(request) => request,
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: failed to build canonical MLS KeyPackage upload request: {err:#}"
            );
            return;
        }
    };

    match client.inner().keypackages_upload(&request).await {
        Ok(outcome) => {
            let accepted_refs = outcome
                .key_package_refs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if let Err(error) = crypto_store.confirm_key_package_uploads(&accepted_refs) {
                warn!(channel_id = %channel.id, account_id = %account.id, "arkret: upload receipt persistence failed: {error:#}");
                return;
            }
            if !outcome.rejections.is_empty() {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    accepted = outcome.accepted,
                    rejected = ?outcome.rejections,
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
        Err(err) => {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                "arkret: MLS KeyPackage upload failed: {err}"
            );
        }
    }
}

fn consume_outcome_acknowledges_binding(
    outcome: &KeyPackagesConsumeOutcome,
    claim_id: &str,
    keypackage_ref: &str,
) -> bool {
    outcome.consume_receipt.claim_id.as_str() == claim_id
        && outcome
            .consume_receipt
            .recipient_durable_receipt
            .key_package_ref
            .as_str()
            == keypackage_ref
}

/// Outcome of an explicit Agent runtime unbind, surfaced to the RPC caller.
pub(crate) struct ArkretUnbindReport {
    pub principal_id: String,
    pub device_id: String,
    pub listeners_stopped: usize,
}

/// Explicitly unbind the Agent currently bound to this channel/account.
///
/// Stop the listener and purge local Agent key, MLS and subscribe state. The
/// controller's accepted key replacement fences the old remote KeyPackage
/// pool; the revoke endpoint is device-only and cannot carry an Agent signer.
/// The caller clears the persisted channel binding afterward.
pub(crate) async fn unbind_arkret_account(
    savfox_home: &std::path::Path,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<ArkretUnbindReport> {
    let listeners_stopped = stop_arkret_account_listeners(&channel.id);

    let crypto_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &account.id);

    let key_ref = account
        .key_ref
        .as_ref()
        .context("Agent unbind requires the authorized runtime key reference")?;
    savfox_channels::arkret::delete_ed25519_key_ref_from_keyring(key_ref)
        .context("delete retired Agent runtime key from the platform credential vault")?;

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
        "arkret: unbound Agent runtime; local state purged"
    );

    Ok(ArkretUnbindReport {
        principal_id: account.principal_id.clone(),
        device_id: account.device_id.clone(),
        listeners_stopped,
    })
}

fn build_signed_keypackage_upload_request(
    actor_id: arkret::ActorId,
    principal_id: DidCoreId,
    records: &[MlsKeyPackageRecord],
    key_ref: &ArkretKeyRef,
    verification_method: &str,
    authorized_event_ref: &str,
) -> anyhow::Result<KeyPackagesUploadRequestBody> {
    let key_packages = records
        .iter()
        .map(arkret::mls_key_package_record_upload_entry)
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::msg)
        .context("project canonical MLS KeyPackage upload entries")?;
    let unsigned = KeyPackagesUploadUnsignedRequest {
        actor_id,
        principal_id,
        device_id: None,
        pairwise_verification_method: None,
        intended_realm_id: None,
        agent_verification_method: Some(
            arkret::DidUrl::new(verification_method.to_owned()).map_err(anyhow::Error::msg)?,
        ),
        agent_key_authorize_event_id: Some(EventId::new(authorized_event_ref.to_owned())?),
        keypackages: key_packages,
        expires_at: None,
        strand_id: None,
        mls_group_id: None,
    };
    let endpoint_signature =
        sign_keypackages_upload_request(key_ref, verification_method, &unsigned)?;
    Ok(unsigned.into_signed(endpoint_signature))
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
    let mut cursor = if let Some(cursor) = initial_cursor {
        Some(cursor)
    } else {
        let scope = receive::delivery_scope(channel, account);
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

        for delivery in &outcome.deliveries {
            match delivery {
                arkret::RecipientDelivery::DeviceMessage { .. } => {
                    warn!(channel_id = %channel.id, account_id = %account.id,
                        "arkret: Agent DeviceMessage handler is unavailable; retaining the recipient queue without ACK");
                    return;
                }
                arkret::RecipientDelivery::MlsWelcome { mls_welcome } => {
                    match governance::admit_owned_agent_welcome_delivery(
                        client.inner(),
                        crypto_store,
                        mls_welcome,
                        account,
                    )
                    .await
                    {
                        Ok((admitted, accepted)) => {
                            if let Err(error) = receive::seed_welcome_checkpoint(
                                client.inner(),
                                channel,
                                account,
                                account_store,
                                mls_welcome,
                                &accepted,
                            )
                            .await
                            {
                                warn!(channel_id = %channel.id, account_id = %account.id,
                                    "arkret: Welcome stream checkpoint pending; retaining queue: {error:#}");
                                return;
                            }
                            debug!(
                            channel_id = %channel.id,
                            account_id = %account.id,
                            welcome_id = %mls_welcome.welcome_id,
                            admitted,
                            "arkret: accepted MLS Welcome delivery is durable"
                            );
                        }
                        Err(error) => {
                            warn!(
                                channel_id = %channel.id,
                                account_id = %account.id,
                                reason,
                                welcome_id = %mls_welcome.welcome_id,
                                "arkret: MLS Welcome admission pending; preserving the queue: {error:#}"
                            );
                            return;
                        }
                    }
                }
            }
        }
        if outcome.lost == Some(true) {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                page,
                "arkret: device_messages reported cursor loss; clearing local cursor without ack"
            );
            let clear = receive::delivery_scope(channel, account);
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
        if !outcome.deliveries.is_empty() && outcome.ack_token.is_none() {
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
            let save = receive::delivery_scope(channel, account);
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
        if outcome.limited == Some(true) {
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
        Ok(outcome) => {
            debug!(
                channel_id = %channel.id,
                account_id = %account.id,
                reason,
                pruned_count = outcome.pruned_count,
                "arkret: acknowledged standard device messages"
            );
            true
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
        let Some(recipient_durable_receipt) = binding.recipient_durable_receipt.clone() else {
            warn!(
                channel_id = %channel.id,
                account_id = %account.id,
                keypackage_ref = %binding.keypackage_ref,
                "arkret: deferring MLS KeyPackage consume until recipient durable receipt is available"
            );
            continue;
        };
        let claim_id = match arkret::identifiers::KeypackageClaimId::new(binding.claim_id.clone()) {
            Ok(value) => value,
            Err(error) => {
                warn!(channel_id = %channel.id, account_id = %account.id, keypackage_ref = %binding.keypackage_ref, "arkret: invalid claim id: {error}");
                continue;
            }
        };
        let unsigned = KeyPackagesConsumeUnsignedRequest {
            claim_id,
            recipient_durable_receipt,
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
                if consume_outcome_acknowledges_binding(
                    &outcome,
                    &binding.claim_id,
                    &binding.keypackage_ref,
                ) =>
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
                    "arkret: consumed MLS KeyPackage after Welcome decrypt"
                );
            }
            Ok(outcome) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    keypackage_ref = %binding.keypackage_ref,
                    receipt = ?outcome.consume_receipt,
                    "arkret: MLS KeyPackage consume receipt does not match the pending binding"
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
        // the endpoint's private inventory supplies the observed deficit.
        publish_account_mls_key_packages(client, channel, account, crypto_store).await;
    }
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
    streams: Vec<arkret::CommitStreamRef>,
}

#[derive(Debug)]
struct AccountScanCatchupOutcome {
    events: Vec<arkret::Event>,
    limited: bool,
    pages: usize,
}

async fn collect_account_scan_catchup<F, Fut>(
    request: AccountScanCatchupRequest,
    mut fetch: F,
) -> anyhow::Result<AccountScanCatchupOutcome>
where
    F: FnMut(arkret::RealmId, arkret::CommitStreamRef, Option<u64>, u16) -> Fut,
    Fut: Future<Output = anyhow::Result<arkret::StreamScanOutcome>>,
{
    let mut events = Vec::new();
    let mut pages = 0;
    let mut limited = false;

    if request.streams.is_empty() {
        anyhow::bail!("account scan has no visible commit streams");
    }

    for stream_ref in request.streams {
        if stream_ref.realm_id() != &request.realm_id {
            anyhow::bail!("account scan stream belongs to another Realm");
        }
        let mut after_position = None;
        let mut stream_limited = false;
        for _ in 0..ACCOUNT_SCAN_CATCHUP_MAX_PAGES {
            pages += 1;
            let outcome = fetch(
                request.realm_id.clone(),
                stream_ref.clone(),
                after_position,
                ACCOUNT_SCAN_CATCHUP_LIMIT,
            )
            .await?;
            let last_position = outcome
                .committed_events
                .last()
                .map(|item| item.commit().stream_position);
            for item in outcome.committed_events {
                item.validate_shape()?;
                if item.commit().stream_ref != stream_ref {
                    anyhow::bail!("account scan returned an event from another stream");
                }
                if let arkret::CommittedEventView::Full(full) = item {
                    events.push(full.event);
                }
            }
            stream_limited = outcome.truncated;
            match (outcome.truncated, last_position) {
                (true, Some(position)) => after_position = Some(position),
                (true, None) => anyhow::bail!("truncated account scan did not advance"),
                (false, _) => break,
            }
        }
        limited |= stream_limited;
    }

    Ok(AccountScanCatchupOutcome {
        events,
        limited,
        pages,
    })
}

fn parse_backfill_events_for_account(
    realm_id: &arkret::RealmId,
    events: Vec<arkret::Event>,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let event_values = events
        .into_iter()
        .filter(|event| &event.realm_id == realm_id)
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect::<Vec<_>>();
    parse_event_values_for_account(&event_values, account)
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
                refs.iter()
                    .any(|value| value.as_str() == Some("ak.profile.direct_conversation_realm.v1"))
            })
}

async fn apply_account_mls_commits_from_value_tree(
    client: &ArkretHttpClient,
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
        let needs_install = match crypto_store.mls_commit_needs_accepted_leaf_authority(&payload) {
            Ok(needs_install) => needs_install,
            Err(err) => {
                warn!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    source,
                    event_id = %event_ref,
                    "arkret: local MLS state unreadable, Commit not applied: {err:#}"
                );
                continue;
            }
        };
        if !needs_install {
            continue;
        }
        let scope = payload.governance_binding().effective_scope();
        let Some(realm_id) = scope.realm_id_opt() else {
            warn!(event_id = %event_ref, "arkret: MLS Commit has no Realm scope");
            continue;
        };
        let base = match crypto_store
            .station_mls_current_for_scope_epoch(scope, payload.base_epoch())
        {
            Ok(Some(base)) => base,
            Ok(None) => {
                warn!(event_id = %event_ref, base_epoch = payload.base_epoch(),
                    "arkret: exact Station MLS base current is unavailable; Commit remains pending");
                continue;
            }
            Err(err) => {
                warn!(event_id = %event_ref,
                    "arkret: Station MLS base current cannot be loaded: {err:#}");
                continue;
            }
        };
        let accepted = match governance::accepted_commit_for_event(
            client.inner(),
            realm_id,
            scope,
            &event_ref,
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(err) => {
                warn!(event_id = %event_ref,
                    "arkret: full accepted MLS Commit is unavailable: {err:#}");
                continue;
            }
        };
        match crypto_store.install_accepted_mls_commit(&accepted, &base, &[]) {
            Ok(true) => {
                applied += 1;
                debug!(
                    channel_id = %channel.id,
                    account_id = %account.id,
                    source,
                    event_id = %event_ref,
                    group_id = ?payload.mls_group_id(),
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
                    group_id = ?payload.mls_group_id(),
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
                    "arkret: encrypted account message remains pending; local MLS decryption is not ready"
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
    if !account.has_requested_scope(ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1) {
        delivery_store
            .mark_history_unavailable(
                conversation,
                "history_unavailable: account lacks ak.self.committed_event.read.scan.v1",
            )
            .await?;
        return Ok(());
    }
    let realm_id = RealmId::new(trigger.realm_id.clone())?;
    let outcome = async {
        let snapshot = client.inner().realm_state_snapshot_head(&realm_id).await?;
        let request = AccountScanCatchupRequest {
            realm_id: realm_id.clone(),
            streams: snapshot
                .visible_stream_heads
                .into_iter()
                .map(|head| head.stream_ref)
                .collect(),
        };
        collect_account_scan_catchup(request, |realm_id, stream_ref, after, limit| async move {
            client
                .inner()
                .scan_commit_stream_tail(realm_id, stream_ref, after, limit)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
    }
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            delivery_store
                .mark_history_unavailable(
                    conversation,
                    &format!("history_unavailable: committed stream scan failed: {error}"),
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
                    realm_id: realm_id.clone(),
                    chat_type: if crypto_store
                        .direct_conversation_binding_event_ref(&realm_id)?
                        .is_some()
                    {
                        Some("dm".to_owned())
                    } else {
                        skipped.chat_type.clone()
                    },
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
    state.account_id == account.actor_account_id
        && state.expires_at > Utc::now()
        && savfox_gateway_shared::arkret::session_scope_matches_request(
            &account.requested_scope,
            &state.granted_scope,
        )
        && savfox_channels::arkret::missing_required_scope_actions(
            &state.granted_scope,
            account.listen,
            account.send,
        )
        .is_empty()
        && state.device_id.is_none()
        && state.audience_id == account.actor_account_id.station_id
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
    let (Some(actor_id), Some(strand_id), Some(payload), Some(event_id)) = (
        skipped.sender_actor_id.as_ref(),
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
    let Some(control) = gate_inbound_exchange_control(
        &plaintext,
        strand_id,
        strand_id,
        actor_id,
        &account.controller_account_id,
    ) else {
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
    match store.record_terminal_control(
        &account.controller_account_id,
        strand_id,
        &control,
        event_id,
    )? {
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
    let Some(actor_id) = skipped.sender_actor_id.as_ref() else {
        return Ok(SidecarConsumeOutcome::DropSilently);
    };
    match gate_inbound_request_binding(
        &binding,
        event_id,
        actor_id,
        &account.controller_account_id,
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
                &account.controller_account_id,
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
    let principal = DidCoreId::new(account.principal_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid principal_id: {err}"))?;
    let _device_id = DeviceId::new(account.device_id.clone())
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
        None,
    )
    .await?;
    let granted = provider
        .session()
        .current_state()
        .context("Agent session provider returned no accepted grant")?;
    anyhow::ensure!(
        savfox_gateway_shared::arkret::session_scope_matches_request(
            &account.requested_scope,
            &granted.granted_scope
        ),
        "Agent session grant differs from the exact requested runtime scope"
    );
    savfox_channels::arkret::save_verified_runtime_scope(
        savfox_home,
        &channel.id,
        account,
        runtime_public_key_digest.clone(),
        &granted.granted_scope,
    )
    .await?;
    info!(
        "arkret: agent '{}' obtained DPoP-bound session; audience='{}' expires_at='{}'",
        account.id, audience, session.expires_at
    );
    Ok(provider)
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
    let realm_id_typed = RealmId::new(realm_id.to_owned())?;
    let strand_id = strand_id.map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret account '{}' cannot send without an inbound Arkret strand id",
            account.id
        )
    })?;
    if authorization_fence_key(&channel, &account).is_some_and(|key| {
        known_revoked_authorizations()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(&key)
    }) {
        anyhow::bail!("Arkret runtime authorization is known revoked");
    }

    // One-shot send restores the same keyring-backed session grant as the
    // listener and participates in the shared refresh/client rebuild path.
    if let Some(context) = sidecar_exchange {
        // Defense in depth for work already queued when the controller closed
        // the exchange: a cached context must not author a response into a
        // terminal exchange, and it must still be the canonical request
        // (§7.2.2/§7.2.3). The terminal fact is durable and controller-authored,
        // so this is decided locally, not inferred from elapsed time — and it
        // runs before the network, the session, the actor chain and the store.
        let store = SidecarExchangeStore::for_account(savfox_home, &channel.id, &account.id);
        anyhow::ensure!(
            store.exchange_accepts_new_response(
                &account.controller_account_id,
                &strand_id,
                &context.exchange_id,
                &context.request_event_id,
            )?,
            "Arkret Sidecar exchange {} no longer accepts a response from this runtime",
            context.exchange_id
        );
    }
    anyhow::ensure!(
        account.authorized_event_ref.is_some(),
        "Arkret runtime has no locally verified active key authorization"
    );
    let provider = construct_account_provider(savfox_home, &channel, &account).await?;
    let client = ArkretHttpClient::from_inner(provider.provide().await?);
    let outbound_store = open_account_store(
        savfox_home,
        &channel.id,
        &account.id,
        ACCOUNT_EVENT_DEDUPE_MAX,
    )?;
    let crypto_store = FileArkretCryptoStore::for_account(savfox_home, &channel.id, &account.id);
    let conversation = crate::arkret_delivery::RemoteConversationKey {
        channel_config_id: channel.id.clone(),
        account_id: account.id.clone(),
        realm_id: realm_id.to_owned(),
        strand_id: strand_id.clone(),
    };
    let delivery_store = crate::arkret_delivery::ArkretExecutionBindingStore::new(savfox_home);
    let retained = match delivery {
        Some(delivery) => {
            delivery_store
                .checkpoint_submission(delivery.checkpoint_id, &conversation, body)
                .await?
        }
        None => None,
    };
    let frozen = if let Some(retained) = retained {
        retained
    } else {
        let request = MessageCreateRequest {
            scope_ref: arkret::ScopeRef::Realm {
                realm_id: realm_id_typed.clone(),
            },
            strand_id,
            body: body.to_owned(),
            actor_account_id: account.actor_account_id.clone(),
            thread_root_id: None,
            sidecar_exchange: sidecar_exchange.cloned(),
        };
        let mut event = build_message_create_event(&request)?;
        // This is the currently registered message payload carrier, not an
        // authority checkpoint or a substitute for the producer signature.
        let context = arkret::MessageAgentContext {
            agent_id: DidCoreId::new(account.principal_id.clone())?,
            operator_or_controller: account.controller_account_id.canonical_key()?,
            execution_purpose: if delivery.is_some() {
                "task_delivery_checkpoint"
            } else {
                "direct_conversation_reply"
            }
            .to_owned(),
            authorization_ref: realm_id.to_owned(),
        };
        event
            .payload
            .insert("agent_context".to_owned(), serde_json::to_value(context)?);
        apply_account_outbound_encryption(
            &crypto_store,
            realm_id,
            &mut event,
            sidecar_exchange,
            delivery,
        )?;
        let authored = savfox_channels::arkret::finalize_outbound_event(event)?;
        let key_ref = account
            .key_ref
            .as_ref()
            .context("runtime key is unavailable")?;
        let verification_method = account
            .verification_method
            .as_deref()
            .context("runtime verification method is unavailable")?;
        let frozen =
            garth::MessageAuthoringSession::from_authored_event(authored)?.sign_with(|event| {
                savfox_channels::arkret::sign_outbound_event_with_key(
                    event,
                    key_ref,
                    verification_method,
                )
                .map_err(|error| garth::Error::Protocol(error.to_string()))
            })?;
        match delivery {
            Some(delivery) => {
                delivery_store
                    .retain_checkpoint_submission(
                        delivery.checkpoint_id,
                        &conversation,
                        body,
                        frozen,
                    )
                    .await?
            }
            None => frozen,
        }
    };
    anyhow::ensure!(
        queued_message_matches_runtime(
            &frozen.request().submission.event,
            &account,
            &crypto_store,
        )?,
        "frozen producer submission no longer matches the configured runtime or encryption policy"
    );
    let queued = frozen.into_queued_submission();
    let event_id = queued.event_id.clone();
    let outbound = OutboundEngine::new(outbound_store, garth::SystemClock);
    outbound.enqueue(queued).await?;
    cancel_unusable_account_submissions(&outbound, &account, &crypto_store).await?;
    let authority = garth::AuthorityClient::new(client.inner().clone());
    let options = arkret::http_client::ClientRequestOptions::default();
    loop {
        match outbound.submit_next(&authority, &options).await? {
            OutboundEngineOutcome::Committed { item, .. } if item.event_id() == &event_id => {
                return Ok(event_id.to_string());
            }
            OutboundEngineOutcome::Rejected { item, reason_code }
                if item.event_id() == &event_id =>
            {
                anyhow::bail!("producer submission rejected: {reason_code}");
            }
            OutboundEngineOutcome::Failed { item, error, .. } if item.event_id() == &event_id => {
                anyhow::bail!("producer submission failed: {error}");
            }
            OutboundEngineOutcome::Retry { item, delay } if item.event_id() == &event_id => {
                anyhow::bail!(
                    "producer submission remains durably queued for retry in {} ms",
                    delay.as_millis()
                );
            }
            OutboundEngineOutcome::Idle => {
                let snapshot = outbound.snapshot().await?;
                let item = snapshot
                    .items
                    .iter()
                    .find(|item| item.event_id() == &event_id)
                    .context("durable queue lost the producer submission")?;
                if item.status == garth::SendQueueStatus::Committed && item.commit().is_some() {
                    return Ok(event_id.to_string());
                }
                anyhow::bail!(
                    "producer submission is not committed; durable state is {:?}",
                    item.status
                );
            }
            _ => {}
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
            principal_id: "ak:did_core:webvh:z6mkfixture:agent.example".into(),
            actor_account_id: arkret::AccountId::new(
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:agent.example").unwrap(),
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:station.example").unwrap(),
            ),
            device_id: "ak:device:01904100-0000-7000-8000-000000000001".into(),
            key_ref: None,
            verification_method: None,
            inkson_bootstrap: None,
            authorized_event_ref: None,
            signer_resolution_evidence_ref: None,
            current_signer_evidence: None,
            controller_account_id: arkret::AccountId::new(
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:controller.example").unwrap(),
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:controller-station.example")
                    .unwrap(),
            ),
            requested_scope: vec![
                ServiceOperationId::SELF_COMMITTED_EVENT_STREAM_SUBSCRIBE_V1.into(),
                ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1.into(),
                "ak.event.read".into(),
            ],
            listen: true,
            send: true,
        }
    }

    fn queued_message_fixture() -> (ArkretAccountConfig, arkret::Event) {
        let mut account = make_account();
        account.principal_id = "ak:did_core:web:agent.example".to_owned();
        account.actor_account_id = arkret::AccountId::new(
            DidCoreId::new(account.principal_id.clone()).unwrap(),
            DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        );
        let method = "did:web:agent.example#runtime-1";
        account.verification_method = Some(method.to_owned());
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
        let jwk = arkret::signatures::JsonWebKey::from_ed25519_verifying_key(
            &signing_key.verifying_key(),
        );
        let root = arkret::build_agent_signer_evidence(
            DidCoreId::new(account.principal_id.clone()).unwrap(),
            arkret::DidUrl::new(method).unwrap(),
            serde_json::from_value(serde_json::to_value(jwk).unwrap()).unwrap(),
            arkret::RealmCommitId::from_digest([74; 32]),
            "2026-09-16T00:00:00.000Z".parse().unwrap(),
        )
        .unwrap();
        let reference = root.signer_evidence_ref().unwrap();
        account.signer_resolution_evidence_ref = Some(reference.clone());
        account.current_signer_evidence = Some(arkret::KeyStateCurrentSignerEvidence {
            signer_resolution_evidence_ref: reference,
            authenticated_signer_evidence: root,
        });
        let request = MessageCreateRequest {
            scope_ref: arkret::ScopeRef::Realm {
                realm_id: realm_id(),
            },
            strand_id: arkret::StrandId::from_event_id(&EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [75; 32],
            ))
            .to_string(),
            body: "frozen producer bytes".to_owned(),
            actor_account_id: account.actor_account_id.clone(),
            thread_root_id: None,
            sidecar_exchange: None,
        };
        let event = build_message_create_event(&request).unwrap();
        let mut authored = savfox_channels::arkret::finalize_outbound_event(event).unwrap();
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(signing_key, method);
        savfox_channels::arkret::sign_outbound_event(&mut authored, &signer).unwrap();
        (account, authored.into_event())
    }

    #[test]
    fn queued_submission_verifies_current_key_even_when_method_is_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let crypto = FileArkretCryptoStore::for_account(home.path(), "support", "agent-account");
        let (mut account, event) = queued_message_fixture();
        assert!(queued_message_matches_runtime(&event, &account, &crypto).unwrap());
        let other_key = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
        let jwk =
            arkret::signatures::JsonWebKey::from_ed25519_verifying_key(&other_key.verifying_key());
        let evidence = account.current_signer_evidence.as_mut().unwrap();
        evidence.authenticated_signer_evidence.public_key_jwk =
            serde_json::from_value(serde_json::to_value(jwk).unwrap()).unwrap();
        evidence.signer_resolution_evidence_ref = evidence
            .authenticated_signer_evidence
            .signer_evidence_ref()
            .unwrap();
        account.signer_resolution_evidence_ref =
            Some(evidence.signer_resolution_evidence_ref.clone());
        assert!(!queued_message_matches_runtime(&event, &account, &crypto).unwrap());
    }

    #[test]
    fn queued_submission_rejects_another_station_and_an_unsigned_event() {
        let home = tempfile::tempdir().unwrap();
        let crypto = FileArkretCryptoStore::for_account(home.path(), "support", "agent-account");
        let (account, mut event) = queued_message_fixture();
        let mut other_account = account.clone();
        other_account.actor_account_id.station_id =
            DidCoreId::new("ak:did_core:web:other-station.example").unwrap();
        assert!(!queued_message_matches_runtime(&event, &other_account, &crypto).unwrap());
        event.producer_proof = None;
        assert!(!queued_message_matches_runtime(&event, &account, &crypto).unwrap());
    }

    /// Signed consume receipt every `KeyPackagesConsumeOutcome` now carries.
    fn consume_receipt_fixture(
        claim_id: &str,
        keypackage_ref: &str,
    ) -> arkret::KeyPackageConsumeReceipt {
        let device_verification_method = "did:webvh:example.org:service#key-1";
        arkret::KeyPackageConsumeReceipt {
            domain: arkret::NonEmptyString::new("ak.keypackage-consume-receipt.v1").unwrap(),
            request_digest: arkret::Hash::new(format!("sha256:{}", "22".repeat(32))).unwrap(),
            claim_id: arkret::identifiers::KeypackageClaimId::new(claim_id.to_owned()).unwrap(),
            recipient_durable_receipt: arkret::RecipientMlsDurableReceipt {
                domain: arkret::NonEmptyString::new("ak.recipient-mls-durable-receipt.v1").unwrap(),
                claim_request_id: arkret::Base64UrlString::new("Y2xhaW0tcmVxdWVzdC0x").unwrap(),
                key_package_ref: arkret::NonEmptyString::new(keypackage_ref).unwrap(),
                recipient: arkret::RecipientMlsDurableSigner::Device {
                    recipient_account_id: arkret::AccountId::new(
                        actor_id(),
                        arkret::DidCoreId::new("ak:did_core:webvh:example.org:service".to_owned())
                            .unwrap(),
                    ),
                    recipient_device_id: arkret::DeviceId::new(
                        "ak:device:01904100-0000-7000-8000-000000000006".to_owned(),
                    )
                    .unwrap(),
                    device_verification_method: arkret::DidUrl::new(
                        device_verification_method.to_owned(),
                    )
                    .unwrap(),
                },
                recipient_id: arkret::DidCoreId::new(
                    "ak:did_core:webvh:example.org:service".to_owned(),
                )
                .unwrap(),
                realm_id: realm_id(),
                mls_group_id: arkret::ScopeRef::Realm {
                    realm_id: realm_id(),
                }
                .canonical_mls_group_id()
                .unwrap(),
                mls_epoch: 1,
                welcome_ref: arkret::identifiers::MlsWelcomeDeliveryId::new(
                    "ak:mls_welcome_delivery:01904100-0000-7000-8000-000000000007",
                )
                .unwrap(),
                welcome_digest: arkret::Hash::new(format!("sha256:{}", "11".repeat(32))).unwrap(),
                durable_at: chrono::Utc::now(),
                signature: arkret::KeyOperationSignature {
                    kid: arkret::NonEmptyString::new(device_verification_method).unwrap(),
                    signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                    sig: arkret::Base64UrlString::new("c2lnbmF0dXJl").unwrap(),
                },
            },
            consumed_at: chrono::Utc::now(),
            signature: arkret::KeyOperationSignature {
                kid: arkret::NonEmptyString::new(device_verification_method).unwrap(),
                signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                sig: arkret::Base64UrlString::new("c2lnbmF0dXJl").unwrap(),
            },
        }
    }

    #[test]
    fn pending_welcome_consume_accepts_only_its_own_receipt_binding() {
        let claim_id = "ak:keypackage_claim:01904100-0000-7000-8000-000000000008";
        let keypackage_ref = "sha256:direct-welcome-keypackage";
        let outcome = KeyPackagesConsumeOutcome {
            consume_receipt: consume_receipt_fixture(claim_id, keypackage_ref),
        };
        assert!(consume_outcome_acknowledges_binding(
            &outcome,
            claim_id,
            keypackage_ref
        ));
        assert!(!consume_outcome_acknowledges_binding(
            &outcome,
            claim_id,
            "sha256:another-keypackage"
        ));
        assert!(!consume_outcome_acknowledges_binding(
            &outcome,
            "ak:keypackage_claim:01904100-0000-7000-8000-000000000009",
            keypackage_ref
        ));
    }

    #[test]
    fn account_sync_extracts_strongly_typed_mls_commit_event() {
        let realm_id = realm_id();
        let identity = arkret::mls::ArkretMlsIdentity::new_human_device(
            arkret::ActorId::account(arkret::AccountId::new(actor_id(), principal_server_id())),
            arkret::DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned())
                .unwrap(),
            arkret::mls::ArkretMlsSigner::from_ed25519_signing_key(
                ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]),
            ),
        )
        .unwrap();
        let mut group = identity
            .create_group(&arkret::ScopeRef::Realm {
                realm_id: realm_id.clone(),
            })
            .unwrap();
        let commit = group.self_update_commit().unwrap();
        let base_ref =
            arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [0x01; 32]);
        let governance_binding = arkret::MlsGovernanceBindingPayload::realm(
            realm_id,
            Some(base_ref.clone()),
            0,
            commit.epoch,
            0,
        )
        .unwrap();
        let payload =
            arkret::MlsCommitPayload::new(base_ref, 0, &commit, governance_binding).unwrap();
        let event_ref =
            arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [0x11; 32]);
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
            Some("ak:did_core:webvh:z6mkfixture:agent.example"),
            &account,
        ));
        assert!(arkret_sender_is_account_principal(
            Some("AK:DID_CORE:WEBVH:Z6MKFIXTURE:AGENT.EXAMPLE"),
            &account,
        ));
        assert!(!arkret_sender_is_account_principal(
            Some("ak:did_core:webvh:z6mkfixture:controller.example"),
            &account,
        ));
        assert!(!arkret_sender_is_account_principal(None, &account));
    }

    #[test]
    fn initial_account_catchup_selects_history_baseline_mode() {
        let initial = ClientEvent::AccountUpdates(garth::AccountUpdateContext {
            initial_catchup: true,
            ..Default::default()
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
    fn agent_scope_typed_refresh_and_bare_stream_reasons_reach_diagnostics() {
        let account = make_account();
        let channel = ArkretChannelConfig {
            id: format!("scope-reasons-{}", uuid::Uuid::now_v7()),
            base_url: "https://arkret.example.org".to_owned(),
            service_id: None,
            delivery_mode: "interactive_chat".to_owned(),
            accounts: vec![account.clone()],
        };
        let key = task_key(&channel.id, &account.id);
        runtime_state().lock().unwrap().diagnostics.insert(
            key.clone(),
            ArkretListenerDiagnostic::new(&channel, &account),
        );
        for reason in [
            "agent_provision_scope_migration_required",
            "agent_key_scope_reauthorization_required",
            "agent_session_scope_refresh_required",
        ] {
            let api_error = garth::Error::Api {
                status: 412,
                error: Box::new(
                    arkret::Problem::from_code("failed_precondition", "rejected")
                        .with_extension("reason_code", json!(reason)),
                ),
            };
            record_listener_service_failure(
                &channel,
                &account,
                "authentication_error",
                &api_error,
                transport_service_reason(&api_error),
            );
            assert_eq!(
                runtime_state().lock().unwrap().diagnostics[&key]
                    .last_reason_code
                    .as_deref(),
                Some(reason)
            );
            let message_encoded_error = garth::Error::Api {
                status: 412,
                error: Box::new(arkret::Problem::from_code(
                    "failed_precondition",
                    format!("reason_code={reason}; rejected"),
                )),
            };
            record_listener_service_failure(
                &channel,
                &account,
                "authentication_error",
                &message_encoded_error,
                transport_service_reason(&message_encoded_error),
            );
            assert_eq!(
                runtime_state().lock().unwrap().diagnostics[&key]
                    .last_reason_code
                    .as_deref(),
                Some(reason)
            );
            let error = anyhow::Error::new(api_error);
            assert_eq!(listener_service_reason(&error), Some(reason));
            record_listener_service_failure(
                &channel,
                &account,
                "subscribe_error",
                &error,
                listener_service_reason(&error),
            );
            assert_eq!(
                runtime_state().lock().unwrap().diagnostics[&key]
                    .last_reason_code
                    .as_deref(),
                Some(reason)
            );
            let wire_reason: Option<String> = None;
            record_listener_service_failure(
                &channel,
                &account,
                "unauthorized",
                "rejected",
                wire_reason.as_deref(),
            );
            assert_eq!(
                runtime_state().lock().unwrap().diagnostics[&key]
                    .last_reason_code
                    .as_deref(),
                None
            );
        }
        let details_win = garth::Error::Api {
            status: 412,
            error: Box::new(
                arkret::Problem::from_code(
                    "failed_precondition",
                    "reason_code=agent_session_scope_refresh_required; rejected",
                )
                .with_extension(
                    "reason_code",
                    json!("agent_key_scope_reauthorization_required"),
                ),
            ),
        };
        assert_eq!(
            transport_service_reason(&details_win),
            Some("agent_key_scope_reauthorization_required")
        );
        let generic = garth::Error::Protocol(
            "reason_code=agent_provision_scope_migration_required; rejected".to_owned(),
        );
        assert_eq!(transport_service_reason(&generic), None);
        runtime_state().lock().unwrap().diagnostics.remove(&key);
    }

    #[test]
    fn agent_scope_refreshed_grant_preserves_exact_runtime_identity_and_scope() {
        let mut account = make_account();
        account.principal_id = "ak:did_core:webvh:z6mkfixture:agent.example".to_owned();
        account.requested_scope = savfox_channels::arkret::default_agent_runtime_scope().unwrap();
        account.actor_account_id = arkret::AccountId::new(
            DidCoreId::new(account.principal_id.clone()).unwrap(),
            DidCoreId::new("ak:did_core:webvh:z6mkfixture:service.example").unwrap(),
        );
        let mut state = garth::SessionGrantState {
            account_id: arkret::AccountId::new(
                DidCoreId::new(account.principal_id.clone()).unwrap(),
                DidCoreId::new("ak:did_core:webvh:z6mkfixture:service.example").unwrap(),
            ),
            device_id: None,
            grant_id: arkret::identifiers::SessionGrantId::from_issuance_digest([0x11; 32]),
            grant_jwt: "redacted-test-grant".to_owned(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            audience_id: DidCoreId::new("ak:did_core:webvh:z6mkfixture:service.example").unwrap(),
            granted_scope: account.requested_scope.clone(),
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
            ServiceOperationId::SELF_REALM_READ_EXPORT_V1
        ));

        state
            .granted_scope
            .push(ServiceOperationId::SELF_REALM_READ_EXPORT_V1.to_owned());
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.granted_scope = account.requested_scope.clone();
        state
            .granted_scope
            .retain(|action| action != ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1);
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.granted_scope = account.requested_scope.clone();
        state.expires_at = Utc::now() - chrono::Duration::seconds(1);
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.expires_at = Utc::now() + chrono::Duration::minutes(5);

        state.granted_scope.clear();
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.granted_scope = account.requested_scope.clone();
        state.device_id =
            Some(DeviceId::new("ak:device:01904100-0000-7000-8000-000000000099").unwrap());
        assert!(!refreshed_grant_matches_account(
            &state,
            &account,
            "ak.event.read"
        ));
        state.device_id = None;
        state.account_id.principal_id =
            DidCoreId::new("ak:did_core:webvh:z6mkfixture:other.example").unwrap();
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
        arkret::RealmId::from_event_id(&arkret::EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            [1; 32],
        ))
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
            request_event_id: arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [0x31; 32],
            )
            .to_string(),
            coordinator_assignment_event_id: None,
        };
        let request = MessageCreateRequest {
            scope_ref: arkret::ScopeRef::Sidecar {
                realm_id: realm_id(),
                sidecar_id: arkret::SidecarId::from_event_id(&arkret::EventId::from_digest(
                    arkret::canonical::DigestSuite::Sha256,
                    [0x51; 32],
                )),
            },
            strand_id: arkret::StrandId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [0x11; 32],
            ))
            .to_string(),
            body: "final user-visible reply".to_owned(),
            actor_account_id: arkret::AccountId::new(
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:agent.example").unwrap(),
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkfixture:station.example").unwrap(),
            ),
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

    fn actor_id() -> DidCoreId {
        DidCoreId::new("ak:did_core:webvh:z6mkfixture:alice.example".to_owned()).unwrap()
    }

    fn principal_server_id() -> DidCoreId {
        DidCoreId::new("ak:did_core:webvh:z6mkfixture:principal-server.example".to_owned()).unwrap()
    }

    fn keypackage_record(
        principal_id: &DidCoreId,
        device_id: &DeviceId,
        marker: u8,
        last_resort: bool,
    ) -> MlsKeyPackageRecord {
        let key_package_bytes = vec![marker; 16];
        MlsKeyPackageRecord {
            keypackage_id: format!("ak:mls:kp:01904100-0000-7000-8000-0000000000{marker:02x}"),
            actor_id: arkret::ActorId::account(arkret::AccountId::new(
                principal_id.clone(),
                principal_server_id(),
            )),
            endpoint: arkret::MlsEndpointIdentity::human_device(
                principal_id.clone(),
                device_id.clone(),
            ),
            keypackage: URL_SAFE_NO_PAD.encode(&key_package_bytes),
            keypackage_ref: arkret::Hash::new(arkret::canonical::sha256_digest(&key_package_bytes))
                .unwrap(),
            cipher_suites: vec!["MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519".to_owned()],
            capabilities: vec!["ak.mls.rfc9420".to_owned()],
            state: arkret::MlsKeyPackageState::Published,
            claim_id: None,
            created_at: Utc::now(),
            expires_at: None,
            last_resort,
        }
    }

    #[test]
    fn keypackage_upload_uses_sdk_batch_transcript_without_entry_signatures() {
        let principal_id = DidCoreId::new("ak:did_core:web:agent.example".to_owned()).unwrap();
        let device_id =
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000001".to_owned()).unwrap();
        let verification_method = "did:web:agent.example#runtime-1".to_owned();
        let authorize_event_id = "ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM";
        let seed = [7_u8; 32];
        let key_ref = ArkretKeyRef::InlineSeedBase64 {
            value: STANDARD_NO_PAD.encode(seed),
        };
        let records = [
            keypackage_record(&principal_id, &device_id, 1, false),
            keypackage_record(&principal_id, &device_id, 2, true),
        ];

        let request = build_signed_keypackage_upload_request(
            arkret::ActorId::account(make_account().actor_account_id),
            principal_id.clone(),
            &records,
            &key_ref,
            &verification_method,
            authorize_event_id,
        )
        .unwrap();

        assert_eq!(request.device_id, None);
        assert_eq!(
            request
                .agent_verification_method
                .as_ref()
                .map(arkret::DidUrl::as_str),
            Some(verification_method.as_str())
        );
        assert_eq!(
            request
                .agent_key_authorize_event_id
                .as_ref()
                .map(arkret::EventId::as_str),
            Some(authorize_event_id)
        );
        assert_eq!(request.keypackages[0].last_resort, None);
        assert_eq!(request.keypackages[1].last_resort, Some(true));
        let unsigned = request.unsigned();
        let batch_input = arkret::keypackages_upload_signing_input(&unsigned).unwrap();
        let public_key = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        arkret::verify_keypackage_signing_input(
            &public_key,
            &verification_method,
            &batch_input,
            &request.endpoint_signature,
        )
        .unwrap();
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
}
