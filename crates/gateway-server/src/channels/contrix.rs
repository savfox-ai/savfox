//! Contrix channel runtime — phase 1 only.
//!
//! Owns one async task per (channel, account) pair. Each task:
//!
//! 1. Opens an NDJSON long-poll against `/api/v1/account/subscribe`.
//! 2. Reads frames one at a time from the SDK-provided
//!    [`ContrixFrameStream`] (Stream of `AccountSubscribeFrame`).
//! 3. Walks `delta` frames and extracts `cx.message.create` events.
//! 4. Dispatches each event to the agent pipeline.
//!
//! Outbound sends go through [`send_to_contrix_account`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use contrix_core::AccountSubscribeFrameKind;
use contrix_identifiers::{DeviceId, Did};
use savfox_channels::contrix::{
    ContrixAccountConfig, ContrixChannelConfig, ContrixFrameStream, ContrixHttpClient,
    ContrixInboundEvent, MessageCreateRequest, build_message_create_event, load_ed25519_signer,
    parse_delta_frame_for_account, resolve_contrix_outbound_account,
};
use savfox_core::channel::ChannelAction;
use tracing::{debug, info, warn};

use super::ChannelRegistry;
use super::runtime;
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// Per-(channel, account) runtime handles. Indexed by `{channel_id}::{account_id}`.
#[derive(Default)]
struct ContrixRuntimeState {
    handles: HashMap<String, tokio::task::JoinHandle<()>>,
}

const ACCOUNT_EVENT_DEDUPE_MAX: usize = 4096;

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

fn runtime_state() -> &'static StdMutex<ContrixRuntimeState> {
    static STATE: OnceLock<StdMutex<ContrixRuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| StdMutex::new(ContrixRuntimeState::default()))
}

fn task_key(channel_id: &str, account_id: &str) -> String {
    format!("{channel_id}::{account_id}")
}

pub(crate) async fn start_contrix_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let contrix_config = ContrixChannelConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Contrix channel config must be an object"))?;
    contrix_config
        .validate()
        .with_context(|| format!("Contrix channel '{}' config validation failed", config.id))?;

    info!(
        "contrix: channel '{}' validated; {} account(s), base_url='{}'",
        contrix_config.id,
        contrix_config.accounts.len(),
        contrix_config.base_url,
    );

    for account in &contrix_config.accounts {
        if account.listen {
            spawn_account_listener(
                contrix_config.clone(),
                account.clone(),
                Arc::clone(gateway_channel),
                Arc::clone(session_store),
            );
        }
    }

    Ok(())
}

fn spawn_account_listener(
    channel: ContrixChannelConfig,
    account: ContrixAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let key = task_key(&channel.id, &account.id);
    let handle = tokio::spawn(async move {
        run_account_subscribe_loop(channel, account, gateway_channel, session_store).await;
    });
    let Ok(mut state) = runtime_state().lock() else {
        warn!("contrix: runtime state mutex poisoned; aborting listener task '{key}'");
        handle.abort();
        return;
    };
    if let Some(prev) = state.handles.insert(key, handle) {
        prev.abort();
    }
}

async fn run_account_subscribe_loop(
    channel: ContrixChannelConfig,
    account: ContrixAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let mut backoff = Duration::from_secs(1);
    let mut cursor: Option<String> = None;
    let mut dedupe = EventDedupe::new(ACCOUNT_EVENT_DEDUPE_MAX);

    // Phase 8 (T8.D): if `key_ref` is configured, run DID-proof login to
    // obtain a session grant; otherwise fall back to bare-bearer mode
    // (Phase 1-7 behavior). Login failure is fatal — we don't silently
    // downgrade.
    let client = match construct_account_client(&channel, &account).await {
        Ok(client) => client,
        Err(err) => {
            warn!(
                "contrix: account '{}' on channel '{}' failed to construct HTTP client: {err:#}",
                account.id, channel.id
            );
            return;
        }
    };

    loop {
        runtime::record_channel_probe("contrix", "tick").await;
        let initial = cursor.is_none();
        match client
            .account_subscribe_stream(cursor.as_deref(), initial)
            .await
        {
            Ok(stream) => {
                let outcome = consume_stream(
                    stream,
                    &channel,
                    &account,
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
                        warn!(
                            "contrix: account '{}' became unauthorized mid-stream — stopping",
                            account.id
                        );
                        return;
                    }
                    StreamOutcome::Backoff => {
                        sleep_with_backoff(&mut backoff).await;
                    }
                }
            }
            Err(err) => {
                warn!(
                    "contrix: subscribe call for '{}/{}' failed: {err}",
                    channel.id, account.id
                );
                sleep_with_backoff(&mut backoff).await;
            }
        }
    }
}

enum StreamOutcome {
    Reconnect,
    ResetCursor,
    Unauthorized,
    Backoff,
}

async fn consume_stream(
    mut stream: ContrixFrameStream,
    channel: &ContrixChannelConfig,
    account: &ContrixAccountConfig,
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
                    "contrix: stream read error on '{}/{}': {err}",
                    channel.id, account.id
                );
                return StreamOutcome::Backoff;
            }
        };
        if let Some(new_cursor) = frame.cursor.clone() {
            *cursor = Some(new_cursor);
        }
        match frame.kind {
            AccountSubscribeFrameKind::Delta => {
                if let Some(realms) = &frame.realms {
                    let realms_value = serde_json::to_value(realms).unwrap_or_default();
                    let parsed = parse_delta_frame_for_account(&realms_value, account);
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
            }
            AccountSubscribeFrameKind::CatchupComplete => {
                debug!("contrix: '{}/{}' catchup complete", channel.id, account.id);
            }
            AccountSubscribeFrameKind::Frontier => {
                // cursor already updated above
            }
            AccountSubscribeFrameKind::Heartbeat => {}
            AccountSubscribeFrameKind::Dropped => {
                warn!(
                    "contrix: '{}/{}' stream dropped — resyncing",
                    channel.id, account.id
                );
                return StreamOutcome::ResetCursor;
            }
            AccountSubscribeFrameKind::ResyncRequired => {
                warn!(
                    "contrix: '{}/{}' resync required — resetting cursor",
                    channel.id, account.id
                );
                return StreamOutcome::ResetCursor;
            }
            AccountSubscribeFrameKind::Unauthorized => {
                return StreamOutcome::Unauthorized;
            }
        }
    }
}

async fn dispatch_to_agent(
    event: ContrixInboundEvent,
    channel: &ContrixChannelConfig,
    account: &ContrixAccountConfig,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    let agent_id = account.agent_id.clone();
    let config_id = channel.id.clone();
    let sender = event.sender_did.clone();
    let realm_id = event.realm_id.clone();
    let flow_id = event.flow_id.clone();
    let thread_id = event.thread_root_id.clone();
    let body = event.body.clone();

    tokio::spawn(async move {
        runtime::spawn_start_thread_pipeline_with_meta_coordinated(
            gateway_channel,
            session_store,
            "contrix",
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
                ..runtime::StartThreadMeta::default()
            }),
        )
        .await;
    });
}

async fn sleep_with_backoff(backoff: &mut Duration) {
    tokio::time::sleep(*backoff).await;
    let next = (backoff.as_secs() * 2).min(60);
    *backoff = Duration::from_secs(next.max(1));
}

/// Phase 8 (T8.D): build an authenticated `ContrixHttpClient` for one
/// account. If `account.key_ref` is set, runs DID-proof login (S-2) to
/// obtain a `cx.session.grant`; otherwise builds a bare-bearer client
/// from the static `access_token`.
async fn construct_account_client(
    channel: &ContrixChannelConfig,
    account: &ContrixAccountConfig,
) -> anyhow::Result<ContrixHttpClient> {
    if let Some(key_ref) = &account.key_ref {
        let vm = account
            .verification_method
            .clone()
            .unwrap_or_else(|| format!("{}#key-1", account.principal_id));
        let audience = account
            .contrix_server_did
            .clone()
            .or_else(|| channel.service_did.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "contrix account '{}' has key_ref but no contrix_server_did or \
                     channel.service_did for login audience",
                    account.id
                )
            })?;
        let signer = load_ed25519_signer(key_ref, &account.principal_id, &vm)?;
        let principal = Did::new(account.principal_id.clone())
            .map_err(|err| anyhow::anyhow!("invalid principal_id: {err}"))?;
        let device = DeviceId::new(account.device_id.clone())
            .map_err(|err| anyhow::anyhow!("invalid device_id: {err}"))?;
        let (client, _session) = ContrixHttpClient::login(
            &channel.base_url,
            &signer,
            principal,
            device,
            &vm,
            &audience,
        )
        .await?;
        info!(
            "contrix: account '{}' logged in via DID-proof; audience='{}'",
            account.id, audience
        );
        Ok(client)
    } else {
        ContrixHttpClient::new(&channel.base_url, &account.access_token)
    }
}

/// Send a `cx.message.create` event as one of the channel's configured
/// outbound accounts.
///
/// `realm_id` selects the account via
/// [`ContrixChannelConfig::select_send_account`]. `flow_id` falls back to the
/// account's `default_flow_id` if `None`.
pub(crate) async fn send_to_contrix_account(
    savfox_home: &std::path::PathBuf,
    realm_id: &str,
    flow_id: Option<&str>,
    body: &str,
) -> anyhow::Result<()> {
    let Some((channel, account)) = resolve_contrix_outbound_account(savfox_home, realm_id).await?
    else {
        anyhow::bail!("no Contrix channel configured for realm {realm_id}");
    };
    let flow = flow_id
        .map(str::to_owned)
        .or_else(|| account.default_flow_id.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Contrix account '{}' has no default_flow_id and caller did not supply one",
                account.id
            )
        })?;

    // Phase 8 (T8.D): same auth flow as the sync loop — login when
    // key_ref is set, bare bearer otherwise.
    let client = construct_account_client(&channel, &account).await?;
    let request = MessageCreateRequest {
        realm_id: realm_id.to_owned(),
        flow_id: flow,
        body: body.to_owned(),
        principal_id: account.principal_id.clone(),
        actor_seq: chrono::Utc::now().timestamp_millis().max(0) as u64,
        thread_root_id: None,
    };
    let mut event = build_message_create_event(&request)?;

    // Phase 8 (T8.E): attach capability grant event_id when configured.
    if let Some(grant_path) = &account.grant_event_path {
        let grant = savfox_channels::contrix::load_and_verify_grant(
            grant_path,
            &account.principal_id,
            account.default_realm_id.as_deref(),
        )
        .await
        .with_context(|| {
            format!(
                "Contrix account '{}' failed to load capability grant {}",
                account.id,
                grant_path.display()
            )
        })?;
        if !grant.covers_action("cx.message.create") {
            anyhow::bail!(
                "Contrix account '{}' capability grant {} does not cover cx.message.create",
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
            savfox_channels::contrix::load_ed25519_signer(key_ref, &account.principal_id, &vm)?;
        savfox_channels::contrix::sign_outbound_event(&mut event, &signer, &vm)?;
    }

    let response = client.submit_event(&event).await?;
    debug!(
        "contrix: submitted event to '{}': status={:?} accepted={}",
        realm_id,
        response.status,
        response.accepted.len()
    );
    Ok(())
}

/// Convenience wrapper for the channel registry's `ChannelAction::SendToThread`.
#[allow(dead_code)]
pub(crate) async fn handle_outbound_action(
    savfox_home: &std::path::PathBuf,
    action: ChannelAction,
) -> anyhow::Result<()> {
    if let ChannelAction::SendToThread { thread_id, message } = action {
        send_to_contrix_account(savfox_home, &thread_id, None, &message).await
    } else {
        Ok(())
    }
}
