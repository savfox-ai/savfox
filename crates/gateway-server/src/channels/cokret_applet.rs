//! Cokret Applet HTTP server endpoints.
//!
//! When savfox runs as a registered Cokret Applet (savfox channel config
//! with `kind = "cokret"` + `mode = "applet"`), this module hosts the
//! inbound HTTP routes the Cokret server calls. Mirrors the Matrix
//! Appservice route layout in [`channels::matrix`](crate::channels::matrix)
//! one-for-one:
//!
//! Paths follow the Cokret spec `edge` trust segment (applet-integration.md
//! §6) — versionless, under the `/_cokret/edge/applet/...` namespace:
//!
//! | path | method | corresponds to |
//! |---|---|---|
//! | `/_cokret/edge/applet/ping` | GET | Matrix `/_matrix/app/v1/ping` |
//! | `/_cokret/edge/applet/describe` | GET | (new — capability descriptor) |
//! | `/_cokret/edge/applet/transactions` | POST | Matrix `PUT /_matrix/app/v1/transactions/{txn_id}` |
//! | `/_cokret/edge/applet/actors/{actor_id}` | GET | Matrix `/_matrix/app/v1/users/{user_id}` |
//! | `/_cokret/edge/applet/realms/{realm_id_or_alias}` | GET | Matrix `/_matrix/app/v1/rooms/{room_alias}` |
//! | `/_cokret/edge/applet/protocols/{protocol}` | GET | Matrix `/_matrix/app/v1/thirdparty/protocol/{protocol}` |
//! | `/_cokret/edge/applet/third_party/users` | GET | Third-party actor lookup |
//! | `/_cokret/edge/applet/third_party/locations` | GET | Third-party realm lookup |
//!
//! Two mount points (mirroring matrix.rs convention):
//!
//! * Direct: `/_cokret/edge/applet/...` — auth resolves the channel via bearer.
//! * Per-config: `/appservices/cokret/{config_id}/_cokret/edge/applet/...`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use cokret::http_signature::{
    Component, HttpMessageVerificationError, SignaturePolicyError, SignatureVerificationPolicy,
    parse_signature_input, public_key_from_bytes, verify_signed_http_message,
};
use cokret::{
    AppletActorView, AppletDescription, AppletPingOutcome, AppletProtocolMetadata, AppletRealmView,
    AppletTransactionOutcome, AppletTransactionRequestBody, Did, Hash, IdempotencyClaim,
    IdempotencyDirection, IdempotencyIdentity, IdempotencyWindow, canonical,
};
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::cokret::applet::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, CokretAppletConfig,
    classify_inbound_event, load_cokret_applet_configs,
};
use savfox_channels::cokret::{
    CokretDecryptOutcome, CokretEncryptOutcome, FileCokretCryptoStore,
    extract_encrypted_payload_from_message_content,
};
use serde_json::{Map, Value, json};
use subtle::ConstantTimeEq;
use tracing::{debug, info, warn};

use super::{render_error, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// Per-config in-memory state. We don't try to persist anything yet — the
/// idempotency window is 5 minutes; cold restart cleanly accepts a retry.
///
/// Phase 7: `txn_dedupe` is now an SDK [`IdempotencyWindow`] (S-5) that
/// implements spec applet-integration.md §7.3 properly — including
/// `duplicate_conflict` detection when the same `(source_service_did,
/// idempotency_key)` arrives with a different canonical body hash.
#[derive(Debug)]
struct AppletRuntimeState {
    txn_dedupe: IdempotencyWindow<AppletTransactionOutcome>,
}

impl Default for AppletRuntimeState {
    fn default() -> Self {
        Self {
            txn_dedupe: IdempotencyWindow::new(TXN_DEDUPE_WINDOW),
        }
    }
}

struct AppletChannelState {
    config: CokretAppletConfig,
    runtime: Mutex<AppletRuntimeState>,
    crypto_store: FileCokretCryptoStore,
    /// Restart-safe monotonic allocator for this applet's outbound
    /// `actor_seq`. Backed by a file-backed [`SeqStore`] under the savfox
    /// home dir; replaces the previous `timestamp_millis()` hack which was
    /// neither monotonic across calls nor restart-safe. `SeqAllocator` is
    /// internally synchronized, so it lives outside the `runtime` Mutex.
    seq: cokret_bridge_runtime::SeqAllocator,
}

// `SeqAllocator` is not `Debug`; provide a manual impl that elides it so
// `AppletChannelState` keeps a `Debug` representation for tracing/asserts.
impl std::fmt::Debug for AppletChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppletChannelState")
            .field("config", &self.config)
            .field("runtime", &self.runtime)
            .field("crypto_store_path", &self.crypto_store.path())
            .finish_non_exhaustive()
    }
}

type AppletRegistry = HashMap<String, Arc<AppletChannelState>>;

fn applet_registry() -> &'static Mutex<AppletRegistry> {
    static REGISTRY: OnceLock<Mutex<AppletRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

const TXN_DEDUPE_WINDOW: Duration = Duration::from_secs(300);
const MAX_APPLET_TRANSACTION_BODY_BYTES: usize = 65_536;
const SOURCE_SERVICE_DID_HEADER: &str = "source-service-did";
const DESTINATION_SERVICE_DID_HEADER: &str = "destination-service-did";
const APPLET_TRANSACTION_SIGNATURE_MAX_LIFETIME_SECS: i64 = 300;
const APPLET_TRANSACTION_SIGNATURE_MAX_CLOCK_SKEW_SECS: i64 = 30;

fn register_channel(state: AppletChannelState) -> anyhow::Result<()> {
    let mut reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    reg.insert(state.config.id.clone(), Arc::new(state));
    Ok(())
}

/// Remove a registered applet channel from the global registry.
///
/// Must be called when a Cokret applet channel is disabled, deleted, or
/// reconfigured. Without this, a stale `AppletChannelState` (carrying the
/// bearer token and namespace patterns) would linger forever and keep matching
/// `lookup_by_bearer` / `lookup_by_realm`, dispatching to a channel the
/// operator already removed. Mirrors `matrix::remove_matrix_appservice_channel`.
pub(crate) fn remove_cokret_applet_channel(config_id: &str) -> anyhow::Result<bool> {
    let mut reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg.remove(config_id).is_some())
}

pub(crate) fn is_cokret_applet_registered(config_id: &str) -> bool {
    let Ok(reg) = applet_registry().lock() else {
        return false;
    };
    reg.contains_key(config_id)
}

fn lookup_by_config_id(config_id: &str) -> anyhow::Result<Option<Arc<AppletChannelState>>> {
    let reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg.get(config_id).cloned())
}

fn applet_registry_is_empty() -> anyhow::Result<bool> {
    let reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg.is_empty())
}

fn lookup_by_bearer(req: &Request) -> anyhow::Result<Option<Arc<AppletChannelState>>> {
    let Some(token) = bearer_token(req) else {
        return Ok(None);
    };
    let reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg
        .values()
        .find(|state| applet_token_matches(state.config.cokret_bearer_token.as_deref(), &token))
        .cloned())
}

fn lookup_by_realm(realm_id: &str) -> anyhow::Result<Option<Arc<AppletChannelState>>> {
    let reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg
        .values()
        .find(|state| state.config.namespaces.realm_matches(realm_id))
        .cloned())
}

fn parse_bearer_header(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    let rest = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

fn bearer_token(req: &Request) -> Option<String> {
    req.header::<String>("authorization")
        .and_then(|hv| parse_bearer_header(&hv).map(str::to_owned))
}

fn applet_token_matches(configured: Option<&str>, provided: &str) -> bool {
    let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    bool::from(configured.as_bytes().ct_eq(provided.trim().as_bytes()))
}

fn release_transaction_claim(state: &AppletChannelState, identity: &IdempotencyIdentity) {
    match state.runtime.lock() {
        Ok(runtime_state) => {
            runtime_state.txn_dedupe.release(identity);
        }
        Err(_) => {
            warn!("applet: failed to release idempotency claim because runtime state is poisoned");
        }
    }
}

fn complete_transaction_claim(
    state: &AppletChannelState,
    identity: &IdempotencyIdentity,
    outcome: AppletTransactionOutcome,
) {
    match state.runtime.lock() {
        Ok(runtime_state) => {
            if !runtime_state.txn_dedupe.complete(identity, outcome) {
                warn!("applet: idempotency claim was not in-flight when completing transaction");
            }
        }
        Err(_) => {
            warn!("applet: failed to complete idempotency claim because runtime state is poisoned");
        }
    }
}

fn render_unauthorized(res: &mut Response, code: &str, message: impl Into<String>) {
    render_error(res, StatusCode::UNAUTHORIZED, code, message);
}

#[derive(Debug, Clone)]
struct VerifiedAppletHttpSignature {
    source_service_did: String,
    destination_service_did: String,
    key_id: String,
    source_key_state_digest: String,
    content_digest: Option<String>,
    covered_components: Vec<String>,
    created: i64,
    expires: i64,
    canonical_message_digest: String,
}

fn render_state_unavailable(res: &mut Response, err: &anyhow::Error) {
    warn!("cokret applet state unavailable: {err:#}");
    render_error(
        res,
        StatusCode::INTERNAL_SERVER_ERROR,
        "state_unavailable",
        "Cokret applet state unavailable",
    );
}

fn resolve_applet_for_request(
    req: &mut Request,
    res: &mut Response,
) -> Option<Arc<AppletChannelState>> {
    // 1. Per-config path param wins.
    if let Some(config_id) = req.param::<String>("config_id")
        && !config_id.is_empty()
    {
        let state = match lookup_by_config_id(&config_id) {
            Ok(state) => state,
            Err(err) => {
                render_state_unavailable(res, &err);
                return None;
            }
        };
        if let Some(state) = state {
            let Some(token) = bearer_token(req) else {
                render_unauthorized(
                    res,
                    "missing_bearer_token",
                    "Cokret applet endpoint requires Authorization: Bearer <token>",
                );
                return None;
            };
            if applet_token_matches(state.config.cokret_bearer_token.as_deref(), &token) {
                return Some(state);
            }
            render_unauthorized(
                res,
                "invalid_bearer_token",
                "Authorization token does not match this Cokret applet channel",
            );
            return None;
        }
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "applet_not_found",
            format!("no cokret applet channel configured with id '{config_id}'"),
        );
        return None;
    }
    // 2. Bearer token match.
    match lookup_by_bearer(req) {
        Ok(Some(state)) => return Some(state),
        Ok(None) => {}
        Err(err) => {
            render_state_unavailable(res, &err);
            return None;
        }
    }
    match applet_registry_is_empty() {
        Ok(true) => {
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "applet_unconfigured",
                "no cokret applet channel is currently registered",
            );
            return None;
        }
        Ok(false) => {}
        Err(err) => {
            render_state_unavailable(res, &err);
            return None;
        }
    }
    render_unauthorized(
        res,
        "invalid_bearer_token",
        "Cokret applet endpoint requires a matching Authorization bearer token",
    );
    None
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[handler]
async fn applet_ping(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let body = AppletPingOutcome {
        ok: true,
        applet_id: state.config.applet_id.clone(),
        // `service_did` is strictly validated (Did::new) in
        // `CokretAppletConfig::validate()` before the channel is registered,
        // so a registered applet always has a parseable DID here. No silent
        // `applet.unknown` fallback that would mask a config error.
        service_did: Did::new(state.config.service_did.clone())
            .expect("service_did validated at channel registration"),
        protocol_version: "1.0".to_owned(),
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_describe(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let cfg = &state.config;
    let body = AppletDescription {
        applet_id: cfg.applet_id.clone(),
        // See `applet_ping`: service_did is validated before registration.
        service_did: Did::new(cfg.service_did.clone())
            .expect("service_did validated at channel registration"),
        protocols: cfg.protocols.clone(),
        namespaces: json!({
            "actors": cfg.namespaces.actors,
            "realms": cfg.namespaces.realms,
            "handles": cfg.namespaces.handles,
        }),
        limits: json!({
            "max_events_per_transaction": 100,
            "max_body_bytes": 65_536,
            "e2ee": {
                "encrypted_content": "decrypt_when_local_group_state_exists",
                "outbound_policy": "encrypt_when_realm_requires_e2ee",
                "plaintext_fallback": "only_when_realm_policy_allows_plaintext",
                "device_id_configured": cfg.device_id.is_some(),
                "crypto_store": state.crypto_store.path().display().to_string(),
            },
        }),
        auth: json!({
            "type": "bearer",
            "controller_did": cfg.controller_did,
            "bot_actor_id": cfg.bot_actor_id,
            "bot_device_id": cfg.device_id.as_deref(),
            "http_message_signature": {
                "required_when_trusted_keys_configured": true,
                "trusted_verification_methods": cfg.trusted_verification_methods.len(),
            },
        }),
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_transactions(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let Some(idempotency_key) = req
        .header::<String>("idempotency-key")
        .filter(|v| !v.trim().is_empty())
    else {
        render_error(
            res,
            StatusCode::BAD_REQUEST,
            "missing_idempotency_key",
            "Cokret applet transactions require an Idempotency-Key header",
        );
        return;
    };
    let request_headers = collect_headers(req);
    let signature_method = req.method().as_str().to_owned();
    let signature_path = req
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let signature_authority = request_authority(req, &request_headers).ok();
    let signature_target_uri = signature_authority
        .as_deref()
        .map(|authority| request_target_uri(req, &request_headers, authority, &signature_path));

    let body_bytes = match req
        .payload_with_max_size(MAX_APPLET_TRANSACTION_BODY_BYTES)
        .await
    {
        Ok(bytes) => bytes,
        Err(salvo::http::ParseError::PayloadTooLarge) => {
            render_error(
                res,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                format!(
                    "Cokret applet transaction body exceeds {MAX_APPLET_TRANSACTION_BODY_BYTES} bytes"
                ),
            );
            return;
        }
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to read AppletTransactionRequestBody: {err}"),
            );
            return;
        }
    };

    let body: AppletTransactionRequestBody = match serde_json::from_slice(body_bytes) {
        Ok(body) => body,
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("invalid AppletTransactionRequestBody: {err}"),
            );
            return;
        }
    };

    let source_service_did = body.source_service_did.as_str().to_owned();
    if let Some(expected_source) = state.config.cokret_server_did.as_deref()
        && source_service_did != expected_source
    {
        render_unauthorized(
            res,
            "invalid_source_service_did",
            "Cokret applet transaction source_service_did does not match the trusted server DID",
        );
        return;
    }

    let verified_http_signature = match verify_applet_transaction_http_signature(
        &state,
        &signature_method,
        signature_target_uri.as_deref(),
        signature_authority.as_deref(),
        &signature_path,
        &request_headers,
        body_bytes.as_ref(),
    ) {
        Ok(verified) => verified,
        Err(err) => {
            warn!(
                config_id = %state.config.id,
                "applet: inbound HTTP message signature verification failed: {err:#}"
            );
            render_unauthorized(
                res,
                "invalid_signature",
                "Cokret applet transaction HTTP message signature verification failed",
            );
            return;
        }
    };
    if let Some(signature) = verified_http_signature.as_ref()
        && signature.source_service_did != source_service_did
    {
        render_unauthorized(
            res,
            "invalid_signature",
            "Cokret applet transaction source_service_did does not match signed source service DID",
        );
        return;
    }

    // Canonical body hash for idempotency body-equality check (spec §7.3).
    // Same `(source_service_did, key)` with matching body → return cached
    // accepted; differing body → 409 duplicate_conflict.
    let body_hash = match canonical::canonical_sha256(&body) {
        Ok(digest) => match Hash::new(digest) {
            Ok(h) => h,
            Err(err) => {
                warn!("applet: body hash construct failed: {err}");
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "hash_failed",
                    "failed to compute body canonical hash",
                );
                return;
            }
        },
        Err(err) => {
            warn!("applet: canonical hash failed: {err}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "hash_failed",
                "failed to compute body canonical hash",
            );
            return;
        }
    };

    let idempotency_key = idempotency_key.trim().to_owned();
    let identity = IdempotencyIdentity::applet_transaction(
        IdempotencyDirection::NodeToApplet,
        source_service_did.clone(),
        state.config.service_did.clone(),
        idempotency_key.clone(),
    );
    let source_signature_evidence = if let Some(signature) = verified_http_signature.as_ref() {
        json!({
            "operation_id": cokret::APPLET_TRANSACTION_OPERATION_ID,
            "direction": IdempotencyDirection::NodeToApplet.as_str(),
            "source_service_did": &source_service_did,
            "destination_service_did": &signature.destination_service_did,
            "idempotency_key": &idempotency_key,
            "auth_scheme": "http_message_signature+bearer",
            "canonical_body_digest": body_hash.clone(),
            "content_digest": &signature.content_digest,
            "covered_components": &signature.covered_components,
            "created": signature.created,
            "expires": signature.expires,
            "canonical_message_digest": &signature.canonical_message_digest,
            "source_key_state_digest": &signature.source_key_state_digest,
            "verification_method": &signature.key_id,
        })
    } else {
        json!({
            "operation_id": cokret::APPLET_TRANSACTION_OPERATION_ID,
            "direction": IdempotencyDirection::NodeToApplet.as_str(),
            "source_service_did": &source_service_did,
            "destination_service_did": state.config.service_did.clone(),
            "idempotency_key": &idempotency_key,
            "auth_scheme": "bearer",
            "content_digest": body_hash.clone(),
        })
    };
    let source_signature_anchor = match canonical::canonical_json_string(&source_signature_evidence)
    {
        Ok(anchor) => anchor,
        Err(err) => {
            warn!("applet: source signature anchor construct failed: {err}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "idempotency_anchor_failed",
                "failed to compute idempotency source signature anchor",
            );
            return;
        }
    };

    // Idempotency check (SDK S-5 IdempotencyWindow). The claim is persisted
    // before any gateway dispatch side effect runs.
    {
        let runtime_state = if let Ok(runtime_state) = state.runtime.lock() {
            runtime_state
        } else {
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_unavailable",
                "Cokret applet runtime state unavailable",
            );
            return;
        };
        runtime_state.txn_dedupe.gc();
        match runtime_state
            .txn_dedupe
            .claim(&identity, &body_hash, &source_signature_anchor)
        {
            IdempotencyClaim::Fresh => {}
            IdempotencyClaim::Duplicate { outcome, .. } => {
                debug!(
                    "applet: duplicate transaction (matching body hash/signature anchor) — returning cached outcome"
                );
                res.status_code(StatusCode::OK);
                res.render(Json(outcome));
                return;
            }
            IdempotencyClaim::DuplicateConflict { .. } => {
                warn!(
                    "applet: idempotency conflict — same identity with different body hash or source signature anchor"
                );
                render_error(
                    res,
                    StatusCode::CONFLICT,
                    "duplicate_conflict",
                    "Idempotency-Key already used for a different request body or signature anchor",
                );
                return;
            }
            IdempotencyClaim::InFlight { .. } => {
                render_error(
                    res,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "duplicate_in_flight",
                    "duplicate transaction is still being processed; retry to receive the outcome",
                );
                return;
            }
        }
    }

    // Classify events.
    let mut rejected: Vec<Value> = Vec::new();
    let mut dispatched_commands = Vec::new();
    for event in body.events.iter() {
        match classify_inbound_event(&state.config, event) {
            AppletEventOutcome::Dispatch(cmd) => dispatched_commands.push(cmd),
            AppletEventOutcome::Skip(reason) => {
                if matches!(reason, AppletDispatchSkip::EncryptedContent)
                    && let Some(cmd) = try_decrypt_applet_event(&state, event)
                {
                    dispatched_commands.push(cmd);
                    continue;
                }
                if matches!(reason, AppletDispatchSkip::EncryptedContent) {
                    warn!(
                        config_id = %state.config.id,
                        event_id = event.event_id.as_str(),
                        realm_id = event.realm_id.as_str(),
                        "cokret applet: encrypted inbound event rejected; crypto session decrypt is not wired"
                    );
                }
                rejected.push(json!({
                    "event_id": event.event_id.as_str(),
                    "reason_code": format!("{reason:?}"),
                }));
            }
        }
    }

    // Dispatch each accepted command via gateway-server runtime.
    let Ok(gateway_channel) = depot.obtain::<Arc<GatewayChannel>>() else {
        warn!("applet: gateway channel state missing from depot");
        release_transaction_claim(&state, &identity);
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_unavailable",
            "gateway channel state unavailable",
        );
        return;
    };
    let Ok(session_store) = depot.obtain::<Arc<SessionStore>>() else {
        warn!("applet: session store state missing from depot");
        release_transaction_claim(&state, &identity);
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_unavailable",
            "session store state unavailable",
        );
        return;
    };
    let gateway_channel = gateway_channel.clone();
    let session_store = session_store.clone();
    let config_id = state.config.id.clone();

    for cmd in dispatched_commands {
        let gw = gateway_channel.clone();
        let store = session_store.clone();
        let cid = config_id.clone();
        let dedupe_key = format!("cokret-applet:{}:{}", cid, cmd.event_id);
        if runtime::should_drop_duplicate(Some(dedupe_key)).await {
            continue;
        }
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gw,
                store,
                "cokret",
                cmd.realm_id.clone(),
                cmd.body,
                Some(cmd.sender_did.clone()),
                Some(runtime::StartThreadMeta {
                    peer_id: Some(cmd.sender_did),
                    group_id: Some(cmd.realm_id),
                    thread_id: cmd.thread_root_id,
                    reply_target: cmd.flow_id,
                    chat_type: Some("group".to_owned()),
                    saved_channel_config_id: Some(cid),
                    ..runtime::StartThreadMeta::default()
                }),
            )
            .await;
        });
    }

    let outcome = AppletTransactionOutcome {
        ok: true,
        rejected,
        retry_after_ms: None,
    };
    complete_transaction_claim(&state, &identity, outcome.clone());
    res.status_code(StatusCode::OK);
    res.render(Json(outcome));
}

fn verify_applet_transaction_http_signature(
    state: &AppletChannelState,
    method: &str,
    target_uri: Option<&str>,
    authority: Option<&str>,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> anyhow::Result<Option<VerifiedAppletHttpSignature>> {
    let has_signature_headers = header_value_from(headers, "signature-input").is_some()
        || header_value_from(headers, "signature").is_some();
    if state.config.trusted_verification_methods.is_empty() {
        if has_signature_headers {
            anyhow::bail!(
                "request carries HTTP Message Signature headers but no trusted verification methods are configured"
            );
        }
        return Ok(None);
    }

    let expected_source = state.config.cokret_server_did.as_deref().ok_or_else(|| {
        anyhow::anyhow!("trusted verification methods require cokret_server_did / trustedServerDid")
    })?;
    let signature_input_header = header_value_from(headers, "signature-input")
        .ok_or_else(|| anyhow::anyhow!("Signature-Input header is required"))?;
    let signature_input = parse_signature_input(&signature_input_header)
        .map_err(|err| anyhow::anyhow!("parse Signature-Input: {err}"))?;
    let trusted_method = state
        .config
        .trusted_verification_methods
        .iter()
        .find(|method| method.verification_method == signature_input.key_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "verification method '{}' is not trusted for applet '{}'",
                signature_input.key_id,
                state.config.id
            )
        })?;
    let signer_did = verification_method_did(&signature_input.key_id)
        .ok_or_else(|| anyhow::anyhow!("HTTP signature keyid has no DID fragment"))?;
    if signer_did != expected_source {
        anyhow::bail!(
            "HTTP signature keyid owner '{signer_did}' does not match trusted server DID '{expected_source}'"
        );
    }

    let source = header_value_from(headers, SOURCE_SERVICE_DID_HEADER)
        .ok_or_else(|| anyhow::anyhow!("{SOURCE_SERVICE_DID_HEADER} header is required"))?;
    if source != expected_source {
        anyhow::bail!(
            "HTTP signature source service DID '{source}' does not match trusted server DID '{expected_source}'"
        );
    }
    let destination = header_value_from(headers, DESTINATION_SERVICE_DID_HEADER)
        .ok_or_else(|| anyhow::anyhow!("{DESTINATION_SERVICE_DID_HEADER} header is required"))?;
    if destination != state.config.service_did {
        anyhow::bail!(
            "HTTP signature destination service DID '{destination}' does not match applet service DID '{}'",
            state.config.service_did
        );
    }

    let public_key_bytes = trusted_method
        .public_key
        .ed25519_bytes()
        .map_err(|err| anyhow::anyhow!("trusted HTTP signature public key: {err}"))?;
    let source_key_state_digest = canonical::canonical_digest(&public_key_bytes);
    let public_key = public_key_from_bytes(&public_key_bytes)
        .map_err(|err| anyhow::anyhow!("trusted HTTP signature public key: {err}"))?;
    let authority =
        authority.ok_or_else(|| anyhow::anyhow!("request authority/Host is required"))?;
    let target_uri =
        target_uri.ok_or_else(|| anyhow::anyhow!("request target URI could not be constructed"))?;
    let required_components = vec![
        Component::Method,
        Component::TargetUri,
        Component::Authority,
        Component::Header(SOURCE_SERVICE_DID_HEADER.to_owned()),
        Component::Header(DESTINATION_SERVICE_DID_HEADER.to_owned()),
        Component::Header("content-digest".to_owned()),
        Component::Header("idempotency-key".to_owned()),
    ];
    let policy = SignatureVerificationPolicy::new(required_components)
        .require_content_digest(true)
        .max_clock_skew_seconds(APPLET_TRANSACTION_SIGNATURE_MAX_CLOCK_SKEW_SECS)
        .max_validity_window_seconds(APPLET_TRANSACTION_SIGNATURE_MAX_LIFETIME_SECS);
    let verified = verify_signed_http_message(
        method,
        target_uri,
        authority,
        path,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        body,
        &public_key,
        &policy,
        chrono::Utc::now().timestamp(),
    )
    .map_err(map_http_signature_error)?;
    let content_digest = verified
        .content_digest
        .as_ref()
        .map(|digest| digest.wire_value.clone());
    let covered_components = verified
        .signature_input
        .covered_components
        .iter()
        .map(Component::canonical_name)
        .collect();
    let canonical_message_digest = canonical::canonical_digest(&verified.canonical_message);
    Ok(Some(VerifiedAppletHttpSignature {
        source_service_did: source,
        destination_service_did: destination,
        key_id: verified.signature_input.key_id,
        source_key_state_digest,
        content_digest,
        covered_components,
        created: verified.signature_input.created,
        expires: verified.signature_input.expires,
        canonical_message_digest,
    }))
}

fn collect_headers(req: &Request) -> Vec<(String, String)> {
    req.headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect()
}

fn header_value_from(headers: &[(String, String)], name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let values: Vec<&str> = headers
        .iter()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(&lower))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

fn request_authority(req: &Request, headers: &[(String, String)]) -> anyhow::Result<String> {
    header_value_from(headers, "host")
        .or_else(|| {
            req.uri()
                .authority()
                .map(|authority| authority.as_str().to_owned())
        })
        .ok_or_else(|| anyhow::anyhow!("request authority/Host is required"))
}

fn request_target_uri(
    req: &Request,
    headers: &[(String, String)],
    authority: &str,
    path: &str,
) -> String {
    let uri = req.uri();
    if uri.scheme().is_some() && uri.authority().is_some() {
        return uri.to_string();
    }
    let scheme = header_value_from(headers, "x-forwarded-proto")
        .and_then(|value| value.split(',').next().map(str::trim).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http".to_owned());
    format!("{scheme}://{authority}{path}")
}

fn verification_method_did(verification_method: &str) -> Option<&str> {
    verification_method
        .rsplit_once('#')
        .map(|(did, _)| did)
        .filter(|did| !did.is_empty())
}

fn map_http_signature_error(err: HttpMessageVerificationError) -> anyhow::Error {
    match err {
        HttpMessageVerificationError::MissingHeader(header) => {
            anyhow::anyhow!("required HTTP signature header '{header}' is missing")
        }
        HttpMessageVerificationError::Policy(
            SignaturePolicyError::MissingContentDigest
            | SignaturePolicyError::MissingRequiredCoveredComponent,
        ) => anyhow::anyhow!("HTTP signature does not cover required applet transaction fields"),
        HttpMessageVerificationError::Policy(SignaturePolicyError::InvalidValidityWindow) => {
            anyhow::anyhow!("HTTP signature validity window is invalid")
        }
        HttpMessageVerificationError::Policy(
            SignaturePolicyError::CreatedInFuture | SignaturePolicyError::Expired,
        ) => anyhow::anyhow!("HTTP signature timestamp is outside the accepted window"),
        HttpMessageVerificationError::Signature(err) => anyhow::anyhow!("{err}"),
    }
}

fn try_decrypt_applet_event(
    state: &AppletChannelState,
    event: &cokret::Event,
) -> Option<AppletInboundCommand> {
    let payload = extract_encrypted_payload_from_message_content(&event.content)?;
    if let Some(device_id) = state.config.device_id.as_deref() {
        match state.crypto_store.plan_bootstrap_for_payload(
            &state.config.bot_actor_id,
            device_id,
            &payload,
        ) {
            Ok(plan) => debug!(
                config_id = %state.config.id,
                group_id = %plan.group_id,
                required_epoch = plan.required_epoch,
                local_epoch = ?plan.local_epoch,
                action = ?plan.action,
                "cokret applet: planned crypto bootstrap for encrypted event"
            ),
            Err(err) => warn!(
                config_id = %state.config.id,
                "cokret applet: failed to plan crypto bootstrap for encrypted event: {err:#}"
            ),
        }
    }

    match state.crypto_store.try_decrypt_content_block(&payload) {
        Ok(CokretDecryptOutcome::Decrypted(content)) => {
            let Some(body) = decrypted_text_body(&content) else {
                warn!(
                    config_id = %state.config.id,
                    event_id = event.event_id.as_str(),
                    "cokret applet: decrypted encrypted event but content is not displayable text"
                );
                return None;
            };
            Some(AppletInboundCommand {
                event_id: event.event_id.as_str().to_owned(),
                realm_id: event.realm_id.as_str().to_owned(),
                flow_id: event
                    .content
                    .get("flow_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                sender_did: event.actor_id.as_str().to_owned(),
                body,
                thread_root_id: event
                    .content
                    .get("thread_root_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        Ok(CokretDecryptOutcome::MissingGroupState) => {
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                cokret::crypto_protocol::UnableToDecryptReason::NoSession,
            );
            None
        }
        Ok(CokretDecryptOutcome::UnsupportedScheme(scheme)) => {
            warn!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                scheme,
                "cokret applet: unsupported encrypted payload scheme"
            );
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                cokret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            None
        }
        Err(err) => {
            warn!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                "cokret applet: encrypted event decrypt failed: {err:#}"
            );
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                cokret::crypto_protocol::UnableToDecryptReason::BadCiphertext,
            );
            None
        }
    }
}

fn record_applet_unable_to_decrypt(
    state: &AppletChannelState,
    event: &cokret::Event,
    payload: cokret::EncryptedPayload,
    reason: cokret::crypto_protocol::UnableToDecryptReason,
) {
    if let Err(err) = state.crypto_store.record_unable_to_decrypt(
        event.event_id.as_str(),
        event.realm_id.as_str(),
        event.actor_id.as_str(),
        payload,
        reason,
    ) {
        warn!(
            config_id = %state.config.id,
            event_id = event.event_id.as_str(),
            "cokret applet: failed to persist unable-to-decrypt record: {err:#}"
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

#[handler]
async fn applet_actor(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let actor_id = req.param::<String>("actor_id").unwrap_or_default();
    if !state.config.namespaces.actor_matches(&actor_id) {
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "actor_not_in_namespace",
            format!("actor '{actor_id}' is not in this applet's namespace"),
        );
        return;
    }
    let body = AppletActorView {
        exists: true,
        actor_id: Did::new(actor_id).ok(),
        display_name: None,
        external_ref: Value::Null,
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_realm(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let realm = req.param::<String>("realm_id_or_alias").unwrap_or_default();
    if !state.config.namespaces.realm_matches(&realm) {
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "realm_not_in_namespace",
            format!("realm '{realm}' is not in this applet's namespace"),
        );
        return;
    }
    let body = AppletRealmView {
        exists: true,
        realm_id: cokret::RealmId::new(realm).ok(),
        title: None,
        external_ref: Value::Null,
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_protocol(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let protocol = req.param::<String>("protocol").unwrap_or_default();
    if !state.config.protocols.iter().any(|p| p == &protocol) {
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "protocol_not_supported",
            format!("protocol '{protocol}' is not registered with this applet"),
        );
        return;
    }
    let body = AppletProtocolMetadata {
        protocol: protocol.clone(),
        display_name: protocol,
        icon_blob_ref: None,
        field_types: json!({}),
        instances: vec![],
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

fn third_party_query_fields(req: &Request) -> Map<String, Value> {
    let mut fields = Map::new();
    for (key, value) in req.queries().flat_iter() {
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        match fields.get_mut(key) {
            Some(Value::Array(values)) => values.push(Value::String(value.to_owned())),
            Some(existing) => {
                let first = std::mem::take(existing);
                *existing = Value::Array(vec![first, Value::String(value.to_owned())]);
            }
            None => {
                fields.insert(key.to_owned(), Value::String(value.to_owned()));
            }
        }
    }
    fields
}

fn field_string<'a>(fields: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| match fields.get(*key) {
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        Some(Value::Array(values)) => values.iter().find_map(|value| match value {
            Value::String(value) => {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then_some(trimmed)
            }
            _ => None,
        }),
        _ => None,
    })
}

fn ensure_supported_protocol(
    cfg: &CokretAppletConfig,
    fields: &Map<String, Value>,
    res: &mut Response,
) -> Option<String> {
    let Some(protocol) = field_string(fields, &["protocol"]) else {
        render_error(
            res,
            StatusCode::BAD_REQUEST,
            "missing_protocol",
            "third_party lookup requires a protocol query parameter",
        );
        return None;
    };
    if !cfg.protocols.iter().any(|value| value == protocol) {
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "protocol_not_supported",
            format!("protocol '{protocol}' is not registered with this applet"),
        );
        return None;
    }
    Some(protocol.to_owned())
}

fn third_party_external_ref(protocol: &str, fields: Map<String, Value>) -> Value {
    let mut external_ref = fields;
    external_ref
        .entry("protocol".to_owned())
        .or_insert_with(|| Value::String(protocol.to_owned()));
    Value::Object(external_ref)
}

fn location_candidates(protocol: &str, fields: &Map<String, Value>) -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["realm_id", "space_id"] {
        if let Some(value) = field_string(fields, &[key]) {
            candidates.push(value.to_owned());
        }
    }

    let team = field_string(fields, &["team", "team_id", "workspace", "workspace_id"]);
    let channel = field_string(fields, &["channel", "channel_id", "room", "room_id"]);
    if let (Some(team), Some(channel)) = (team, channel) {
        candidates.push(format!("{protocol}:team:{team}:channel:{channel}"));
    }
    if let Some(channel) = channel {
        candidates.push(format!("{protocol}:channel:{channel}"));
    }
    if let Some(location) = field_string(
        fields,
        &[
            "location",
            "location_id",
            "external_id",
            "id",
            "conversation",
            "conversation_id",
        ],
    ) {
        candidates.push(format!("{protocol}:location:{location}"));
    }
    candidates
}

#[handler]
async fn applet_third_party_users(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let fields = third_party_query_fields(req);
    let Some(protocol) = ensure_supported_protocol(&state.config, &fields, res) else {
        return;
    };
    let external_ref = third_party_external_ref(&protocol, fields.clone());
    let actor_id = field_string(
        &fields,
        &["actor_id", "user", "user_id", "external_id", "id", "actor"],
    )
    .map(|external_id| {
        savfox_channels::cokret::mint_ghost_did(
            &state.config.service_did,
            &state.config.ghost_did_prefix,
            external_id,
        )
    })
    .filter(|actor_id| state.config.namespaces.actor_matches(actor_id));
    let exists = actor_id.is_some();

    res.status_code(StatusCode::OK);
    res.render(Json(json!({
        "actor_id": actor_id,
        "exists": exists,
        "external_ref": external_ref,
    })));
}

#[handler]
async fn applet_third_party_locations(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_applet_for_request(req, res) else {
        return;
    };
    let fields = third_party_query_fields(req);
    let Some(protocol) = ensure_supported_protocol(&state.config, &fields, res) else {
        return;
    };
    let external_ref = third_party_external_ref(&protocol, fields.clone());
    let realm_id = location_candidates(&protocol, &fields)
        .into_iter()
        .find(|candidate| state.config.namespaces.realm_matches(candidate));
    let exists = realm_id.is_some();

    res.status_code(StatusCode::OK);
    res.render(Json(json!({
        "realm_id": &realm_id,
        "space_id": &realm_id,
        "exists": exists,
        "external_ref": external_ref,
    })));
}

// ─── Outbound + bridge_error (Phase 7 T7.C2/T7.D1) ──────────────────────────

/// Try to send an agent reply through a registered applet whose realm
/// namespace covers `realm_id`.
///
/// Returns `Ok(false)` when no registered applet claims the realm so callers
/// can fall back to account-mode Cokret sending.
pub(crate) async fn send_to_cokret_applet_for_realm(
    realm_id: &str,
    flow_id: Option<&str>,
    body: &str,
) -> anyhow::Result<bool> {
    let Some(state) = lookup_by_realm(realm_id)? else {
        return Ok(false);
    };
    let flow_id = flow_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret applet '{}' cannot reply to realm '{}' without a flow id",
            state.config.id,
            realm_id
        )
    })?;
    let external_ref = json!({
        "protocol": "savfox",
        "network_id": state.config.id,
        "external_id": format!("{realm_id}:{flow_id}"),
        "kind": "agent_reply",
    });
    // `actor_seq` is no longer derived from a wall-clock timestamp here — it
    // is sourced inside `send_via_applet` from the per-applet
    // `cokret-bridge-runtime` `SeqAllocator` (monotonic, restart-safe).
    send_via_applet(
        &state.config.id,
        realm_id,
        flow_id,
        &state.config.bot_actor_id,
        body,
        external_ref,
    )
    .await?;
    Ok(true)
}

/// Send a Ghost-actor-attributed `ck.message.create` Event from this applet
/// to the Cokret server. On failure, emit a best-effort
/// `ck.applet.bridge_error` Event so receivers don't silently lose state
/// (spec applet-integration.md §14).
///
/// `config_id` looks up the registered applet; `external_ref` is the
/// bridge-side origin (protocol/network/external_id) for audit.
///
/// The outbound `actor_seq` is allocated from the applet's
/// `cokret-bridge-runtime` [`SeqAllocator`] (file-backed [`SeqStore`]),
/// giving a monotonic, restart-safe sequence — no longer
/// `chrono::Utc::now().timestamp_millis()`.
pub(crate) async fn send_via_applet(
    config_id: &str,
    realm_id: &str,
    flow_id: &str,
    ghost_actor_did: &str,
    body: &str,
    external_ref: Value,
) -> anyhow::Result<()> {
    let state = lookup_by_config_id(config_id)
        .with_context(|| format!("cokret applet '{config_id}' registry lookup failed"))?
        .ok_or_else(|| anyhow::anyhow!("cokret applet '{config_id}' not registered"))?;
    let cfg = &state.config;

    // Monotonic restart-safe actor sequence from the per-applet allocator.
    let actor_seq = state
        .seq
        .alloc()
        .map_err(|e| anyhow::anyhow!("seq alloc: {e}"))?;

    // Phase 8: prefer login_did_proof when key_ref is set; otherwise fall
    // back to static bearer for Phase 6/7-style configs.
    let http = construct_applet_client(cfg).await?;

    // Phase 8 (T8.E): if a grant is configured, attach its event_id as
    // authorization_ref on the outbound event.
    let authorization_ref = load_applet_grant_event_id(cfg)
        .await
        .or_else(|| cfg.authorization_grant_id.clone());

    let req = savfox_channels::cokret::AppletMessageRequest {
        applet_id: cfg.applet_id.clone(),
        realm_id: realm_id.to_owned(),
        flow_id: flow_id.to_owned(),
        ghost_actor_did: ghost_actor_did.to_owned(),
        body: body.to_owned(),
        external_ref: external_ref.clone(),
        authorization_ref,
        executed_by: None,
        actor_seq,
        thread_root_id: None,
    };
    let mut event = savfox_channels::cokret::build_applet_message_event(&req)?;
    apply_applet_outbound_encryption(&state.crypto_store, realm_id, &mut event)?;

    // Phase 8 (T8.C): sign with the applet's bot key when key_ref is set.
    sign_applet_event_if_keyed(cfg, ghost_actor_did, &mut event).await?;

    // A transport 200 is not delivery confirmation: inspect the business-level
    // result and treat a rejection (or zero accepted/duplicate events) as a
    // failure so it flows through the same bridge_error path as a transport
    // error (spec §14), instead of being silently dropped.
    let submit_result = match http.submit_event(&event).await {
        Ok(resp) => {
            if !resp.rejected.is_empty() {
                Err(anyhow::anyhow!(
                    "cokret applet: server rejected event for realm '{realm_id}': {:?}",
                    resp.rejected
                ))
            } else if resp.accepted.is_empty() && resp.duplicate.is_empty() {
                Err(anyhow::anyhow!(
                    "cokret applet: server accepted no events for realm '{realm_id}' (status={:?})",
                    resp.status
                ))
            } else {
                Ok(())
            }
        }
        Err(err) => Err(err),
    };
    match submit_result {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!(
                config_id,
                realm_id,
                flow_id,
                error = %err,
                "cokret applet: send_via_applet submission failed — emitting bridge_error"
            );
            // Best-effort bridge_error emission. If THIS submit also fails,
            // we log and continue — there is no escalation path for a
            // double failure in Phase 7.
            if let Err(err2) = emit_bridge_error(
                &state,
                realm_id,
                "cokret_submit_failed",
                &err.to_string(),
                Some(external_ref),
            )
            .await
            {
                warn!(config_id, error = %err2, "cokret applet: bridge_error emit also failed");
            }
            Err(err)
        }
    }
}

fn apply_applet_outbound_encryption(
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
                .ok_or_else(|| anyhow::anyhow!("Cokret applet message content is not an object"))?;
            object.remove("content");
            object.insert("encrypted_content".to_owned(), encrypted_content);
            Ok(())
        }
        CokretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
            anyhow::bail!(
                "Cokret realm '{realm_id}' requires E2EE but no local applet MLS group state exists for group '{group_id}'"
            );
        }
    }
}

/// Emit a `ck.applet.bridge_error` Event (SDK S-11).
///
/// `code` should be one of the spec-blessed strings:
/// `external_rate_limited` / `external_rejected` / `cokret_submit_failed` /
/// `delivery_unconfirmed`. `message` is human-readable.
async fn emit_bridge_error(
    state: &Arc<AppletChannelState>,
    realm_id: &str,
    code: &str,
    message: &str,
    external_ref: Option<Value>,
) -> anyhow::Result<()> {
    use cokret::{
        AppletBridgeErrorBuilder, AppletBridgeErrorClass, AppletBridgeErrorVisibility, Hlc, RealmId,
    };

    let cfg = &state.config;
    let realm = RealmId::new(realm_id.to_owned())
        .with_context(|| format!("invalid realm_id: {realm_id}"))?;
    let actor = Did::new(cfg.bot_actor_id.clone())
        .with_context(|| format!("invalid bot DID: {}", cfg.bot_actor_id))?;
    // Monotonic restart-safe actor sequence from the per-applet allocator —
    // bridge_error events get a real sequence instead of the previous
    // hard-coded `0`. The HLC origin still uses wall-clock millis (there is
    // no shared HLC source available to the applet host yet).
    let actor_seq = state
        .seq
        .alloc()
        .map_err(|e| anyhow::anyhow!("seq alloc: {e}"))?;
    let unix_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let hlc = Hlc::new(format!("{unix_ms:012x}-0000-00000000"))
        .map_err(|err| anyhow::anyhow!("hlc: {err}"))?;

    // SDK S-13 reshaped the payload: `severity` → `error_class` /
    // `error_code` / `retriable` / `visibility_scope` /
    // `failed_transaction_ref`. An outbound submit failure has no inbound
    // transaction id, so anchor on the target realm (MUST NOT inline external
    // plaintext). A hard upstream rejection is terminal; other classes are
    // retriable.
    let failed_transaction_ref = format!("outbound:{realm_id}");
    let retriable = !matches!(code, "external_rejected");
    let mut builder = AppletBridgeErrorBuilder::new(
        realm,
        cfg.applet_id.clone(),
        actor,
        failed_transaction_ref,
        AppletBridgeErrorClass::ExternalNetwork,
        code,
        retriable,
        AppletBridgeErrorVisibility::RealmAdmins,
    )
    .with_message(message);
    if let Some(ext) = external_ref {
        builder = builder.with_external_ref(ext);
    }
    let mut event = builder
        .build(actor_seq, hlc)
        .map_err(|err| anyhow::anyhow!("bridge_error build: {err}"))?;

    // Phase 8: sign bridge_error too if a signer is configured.
    sign_applet_event_if_keyed(cfg, &cfg.bot_actor_id, &mut event).await?;
    let http = construct_applet_client(cfg).await?;
    http.submit_event(&event).await?;
    Ok(())
}

/// Build the outbound HTTP client for an applet config. Uses DID-proof
/// login when `key_ref` is set; falls back to the static bearer otherwise.
async fn construct_applet_client(
    cfg: &savfox_channels::cokret::CokretAppletConfig,
) -> anyhow::Result<savfox_channels::cokret::CokretHttpClient> {
    if let Some(key_ref) = &cfg.key_ref {
        use savfox_channels::cokret::CokretHttpClient;
        let vm = cfg
            .verification_method
            .clone()
            .unwrap_or_else(|| format!("{}#key-1", cfg.bot_actor_id));
        let audience = cfg.cokret_server_did.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "applet '{}' has key_ref but no cokret_server_did / cokretServerDid for DID-proof audience",
                cfg.id
            )
        })?;
        let challenge = cfg.login_challenge.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "applet '{}' has key_ref but no login_challenge / loginChallenge",
                cfg.id
            )
        })?;
        let signer = savfox_channels::cokret::load_ed25519_signer(key_ref, &cfg.bot_actor_id, &vm)?;
        let principal = cokret::Did::new(cfg.bot_actor_id.clone())
            .map_err(|err| anyhow::anyhow!("invalid bot DID: {err}"))?;
        // Applet configs keep the bot device optional for bearer-only mode.
        // DID-proof login still needs a protocol-valid runtime device id.
        let device = cokret::DeviceId::new(cfg.device_id.clone().unwrap_or_else(|| {
            savfox_channels::cokret::derive_cokret_device_id(&[
                "applet",
                &cfg.id,
                &cfg.applet_id,
                &cfg.bot_actor_id,
            ])
        }))
        .map_err(|err| anyhow::anyhow!("synth device_id: {err}"))?;
        let (client, _session) = CokretHttpClient::login(
            &cfg.cokret_server_url,
            &signer,
            principal,
            device,
            challenge,
            audience,
        )
        .await?;
        Ok(client)
    } else {
        let bearer = cfg.cokret_bearer_token.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "applet '{}' has neither key_ref nor cokret_bearer_token",
                cfg.id
            )
        })?;
        savfox_channels::cokret::CokretHttpClient::new(&cfg.cokret_server_url, bearer)
    }
}

/// Phase 8 (T8.C): sign the outbound event with the applet's bot key,
/// if `key_ref` is set. No-op otherwise.
async fn sign_applet_event_if_keyed(
    cfg: &savfox_channels::cokret::CokretAppletConfig,
    actor_did: &str,
    event: &mut cokret::Event,
) -> anyhow::Result<()> {
    let Some(key_ref) = &cfg.key_ref else {
        // No signer configured: the event goes out with empty `proofs[]`, which
        // spec-compliant production servers reject with `event_proofs_empty`.
        // Don't fail (bare-bearer dev deployments rely on this), but make the
        // misconfiguration visible instead of silently submitting a doomed event.
        warn!(
            "cokret applet '{}': no key_ref configured — submitting UNSIGNED outbound event \
             (production servers will reject with event_proofs_empty)",
            cfg.id
        );
        return Ok(());
    };
    let vm = cfg
        .verification_method
        .clone()
        .unwrap_or_else(|| format!("{actor_did}#key-1"));
    let signer = savfox_channels::cokret::load_ed25519_signer(key_ref, actor_did, &vm)?;
    savfox_channels::cokret::applet::sign_outbound_event(event, &signer, &vm)?;
    Ok(())
}

/// Phase 8 (T8.E): if `grant_event_path` is set, load + verify the
/// capability grant and return its `event_id` for use as
/// `authorization_ref`. Logs and returns `None` on load failure (grant
/// is operator-managed; missing files shouldn't crash outbound).
async fn load_applet_grant_event_id(
    cfg: &savfox_channels::cokret::CokretAppletConfig,
) -> Option<String> {
    let path = cfg.grant_event_path.as_ref()?;
    match savfox_channels::cokret::load_and_verify_grant(path, &cfg.bot_actor_id, None).await {
        Ok(grant) if grant.covers_action("ck.message.create") => Some(grant.event_id),
        Ok(_) => {
            warn!(
                "cokret applet '{}': capability grant at {} does not cover ck.message.create",
                cfg.id,
                path.display()
            );
            None
        }
        Err(err) => {
            warn!(
                "cokret applet '{}': capability grant load failed at {}: {err:#}",
                cfg.id,
                path.display()
            );
            None
        }
    }
}

// ─── Router ─────────────────────────────────────────────────────────────────

/// Routes mounted at `/_cokret/edge/applet/...` (direct).
pub(crate) fn cokret_applet_router() -> Router {
    Router::with_path("_cokret/edge/applet")
        .push(Router::with_path("ping").get(applet_ping))
        .push(Router::with_path("describe").get(applet_describe))
        .push(Router::with_path("transactions").post(applet_transactions))
        .push(Router::with_path("actors/{actor_id}").get(applet_actor))
        .push(Router::with_path("realms/{realm_id_or_alias}").get(applet_realm))
        .push(Router::with_path("protocols/{protocol}").get(applet_protocol))
        .push(Router::with_path("third_party/users").get(applet_third_party_users))
        .push(Router::with_path("third_party/locations").get(applet_third_party_locations))
}

/// Routes mounted at `/appservices/cokret/{config_id}/_cokret/edge/applet/...`.
pub(crate) fn cokret_appservices_router() -> Router {
    Router::with_path("appservices/cokret/{config_id}").push(cokret_applet_router())
}

// ─── Startup glue ───────────────────────────────────────────────────────────

/// Build the restart-safe monotonic [`SeqAllocator`] for an applet.
///
/// The backing [`SeqStore`] is a file under
/// `{savfox_home}/gateway/cokret-applet-seq/{config_id}.seq`; the allocator
/// is keyed `applet:{config_id}:actor_seq` so each applet has an independent
/// monotonic counter. Persisting the high-water mark makes `actor_seq`
/// restart-safe — the previous `timestamp_millis()` approach was neither
/// monotonic across rapid calls nor durable across restarts.
fn build_applet_seq_allocator(
    savfox_home: &std::path::Path,
    config_id: &str,
) -> anyhow::Result<cokret_bridge_runtime::SeqAllocator> {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("cokret-applet-seq");
    // Sanitize the config id for use as a filename (ids are operator-defined
    // and may contain path separators / colons).
    let safe_id: String = config_id
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
        .map_err(|e| anyhow::anyhow!("cokret applet seq store: {e}"))?;
    Ok(cokret_bridge_runtime::SeqAllocator::new(
        store,
        format!("applet:{config_id}:actor_seq"),
    ))
}

/// Start (register) a Cokret Applet channel. Mounts no extra HTTP listener
/// — the routes are added to the main savfox-gateway-server `Router` in
/// `server.rs`. Returns once registry insertion is done.
pub(crate) async fn start_cokret_applet_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    _session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let applet_cfg = CokretAppletConfig::from_channel_config(config).ok_or_else(|| {
        anyhow::anyhow!("Cokret applet channel '{}' missing or invalid", config.id)
    })?;
    applet_cfg.validate().with_context(|| {
        format!(
            "Cokret applet channel '{}' validation failed",
            applet_cfg.id
        )
    })?;

    // Restart-safe monotonic `actor_seq` source for outbound events. The
    // allocator persists its high-water mark in a per-applet file under the
    // savfox home dir, so sequence numbers never regress across restarts
    // (replacing the old `timestamp_millis()` hack). `SeqAllocator` is keyed
    // per-applet (`config.id`) so multiple applets don't share a counter.
    let savfox_home = channel.config().savfox_home.clone();
    let crypto_store = FileCokretCryptoStore::for_applet(&savfox_home, &applet_cfg.id);
    if let Err(err) =
        FileCokretCryptoStore::feature_report().and_then(|_| crypto_store.ensure_created())
    {
        warn!(
            "cokret: applet '{}' crypto state unavailable at {}: {err:#}",
            applet_cfg.id,
            crypto_store.path().display()
        );
    }
    let seq = build_applet_seq_allocator(&savfox_home, &applet_cfg.id)?;

    let state = AppletChannelState {
        config: applet_cfg,
        runtime: Mutex::new(AppletRuntimeState::default()),
        crypto_store,
        seq,
    };
    info!(
        "cokret: applet channel '{}' registered (applet_id={}, service_did={})",
        state.config.id, state.config.applet_id, state.config.service_did
    );
    register_channel(state)?;
    Ok(())
}

/// Loader used at gateway startup to count + log configured applet channels
/// without booting them (booting is `start_cokret_applet_channel`).
pub(crate) async fn log_cokret_applet_configs(savfox_home: &std::path::PathBuf) {
    match load_cokret_applet_configs(savfox_home).await {
        Ok(configs) => {
            for cfg in configs {
                info!(
                    "cokret applet config '{}': applet_id={}, service_did={}, protocols={:?}",
                    cfg.id, cfg.applet_id, cfg.service_did, cfg.protocols,
                );
            }
        }
        Err(err) => {
            warn!("cokret applet: failed to load configs: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use cokret::http_signature::{
        Component, ContentDigest, ContentDigestAlgorithm, SignedRequestParts, canonical_message,
        parse_signature_input, sign_message, signing_key_from_seed,
    };
    use cokret::signatures::PublicKeyMaterial;
    use savfox_channels::cokret::applet::CokretAppletTrustedVerificationMethod;
    use savfox_core::config::channel_store::ChannelConfig;

    use super::*;

    fn valid_channel_config() -> ChannelConfig {
        ChannelConfig {
            id: "applet-test".into(),
            kind: "cokret".into(),
            slug: "applet".into(),
            name: "Applet".into(),
            enabled: true,
            config: json!({
                "mode": "applet",
                "appletId": "ck:applet:21532600-0000-7000-8000-000000000000",
                "serviceDid": "did:web:bridge.example",
                "controllerDid": "did:webvh:example.com:admin",
                "baseUrl": "https://savfox.example/applet-test",
                "botActorId": "did:web:bridge.example:bot",
                "cokretServerUrl": "https://cokret.example.org",
                "cokretServerDid": "did:webvh:cokret.example.org",
                "accessToken": "test-bearer",
                "protocols": ["slack"],
                "namespaces": {
                    "actors": [{"pattern": "did:web:bridge.example:ghost:*", "exclusive": true}],
                    "realms": [{"pattern": "ck:realm:*", "exclusive": true}],
                    "handles": []
                }
            }),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn state_with_trusted_http_signature_key(public_key: Vec<u8>) -> AppletChannelState {
        let cfg = valid_channel_config();
        let mut applet = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.trusted_verification_methods = vec![CokretAppletTrustedVerificationMethod {
            verification_method: "did:webvh:cokret.example.org#key-1".to_owned(),
            public_key: PublicKeyMaterial::Ed25519Raw { bytes: public_key },
        }];
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            crypto_store: FileCokretCryptoStore::for_applet(tmp.path(), &applet.id),
            seq: build_applet_seq_allocator(tmp.path(), &applet.id).expect("seq allocator"),
        }
    }

    fn signed_transaction_headers(body: &[u8], seed: [u8; 32]) -> (Vec<(String, String)>, Vec<u8>) {
        let signing_key = signing_key_from_seed(&seed);
        let public_key = signing_key.verifying_key().to_bytes().to_vec();
        let now = chrono::Utc::now().timestamp();
        let content_digest = ContentDigest::compute(body, ContentDigestAlgorithm::Sha256);
        let signature_input = format!(
            "sig1=(\"@method\" \"@target-uri\" \"@authority\" \
             \"source-service-did\" \"destination-service-did\" \
             \"content-digest\" \"idempotency-key\");created={now};expires={};\
             keyid=\"did:webvh:cokret.example.org#key-1\";alg=\"ed25519\"",
            now + 300
        );
        let mut headers = vec![
            ("host".to_owned(), "savfox.example".to_owned()),
            (
                SOURCE_SERVICE_DID_HEADER.to_owned(),
                "did:webvh:cokret.example.org".to_owned(),
            ),
            (
                DESTINATION_SERVICE_DID_HEADER.to_owned(),
                "did:web:bridge.example".to_owned(),
            ),
            (
                "content-digest".to_owned(),
                content_digest.wire_value.clone(),
            ),
            ("idempotency-key".to_owned(), "txn-1".to_owned()),
            ("signature-input".to_owned(), signature_input.clone()),
        ];
        let parsed = parse_signature_input(&signature_input).expect("signature input should parse");
        assert!(parsed.covers_all(&[
            Component::Method,
            Component::TargetUri,
            Component::Authority,
            Component::Header(SOURCE_SERVICE_DID_HEADER.to_owned()),
            Component::Header(DESTINATION_SERVICE_DID_HEADER.to_owned()),
            Component::Header("content-digest".to_owned()),
            Component::Header("idempotency-key".to_owned()),
        ]));
        let request = SignedRequestParts {
            method: "POST".to_owned(),
            target_uri: "https://savfox.example/_cokret/edge/applet/transactions".to_owned(),
            authority: "savfox.example".to_owned(),
            path: "/_cokret/edge/applet/transactions".to_owned(),
            headers: headers.clone(),
            body_digest: Some(content_digest.wire_value),
        };
        let message = canonical_message(&request, &parsed).expect("canonical message");
        let signature = sign_message(&message, &signing_key);
        headers.push(("signature".to_owned(), format!("sig1=:{signature}:")));
        (headers, public_key)
    }

    #[tokio::test]
    async fn start_registers_applet_into_registry() {
        let cfg = valid_channel_config();
        // We can't easily build a full GatewayChannel/SessionStore in unit
        // scope — but `start_cokret_applet_channel` only uses them for
        // logging context and accepts &Arc<...>. We use placeholder Arcs.
        // Actually it doesn't use them at all in Phase 6, so dummies are fine.
        // We bypass by calling internals directly:
        let applet = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            crypto_store: FileCokretCryptoStore::for_applet(tmp.path(), &applet.id),
            seq: build_applet_seq_allocator(tmp.path(), &applet.id).expect("seq allocator"),
        };
        register_channel(state).expect("register");
        let resolved = lookup_by_config_id(&applet.id)
            .expect("lookup")
            .expect("registered");
        assert_eq!(resolved.config.applet_id, applet.applet_id);
    }

    #[test]
    fn verifies_trusted_http_message_signature() {
        let body = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_did": "did:webvh:cokret.example.org",
            "events": []
        }))
        .expect("body should serialize");
        let (headers, public_key) = signed_transaction_headers(&body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        let verified = verify_applet_transaction_http_signature(
            &state,
            "POST",
            Some("https://savfox.example/_cokret/edge/applet/transactions"),
            Some("savfox.example"),
            "/_cokret/edge/applet/transactions",
            &headers,
            &body,
        )
        .expect("signature should verify")
        .expect("signature should be required");
        assert_eq!(verified.source_service_did, "did:webvh:cokret.example.org");
        assert_eq!(verified.destination_service_did, "did:web:bridge.example");
        assert_eq!(verified.key_id, "did:webvh:cokret.example.org#key-1");
        assert!(verified.content_digest.is_some());
    }

    #[test]
    fn rejects_tampered_http_message_signature_body() {
        let body = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_did": "did:webvh:cokret.example.org",
            "events": []
        }))
        .expect("body should serialize");
        let (headers, public_key) = signed_transaction_headers(&body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        let tampered = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_did": "did:webvh:cokret.example.org",
            "events": [{"kind":"ck.message.create"}]
        }))
        .expect("tampered body should serialize");
        let err = verify_applet_transaction_http_signature(
            &state,
            "POST",
            Some("https://savfox.example/_cokret/edge/applet/transactions"),
            Some("savfox.example"),
            "/_cokret/edge/applet/transactions",
            &headers,
            &tampered,
        )
        .expect_err("tampered body must fail signature verification");
        assert!(
            err.to_string().contains("content-digest") || err.to_string().contains("signature")
        );
    }

    #[test]
    fn seq_allocator_is_strictly_increasing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let seq = build_applet_seq_allocator(tmp.path(), "applet-seq-test").expect("seq allocator");
        let a = seq.alloc().expect("alloc a");
        let b = seq.alloc().expect("alloc b");
        assert!(b > a, "expected strictly increasing seq: a={a}, b={b}");
    }

    #[test]
    fn bearer_header_parser_trims_without_leaking() {
        assert_eq!(parse_bearer_header("Bearer  abc123  "), Some("abc123"));
        assert_eq!(parse_bearer_header("bearer abc123"), Some("abc123"));
        assert_eq!(parse_bearer_header("Basic abc123"), None);
        assert_eq!(parse_bearer_header("Bearer   "), None);
    }

    #[test]
    fn applet_token_match_requires_configured_token() {
        assert!(applet_token_matches(Some("secret"), "secret"));
        assert!(!applet_token_matches(Some("secret"), "wrong"));
        assert!(!applet_token_matches(None, "secret"));
        assert!(!applet_token_matches(Some(""), "secret"));
    }
}
