//! Cokret personal-agent channel runtime.
//!
//! Owns one async task per (channel, account) pair. Each task:
//!
//! 1. Mints a short-lived `agent_key_proof` session grant bound to DPoP.
//! 2. Opens `/_cokret/self/events/subscribe` for the configured Realm.
//! 3. Extracts dispatchable `ck.message.create` events.
//! 4. Dispatches each event to the agent pipeline.
//!
//! Outbound sends go through [`send_to_cokret_account`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use cokret::{Did, EventsSubscribeFrameKind};
use savfox_channels::cokret::{
    CokretAccountConfig, CokretChannelConfig, CokretDecryptOutcome, CokretEncryptOutcome,
    CokretFrameStream, CokretHttpClient, CokretInboundEvent, CokretInboundParseResult,
    CokretInboundSkipReason, CokretInboundSkippedEvent, FileCokretCryptoStore,
    MessageCreateRequest, account_allows_event_read, build_message_create_event,
    parse_event_frame_for_account, resolve_cokret_outbound_account,
};
use serde_json::Value;
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

/// Refresh an agent session grant this many seconds before it expires.
const SESSION_REFRESH_SKEW_SECS: i64 = 60;

struct EventDedupe {
    seen: HashSet<String>,
    order: VecDeque<String>,
    max_len: usize,
}

impl EventDedupe {
    fn new(max_len: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            max_len,
        }
    }

    fn insert(&mut self, event_id: String) -> bool {
        if self.seen.contains(&event_id) {
            return false;
        }
        self.seen.insert(event_id.clone());
        self.order.push_back(event_id);
        while self.order.len() > self.max_len {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

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
        run_account_subscribe_loop(
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

async fn run_account_subscribe_loop(
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
    let mut cursor: Option<String> = None;
    let mut dedupe = EventDedupe::new(ACCOUNT_EVENT_DEDUPE_MAX);
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
    let Some(stream_realm_id) = account.default_realm_id.clone() else {
        warn!(
            "cokret: account '{}' on channel '{}' has listen=true but no defaultRealmId",
            account.id, channel.id
        );
        runtime::record_channel_probe("cokret", "error").await;
        return;
    };

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

        match client
            .events_subscribe_stream(&stream_realm_id, cursor.as_deref())
            .await
        {
            Ok(stream) => {
                let outcome = consume_stream(
                    stream,
                    &channel,
                    &account,
                    &crypto_store,
                    &mut cursor,
                    &mut dedupe,
                    &gateway_channel,
                    &session_store,
                )
                .await;
                match outcome {
                    StreamOutcome::Reconnect => {
                        backoff = Duration::from_secs(1);
                    }
                    StreamOutcome::ResetCursor => {
                        cursor = None;
                        dedupe.clear();
                        backoff = Duration::from_secs(1);
                    }
                    StreamOutcome::Unauthorized => {
                        // The agent session grant expired or was revoked. Back
                        // off and try a fresh agent_key_proof session exchange.
                        warn!(
                            "cokret: account '{}' became unauthorized mid-stream — refreshing agent session",
                            account.id
                        );
                        sleep_with_backoff(&mut backoff).await;
                        match construct_account_client(&channel, &account).await {
                            Ok((fresh, fresh_expiry)) => {
                                client = fresh;
                                session_expiry = fresh_expiry;
                                cursor = None;
                                dedupe.clear();
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
                    StreamOutcome::Backoff => {
                        sleep_with_backoff(&mut backoff).await;
                    }
                }
            }
            Err(err) => {
                warn!(
                    "cokret: subscribe call for '{}/{}' failed: {err}",
                    channel.id, account.id
                );
                sleep_with_backoff(&mut backoff).await;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamOutcome {
    Reconnect,
    ResetCursor,
    Unauthorized,
    Backoff,
}

fn session_grant_needs_refresh(session_expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    session_expiry
        .is_some_and(|expiry| now >= expiry - chrono::Duration::seconds(SESSION_REFRESH_SKEW_SECS))
}

fn stream_outcome_for_terminal_control_frame(
    kind: EventsSubscribeFrameKind,
) -> Option<StreamOutcome> {
    match kind {
        EventsSubscribeFrameKind::EpochRotation
        | EventsSubscribeFrameKind::Dropped
        | EventsSubscribeFrameKind::ResyncRequired => Some(StreamOutcome::ResetCursor),
        EventsSubscribeFrameKind::Unauthorized => Some(StreamOutcome::Unauthorized),
        EventsSubscribeFrameKind::Event
        | EventsSubscribeFrameKind::Frontier
        | EventsSubscribeFrameKind::Heartbeat
        | EventsSubscribeFrameKind::CatchupComplete => None,
        _ => None,
    }
}

async fn consume_stream(
    mut stream: CokretFrameStream,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    crypto_store: &FileCokretCryptoStore,
    cursor: &mut Option<String>,
    dedupe: &mut EventDedupe,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> StreamOutcome {
    use futures_util::StreamExt;
    loop {
        let frame = match stream.next().await {
            Some(Ok(frame)) => frame,
            None => return StreamOutcome::Reconnect,
            Some(Err(err)) => {
                debug!(
                    "cokret: stream read error on '{}/{}': {err}",
                    channel.id, account.id
                );
                return StreamOutcome::Backoff;
            }
        };
        if let Some(new_cursor) = frame.cursor.clone() {
            *cursor = Some(new_cursor.as_str().to_owned());
        }
        match frame.kind {
            EventsSubscribeFrameKind::Event => {
                let parsed = parse_event_frame_for_account(&frame.payload, account);
                handle_parsed_account_events(
                    parsed,
                    channel,
                    account,
                    crypto_store,
                    dedupe,
                    gateway_channel,
                    session_store,
                )
                .await;
            }
            EventsSubscribeFrameKind::CatchupComplete => {
                debug!("cokret: '{}/{}' catchup complete", channel.id, account.id);
            }
            EventsSubscribeFrameKind::Frontier => {
                // cursor already updated above
            }
            EventsSubscribeFrameKind::Heartbeat => {}
            EventsSubscribeFrameKind::EpochRotation => {
                return stream_outcome_for_terminal_control_frame(
                    EventsSubscribeFrameKind::EpochRotation,
                )
                .expect("epoch_rotation has a stream outcome");
            }
            EventsSubscribeFrameKind::Dropped => {
                warn!(
                    "cokret: '{}/{}' stream dropped — resyncing",
                    channel.id, account.id
                );
                return stream_outcome_for_terminal_control_frame(
                    EventsSubscribeFrameKind::Dropped,
                )
                .expect("dropped has a stream outcome");
            }
            EventsSubscribeFrameKind::ResyncRequired => {
                warn!(
                    "cokret: '{}/{}' resync required — resetting cursor",
                    channel.id, account.id
                );
                return stream_outcome_for_terminal_control_frame(
                    EventsSubscribeFrameKind::ResyncRequired,
                )
                .expect("resync_required has a stream outcome");
            }
            EventsSubscribeFrameKind::Unauthorized => {
                return stream_outcome_for_terminal_control_frame(
                    EventsSubscribeFrameKind::Unauthorized,
                )
                .expect("unauthorized has a stream outcome");
            }
            _ => {
                debug!(
                    "cokret: '{}/{}' ignored unknown events subscribe frame kind",
                    channel.id, account.id
                );
            }
        }
    }
}

async fn handle_parsed_account_events(
    parsed: CokretInboundParseResult,
    channel: &CokretChannelConfig,
    account: &CokretAccountConfig,
    crypto_store: &FileCokretCryptoStore,
    dedupe: &mut EventDedupe,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    for skipped in parsed.skipped {
        match skipped.reason {
            CokretInboundSkipReason::EncryptedContent => {
                let decrypted = try_handle_encrypted_account_skip(
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
        if !dedupe.insert(event.event_id.clone()) {
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
    let agent_id = account.agent_id.clone();
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
                forced_agent_id: agent_id,
                sender_kind,
                ..runtime::StartThreadMeta::default()
            }),
        )
        .await;
    });
}

async fn try_handle_encrypted_account_skip(
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

    match crypto_store.try_decrypt_content_block(payload) {
        Ok(CokretDecryptOutcome::Decrypted(content)) => {
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
        Ok(CokretDecryptOutcome::MissingGroupState) => {
            record_account_unable_to_decrypt(
                crypto_store,
                skipped,
                payload.clone(),
                cokret::crypto_protocol::UnableToDecryptReason::NoSession,
            );
            false
        }
        Ok(CokretDecryptOutcome::UnsupportedScheme(scheme)) => {
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
                .yougen_bootstrap
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
        account.default_realm_id.as_deref(),
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
/// `realm_id` selects the account via
/// [`CokretChannelConfig::select_send_account`]. `flow_id` falls back to the
/// account's `default_flow_id` if `None`.
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
    let flow = flow_id
        .map(str::to_owned)
        .or_else(|| account.default_flow_id.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cokret account '{}' has no default_flow_id and caller did not supply one",
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
        let grant = savfox_channels::cokret::load_and_verify_grant(
            grant_path,
            &account.principal_id,
            account.default_realm_id.as_deref(),
        )
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
    let Some(content_block) = event.content.get("content").cloned() else {
        return Ok(());
    };
    match crypto_store.encrypt_content_block_for_realm(realm_id, &content_block)? {
        CokretEncryptOutcome::PlaintextAllowed => Ok(()),
        CokretEncryptOutcome::Encrypted(encrypted_content) => {
            let object = event
                .content
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
    fn terminal_control_frames_refresh_session_only_when_unauthorized() {
        assert_eq!(
            stream_outcome_for_terminal_control_frame(EventsSubscribeFrameKind::Unauthorized),
            Some(StreamOutcome::Unauthorized)
        );
        assert_eq!(
            stream_outcome_for_terminal_control_frame(EventsSubscribeFrameKind::Dropped),
            Some(StreamOutcome::ResetCursor)
        );
        assert_eq!(
            stream_outcome_for_terminal_control_frame(EventsSubscribeFrameKind::ResyncRequired),
            Some(StreamOutcome::ResetCursor)
        );
        assert_eq!(
            stream_outcome_for_terminal_control_frame(EventsSubscribeFrameKind::EpochRotation),
            Some(StreamOutcome::ResetCursor)
        );
        assert_eq!(
            stream_outcome_for_terminal_control_frame(EventsSubscribeFrameKind::CatchupComplete),
            None
        );
    }
}
