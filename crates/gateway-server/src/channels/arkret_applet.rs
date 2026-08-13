//! Arkret Applet HTTP server endpoints.
//!
//! When savfox runs as a registered Arkret Applet (savfox channel config
//! with `kind = "arkret"` + `mode = "applet"`), this module hosts the
//! inbound HTTP routes the Arkret server calls. Mirrors the Matrix
//! Appservice route layout in [`channels::matrix`](crate::channels::matrix)
//! one-for-one:
//!
//! Paths follow the Arkret spec `edge` trust segment (applet-integration.md
//! §6) — versionless, under the `/_arkret/edge/applet/...` namespace:
//!
//! | path | method | corresponds to |
//! |---|---|---|
//! | `/_arkret/edge/applet/ping` | GET | Matrix `/_matrix/app/v1/ping` |
//! | `/_arkret/edge/applet/describe` | GET | (new — capability descriptor) |
//! | `/_arkret/edge/applet/transactions` | POST | Matrix `PUT /_matrix/app/v1/transactions/{txn_id}` |
//! | `/_arkret/edge/applet/actors/{actor_id}` | GET | Matrix `/_matrix/app/v1/users/{user_id}` |
//! | `/_arkret/edge/applet/realms/{realm_id_or_alias}` | GET | Matrix `/_matrix/app/v1/rooms/{room_alias}` |
//! | `/_arkret/edge/applet/protocols/{protocol}` | GET | Matrix `/_matrix/app/v1/thirdparty/protocol/{protocol}` |
//! | `/_arkret/edge/applet/third_party/users` | GET | Third-party actor lookup |
//! | `/_arkret/edge/applet/third_party/locations` | GET | Third-party realm lookup |
//!
//! Two mount points (mirroring matrix.rs convention):
//!
//! * Direct: `/_arkret/edge/applet/...` — auth resolves the channel via bearer.
//! * Per-config: `/appservices/arkret/{config_id}/_arkret/edge/applet/...`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use arkret::http_signature::{
    Component, HttpMessageVerificationError, SignaturePolicyError, SignatureVerificationPolicy,
    parse_signature_input, public_key_from_bytes, verify_signed_http_message,
};
use arkret::{
    AppletActorView, AppletPingOutcome, AppletProtocolMetadata, AppletRealmView,
    AppletTransactionOutcome, AppletTransactionRequestBody, ContentBlock, Did,
    EventPayloadExt as _, Hash, IdempotencyClaim, IdempotencyDirection, IdempotencyIdentity,
    IdempotencyWindow, MessageCreatePayload, RealmId, RejectedItem, ServiceDescribe, ServiceKind,
    ServiceOperationId, StrandId, TypedTrustDomainId, canonical,
};
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::arkret::applet::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, ArkretAppletConfig,
    classify_inbound_event, load_arkret_applet_configs,
};
use savfox_channels::arkret::{
    AppletNamespacesExt, ArkretDecryptOutcome, ArkretEncryptOutcome, FileArkretCryptoStore,
    UnableToDecryptReason, extract_encrypted_payload_from_message_content,
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
/// `duplicate_conflict` detection when the same operation/direction/source/
/// destination/Idempotency-Key identity arrives with different authenticated
/// request material.
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
    config: ArkretAppletConfig,
    runtime: Mutex<AppletRuntimeState>,
    crypto_store: FileArkretCryptoStore,
    /// Restart-safe monotonic allocator for this applet's outbound
    /// `actor_seq`. Backed by a file-backed [`SeqStore`] under the savfox
    /// home dir; replaces the previous `timestamp_millis()` hack which was
    /// neither monotonic across calls nor restart-safe. `SeqAllocator` is
    /// internally synchronized, so it lives outside the `runtime` Mutex.
    seq: arkret_bridge_runtime::SeqAllocator,
    /// Authenticated outbound edge, initialized lazily and refreshed after a
    /// failed submission so expired DID-proof grants cannot wedge the applet.
    edge: tokio::sync::Mutex<Option<Arc<arkret_bridge_runtime::ArkretEdge>>>,
}

// `SeqAllocator` is not `Debug`; provide a manual impl that elides it so
// `AppletChannelState` keeps a `Debug` representation for tracing/asserts.
impl std::fmt::Debug for AppletChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppletChannelState")
            .field("config", &self.config)
            .field("runtime", &self.runtime)
            .field("crypto_store_path", &self.crypto_store.path())
            .field(
                "edge_initialized",
                &self.edge.try_lock().map(|edge| edge.is_some()).ok(),
            )
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
const SOURCE_SERVICE_ID_HEADER: &str = "source-service-id";
const DESTINATION_SERVICE_ID_HEADER: &str = "destination-service-id";
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
/// Must be called when an Arkret applet channel is disabled, deleted, or
/// reconfigured. Without this, a stale `AppletChannelState` (carrying the
/// bearer token and namespace patterns) would linger forever and keep matching
/// `lookup_by_bearer` / `lookup_by_realm`, dispatching to a channel the
/// operator already removed. Mirrors `matrix::remove_matrix_appservice_channel`.
pub(crate) fn remove_arkret_applet_channel(config_id: &str) -> anyhow::Result<bool> {
    let mut reg = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    Ok(reg.remove(config_id).is_some())
}

pub(crate) fn is_arkret_applet_registered(config_id: &str) -> bool {
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
        .find(|state| applet_token_matches(state.config.arkret_bearer_token.as_deref(), &token))
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
    source_service_id: String,
    destination_service_id: String,
    signature_label: String,
    key_id: String,
    verification_key_digest: String,
    signature_algorithm: String,
    content_digest: String,
    covered_components: Vec<String>,
    created: i64,
    expires: i64,
}

fn render_state_unavailable(res: &mut Response, err: &anyhow::Error) {
    warn!("arkret applet state unavailable: {err:#}");
    render_error(
        res,
        StatusCode::INTERNAL_SERVER_ERROR,
        "state_unavailable",
        "Arkret applet state unavailable",
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
                    "Arkret applet endpoint requires Authorization: Bearer <token>",
                );
                return None;
            };
            if applet_token_matches(state.config.arkret_bearer_token.as_deref(), &token) {
                return Some(state);
            }
            render_unauthorized(
                res,
                "invalid_bearer_token",
                "Authorization token does not match this Arkret applet channel",
            );
            return None;
        }
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "applet_not_found",
            format!("no arkret applet channel configured with id '{config_id}'"),
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
                "no arkret applet channel is currently registered",
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
        "Arkret applet endpoint requires a matching Authorization bearer token",
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
        // `service_id` is strictly validated (Did::new) in
        // `ArkretAppletConfig::validate()` before the channel is registered,
        // so a registered applet always has a parseable DID here. No silent
        // `applet.unknown` fallback that would mask a config error.
        service_id: Did::new(state.config.service_id.clone())
            .expect("service_id validated at channel registration"),
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
    let Some(host) = url::Url::parse(&cfg.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    else {
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_applet_base_url",
            "Arkret applet base_url has no trust-domain host",
        );
        return;
    };
    let Ok(trust_domain) = TypedTrustDomainId::new(format!("ak:trust_domain:{host}")) else {
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_trust_domain",
            "Arkret applet base_url host is not a valid trust domain",
        );
        return;
    };
    let mut body = ServiceDescribe::development(
        Did::new(cfg.service_id.clone()).expect("service_id validated at channel registration"),
        trust_domain,
        ServiceKind::AppletService,
    );
    body.supported_profiles = vec!["ak.profile.applet.v1".to_owned()];
    body.supported_operations = vec![
        ServiceOperationId::EDGE_APPLET_QUERY_PING.to_owned(),
        ServiceOperationId::EDGE_APPLET_QUERY_DESCRIBE.to_owned(),
        ServiceOperationId::EDGE_APPLET_COMMAND_TRANSACTION.to_owned(),
        ServiceOperationId::EDGE_APPLET_ACTOR_QUERY_RESOLVE.to_owned(),
        ServiceOperationId::EDGE_APPLET_REALM_QUERY_RESOLVE.to_owned(),
        ServiceOperationId::EDGE_APPLET_QUERY_PROTOCOL_METADATA.to_owned(),
        ServiceOperationId::EDGE_APPLET_THIRD_PARTY_USERS_QUERY_LIST.to_owned(),
        ServiceOperationId::EDGE_APPLET_THIRD_PARTY_LOCATIONS_QUERY_LIST.to_owned(),
    ];
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
            "Arkret applet transactions require an Idempotency-Key header",
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
                    "Arkret applet transaction body exceeds {MAX_APPLET_TRANSACTION_BODY_BYTES} bytes"
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

    let source_service_id = body.source_service_id.as_str().to_owned();
    if let Some(expected_source) = state.config.arkret_server_did.as_deref()
        && source_service_id != expected_source
    {
        render_unauthorized(
            res,
            "invalid_source_service_id",
            "Arkret applet transaction source_service_id does not match the trusted server DID",
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
                "Arkret applet transaction HTTP message signature verification failed",
            );
            return;
        }
    };
    let Some(signature) = verified_http_signature.as_ref() else {
        render_unauthorized(
            res,
            "http_signature_required",
            "Arkret applet transactions require a verified HTTP message signature",
        );
        return;
    };
    if signature.source_service_id != source_service_id {
        render_unauthorized(
            res,
            "invalid_signature",
            "Arkret applet transaction source_service_id does not match signed source service DID",
        );
        return;
    }

    // Canonical body hash for idempotency body-equality check (spec §7.3).
    // Same `(source_service_id, key)` with matching body → return cached
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

    // Preserve the received field value in the protocol record. Trimming is
    // used only above to reject an all-whitespace value.
    let Some(registration_epoch) = state
        .config
        .registration_epoch
        .as_deref()
        .filter(|epoch| !epoch.is_empty())
    else {
        render_unauthorized(
            res,
            "applet_registration_unauthorized",
            "Arkret applet transaction has no effective registration epoch",
        );
        return;
    };
    let identity = IdempotencyIdentity::applet_transaction(
        IdempotencyDirection::NodeToApplet,
        signature.source_service_id.clone(),
        state.config.service_id.clone(),
        idempotency_key.clone(),
    );
    let delivery_authentication_record = json!({
        "operation_id": ServiceOperationId::EDGE_APPLET_COMMAND_TRANSACTION,
        "direction": IdempotencyDirection::NodeToApplet.as_str(),
        "source_service_id": &signature.source_service_id,
        "destination_service_id": &signature.destination_service_id,
        "signature_label": &signature.signature_label,
        "verification_method": &signature.key_id,
        "verification_key_digest": &signature.verification_key_digest,
        "signature_algorithm": &signature.signature_algorithm,
        "registration_epoch": registration_epoch,
        "idempotency_key": &idempotency_key,
        "content_digest": &signature.content_digest,
        "covered_components": &signature.covered_components,
        "created": signature.created,
        "expires": signature.expires,
    });
    let delivery_authentication_record_digest =
        match canonical::canonical_json_bytes(&delivery_authentication_record) {
            Ok(record_bytes) => {
                let mut transcript = b"ak.applet.delivery-authentication-record.v1\n".to_vec();
                transcript.extend(record_bytes);
                canonical::sha256_digest(transcript)
            }
            Err(err) => {
                warn!("applet: delivery authentication record digest failed: {err}");
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "delivery_authentication_record_digest_failed",
                    "failed to compute delivery authentication record digest",
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
                "Arkret applet runtime state unavailable",
            );
            return;
        };
        runtime_state.txn_dedupe.gc();
        match runtime_state.txn_dedupe.claim(
            &identity,
            &body_hash,
            &delivery_authentication_record_digest,
        ) {
            IdempotencyClaim::Fresh => {}
            IdempotencyClaim::Duplicate { outcome, .. } => {
                debug!(
                    "applet: duplicate transaction (matching body hash/delivery authentication record digest) — returning cached outcome"
                );
                res.status_code(StatusCode::OK);
                res.render(Json(outcome));
                return;
            }
            IdempotencyClaim::DuplicateConflict { .. } => {
                warn!(
                    "applet: idempotency conflict — same identity with different body hash or delivery authentication record digest"
                );
                render_error(
                    res,
                    StatusCode::CONFLICT,
                    "duplicate_conflict",
                    "Idempotency-Key already used for a different request body or delivery authentication record digest",
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
    let mut rejected: Vec<RejectedItem> = Vec::new();
    let mut dispatched_commands = Vec::new();
    for event in body.events.iter() {
        if record_applet_mls_welcome_from_event(state.as_ref(), event) {
            continue;
        }
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
                        "arkret applet: encrypted inbound event rejected; crypto session decrypt is not wired"
                    );
                }
                rejected.push(RejectedItem {
                    event_id: Some(event.event_id.clone()),
                    reason_code: reason.reason_code(),
                    retry_after_ms: None,
                });
            }
        }
    }

    // Dispatch each accepted command via gateway-server runtime.
    let Ok(gateway_channel) = depot.get_typed::<Arc<GatewayChannel>>() else {
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
    let Ok(session_store) = depot.get_typed::<Arc<SessionStore>>() else {
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
    let applet_account_id = state.config.applet_id.clone();
    let applet_agent_did = state.config.bot_actor_id.clone();

    for cmd in dispatched_commands {
        let gw = gateway_channel.clone();
        let store = session_store.clone();
        let cid = config_id.clone();
        let account_id = applet_account_id.clone();
        let agent_did = applet_agent_did.clone();
        let dedupe_key = format!("arkret-applet:{}:{}", cid, cmd.event_id);
        if runtime::should_drop_duplicate(Some(dedupe_key)).await {
            continue;
        }
        tokio::spawn(async move {
            let conversation = crate::arkret_delivery::RemoteConversationKey {
                channel_config_id: cid.clone(),
                account_id: account_id.clone(),
                realm_id: cmd.realm_id.clone(),
                strand_id: cmd.strand_id.clone(),
            };
            if let Err(error) = crate::arkret_delivery::ArkretExecutionBindingStore::new(
                &gw.config().savfox_home,
            )
            .mark_history_unavailable(
                conversation,
                "history_unavailable: applet transaction delivery has no timeline query capability",
            )
            .await
            {
                warn!(
                    config_id = %cid,
                    event_id = %cmd.event_id,
                    "arkret applet: failed to record unavailable public history: {error:#}"
                );
            }
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gw,
                store,
                "arkret",
                cmd.realm_id.clone(),
                cmd.body,
                Some(cmd.sender_did.clone()),
                Some(runtime::StartThreadMeta {
                    peer_id: Some(cmd.sender_did),
                    routing_channel_id: Some(format!("arkret:{cid}:{account_id}")),
                    routing_group_id: Some(cmd.realm_id.clone()),
                    routing_thread_id: Some(cmd.strand_id.clone()),
                    group_id: Some(cmd.realm_id.clone()),
                    thread_id: cmd.thread_root_id,
                    reply_target: Some(cmd.strand_id.clone()),
                    account_id: Some(account_id),
                    chat_type: Some("group".to_owned()),
                    saved_channel_config_id: Some(cid),
                    remote_realm_id: Some(cmd.realm_id.clone()),
                    remote_strand_id: Some(cmd.strand_id),
                    remote_event_id: Some(cmd.event_id),
                    remote_agent_did: Some(agent_did),
                    delivery_mode: Some("interactive_chat".to_owned()),
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

    let expected_source = state.config.arkret_server_did.as_deref().ok_or_else(|| {
        anyhow::anyhow!("trusted verification methods require arkret_server_did / trustedServerDid")
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

    let source = header_value_from(headers, SOURCE_SERVICE_ID_HEADER)
        .ok_or_else(|| anyhow::anyhow!("{SOURCE_SERVICE_ID_HEADER} header is required"))?;
    if source != expected_source {
        anyhow::bail!(
            "HTTP signature source service DID '{source}' does not match trusted server DID '{expected_source}'"
        );
    }
    let destination = header_value_from(headers, DESTINATION_SERVICE_ID_HEADER)
        .ok_or_else(|| anyhow::anyhow!("{DESTINATION_SERVICE_ID_HEADER} header is required"))?;
    if destination != state.config.service_id {
        anyhow::bail!(
            "HTTP signature destination service DID '{destination}' does not match applet service DID '{}'",
            state.config.service_id
        );
    }

    let public_key_bytes = trusted_method
        .public_key
        .ed25519_bytes()
        .map_err(|err| anyhow::anyhow!("trusted HTTP signature public key: {err}"))?;
    let verification_key_digest = canonical::sha256_digest(&public_key_bytes);
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
        Component::Header(SOURCE_SERVICE_ID_HEADER.to_owned()),
        Component::Header(DESTINATION_SERVICE_ID_HEADER.to_owned()),
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
        .map(|digest| digest.wire_value.clone())
        .ok_or_else(|| anyhow::anyhow!("verified HTTP signature has no Content-Digest"))?;
    let covered_components = verified
        .signature_input
        .covered_components
        .iter()
        .map(Component::canonical_name)
        .collect();
    Ok(Some(VerifiedAppletHttpSignature {
        source_service_id: source,
        destination_service_id: destination,
        signature_label: verified.signature_input.label,
        key_id: verified.signature_input.key_id,
        verification_key_digest,
        signature_algorithm: verified.signature_input.algorithm,
        content_digest,
        covered_components,
        created: verified.signature_input.created,
        expires: verified.signature_input.expires,
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
            SignaturePolicyError::CreatedInFuture
            | SignaturePolicyError::CreatedTooOld
            | SignaturePolicyError::Expired,
        ) => anyhow::anyhow!("HTTP signature timestamp is outside the accepted window"),
        HttpMessageVerificationError::ContentEncodingNotAllowed => {
            anyhow::anyhow!("signed applet transaction must not use Content-Encoding")
        }
        HttpMessageVerificationError::NonCanonicalJson(detail) => {
            anyhow::anyhow!("signed applet transaction body is not canonical JSON: {detail}")
        }
        HttpMessageVerificationError::Signature(err) => anyhow::anyhow!("{err}"),
    }
}

fn record_applet_mls_welcome_from_event(state: &AppletChannelState, event: &arkret::Event) -> bool {
    let payload = Value::Object(event.payload.clone().into_iter().collect());
    record_applet_mls_welcome_from_value_tree(state, event, &payload, 6) > 0
}

fn record_applet_mls_welcome_from_value_tree(
    state: &AppletChannelState,
    event: &arkret::Event,
    value: &Value,
    remaining_depth: usize,
) -> usize {
    match state.crypto_store.record_mls_welcome_from_value(value) {
        Ok(Some(welcome)) => {
            debug!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                realm_id = event.realm_id.as_str(),
                group_id = %welcome.group_id,
                epoch = welcome.epoch,
                recipient_principal_id = %welcome.recipient_principal_id.as_str(),
                recipient_device_id = %welcome.recipient_device_id.as_str(),
                "arkret applet: recorded MLS Welcome from inbound transaction event"
            );
            return 1;
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                realm_id = event.realm_id.as_str(),
                "arkret applet: failed to persist MLS Welcome from inbound transaction event: {err:#}"
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
                record_applet_mls_welcome_from_value_tree(state, event, item, remaining_depth - 1)
            })
            .sum(),
        Value::Object(object) => object
            .values()
            .map(|item| {
                record_applet_mls_welcome_from_value_tree(state, event, item, remaining_depth - 1)
            })
            .sum(),
        _ => 0,
    }
}

fn try_decrypt_applet_event(
    state: &AppletChannelState,
    event: &arkret::Event,
) -> Option<AppletInboundCommand> {
    let message = event.as_message_create().ok()?;
    let strand_id = message.strand_id.as_str().to_owned();
    let reply_to = message.reply_to;
    let payload = extract_encrypted_payload_from_message_content(&event.payload)?;
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
                "arkret applet: planned crypto bootstrap for encrypted event"
            ),
            Err(err) => warn!(
                config_id = %state.config.id,
                "arkret applet: failed to plan crypto bootstrap for encrypted event: {err:#}"
            ),
        }
    }

    match state.crypto_store.try_decrypt_content_block(&payload) {
        Ok(ArkretDecryptOutcome::Decrypted(content)) => {
            let Some(body) = decrypted_text_body(&content) else {
                warn!(
                    config_id = %state.config.id,
                    event_id = event.event_id.as_str(),
                    "arkret applet: decrypted encrypted event but content is not displayable text"
                );
                return None;
            };
            Some(AppletInboundCommand {
                event_id: event.event_id.as_str().to_owned(),
                realm_id: event.realm_id.as_str().to_owned(),
                strand_id,
                sender_did: event.actor_id.as_str().to_owned(),
                body,
                thread_root_id: reply_to,
            })
        }
        Ok(ArkretDecryptOutcome::MissingGroupState) => {
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                UnableToDecryptReason::NoSession,
            );
            None
        }
        Ok(ArkretDecryptOutcome::UnsupportedScheme(scheme)) => {
            warn!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                scheme,
                "arkret applet: unsupported encrypted payload scheme"
            );
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                UnableToDecryptReason::BadCiphertext,
            );
            None
        }
        Err(err) => {
            warn!(
                config_id = %state.config.id,
                event_id = event.event_id.as_str(),
                "arkret applet: encrypted event decrypt failed: {err:#}"
            );
            record_applet_unable_to_decrypt(
                state,
                event,
                payload,
                UnableToDecryptReason::BadCiphertext,
            );
            None
        }
    }
}

fn record_applet_unable_to_decrypt(
    state: &AppletChannelState,
    event: &arkret::Event,
    payload: arkret::EncryptedPayload,
    reason: UnableToDecryptReason,
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
            "arkret applet: failed to persist unable-to-decrypt record: {err:#}"
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
        external_ref: None,
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
        realm_id: arkret::RealmId::new(realm).ok(),
        title: None,
        external_ref: None,
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
        field_definitions: Default::default(),
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
    cfg: &ArkretAppletConfig,
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
        savfox_channels::arkret::mint_ghost_did(
            &state.config.service_id,
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
/// can fall back to account-mode Arkret sending.
pub(crate) async fn send_to_arkret_applet_for_realm(
    realm_id: &str,
    strand_id: Option<&str>,
    body: &str,
) -> anyhow::Result<bool> {
    let Some(state) = lookup_by_realm(realm_id)? else {
        return Ok(false);
    };
    let strand_id = strand_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret applet '{}' cannot reply to realm '{}' without a strand id",
            state.config.id,
            realm_id
        )
    })?;
    let external_ref = json!({
        "protocol": "savfox",
        "network_id": state.config.id,
        "external_id": format!("{realm_id}:{strand_id}"),
        "kind": "agent_reply",
    });
    // `actor_seq` is no longer derived from a wall-clock timestamp here — it
    // is sourced inside `send_via_applet` from the per-applet
    // `arkret-bridge-runtime` `SeqAllocator` (monotonic, restart-safe).
    send_via_applet(
        &state.config.id,
        realm_id,
        strand_id,
        &state.config.bot_actor_id,
        body,
        external_ref,
    )
    .await?;
    Ok(true)
}

/// Send a Ghost-actor-attributed `ak.message.create` Event from this applet
/// to the Arkret server. On failure, emit a best-effort
/// `ak.applet.bridge_error` Event so receivers don't silently lose state
/// (spec applet-integration.md §14).
///
/// `config_id` looks up the registered applet; `external_ref` is the
/// bridge-side origin (protocol/network/external_id) for audit.
///
/// The outbound `actor_seq` is allocated from the applet's
/// `arkret-bridge-runtime` [`SeqAllocator`] (file-backed [`SeqStore`]),
/// giving a monotonic, restart-safe sequence — no longer
/// `chrono::Utc::now().timestamp_millis()`.
pub(crate) async fn send_via_applet(
    config_id: &str,
    realm_id: &str,
    strand_id: &str,
    ghost_actor_did: &str,
    body: &str,
    external_ref: Value,
) -> anyhow::Result<()> {
    let state = lookup_by_config_id(config_id)
        .with_context(|| format!("arkret applet '{config_id}' registry lookup failed"))?
        .ok_or_else(|| anyhow::anyhow!("arkret applet '{config_id}' not registered"))?;
    let cfg = &state.config;
    let edge = applet_edge(&state).await?;

    // Phase 8 (T8.E): if a grant is configured, attach its event_id as
    // authorization_ref on the outbound event.
    let authorization_ref = load_applet_grant_event_id(cfg)
        .await
        .or_else(|| cfg.authorization_grant_id.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "arkret applet '{}' requires an authorization grant for delegated outbound",
                cfg.id
            )
        })?;
    let realm = RealmId::new(realm_id.to_owned())
        .with_context(|| format!("invalid realm_id: {realm_id}"))?;
    let actor = Did::new(ghost_actor_did.to_owned())
        .with_context(|| format!("invalid ghost actor DID: {ghost_actor_did}"))?;
    let strand = StrandId::new(strand_id.to_owned())
        .with_context(|| format!("invalid strand_id: {strand_id}"))?;
    let content = ContentBlock::text(body.to_owned());
    let payload = MessageCreatePayload::with_content(strand, "discussion", content);
    let mut event = edge
        .mint_event_as_unsigned_async(
            &actor,
            "ak.message.create",
            &realm,
            payload,
            &authorization_ref,
        )
        .await
        .map_err(|err| anyhow::anyhow!("arkret edge mint: {err}"))?;
    let external_ref_object = external_ref
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Arkret external_ref must be an object"))?;
    event.external_ref = Some(external_ref_object.into_iter().collect());
    apply_applet_outbound_encryption(&state.crypto_store, realm_id, &mut event)?;
    edge.resign_event(&mut event)
        .map_err(|err| anyhow::anyhow!("arkret edge sign: {err}"))?;

    // A transport 200 is not delivery confirmation: inspect the business-level
    // result and treat a rejection (or zero accepted/duplicate events) as a
    // failure so it flows through the same bridge_error path as a transport
    // error (spec §14), instead of being silently dropped.
    let submit_result = match edge.submit_event(&event).await {
        Ok(_) => Ok(()),
        Err(first) => {
            debug!(config_id, error = %first, "arkret applet submit failed; refreshing edge once");
            let refreshed = refresh_applet_edge(&state).await?;
            refreshed
                .submit_event(&event)
                .await
                .map(|_| ())
                .map_err(|err| anyhow::anyhow!(err.to_string()))
        }
    };
    match submit_result {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!(
                config_id,
                realm_id,
                strand_id,
                error = %err,
                "arkret applet: send_via_applet submission failed — emitting bridge_error"
            );
            // Best-effort bridge_error emission. If THIS submit also fails,
            // we log and continue — there is no escalation path for a
            // double failure in Phase 7.
            if let Err(err2) = emit_bridge_error(
                &state,
                realm_id,
                "arkret_submit_failed",
                &err.to_string(),
                Some(external_ref),
            )
            .await
            {
                warn!(config_id, error = %err2, "arkret applet: bridge_error emit also failed");
            }
            Err(err)
        }
    }
}

fn apply_applet_outbound_encryption(
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
            event.payload.remove("content");
            event.payload.insert(
                "encrypted_content".to_owned(),
                serde_json::to_value(encrypted_content.into_envelope())?,
            );
            Ok(())
        }
        ArkretEncryptOutcome::MissingRequiredGroupState { realm_id, group_id } => {
            anyhow::bail!(
                "Arkret realm '{realm_id}' requires E2EE but no local applet MLS group state exists for group '{group_id}'"
            );
        }
    }
}

/// Emit a `ak.applet.bridge_error` Event (SDK S-11).
///
/// `code` should be one of the spec-blessed strings:
/// `external_rate_limited` / `external_rejected` / `arkret_submit_failed` /
/// `delivery_unconfirmed`. `message` is human-readable.
async fn emit_bridge_error(
    state: &Arc<AppletChannelState>,
    realm_id: &str,
    code: &str,
    message: &str,
    external_ref: Option<Value>,
) -> anyhow::Result<()> {
    use arkret::{
        AppletBridgeErrorBuilder, AppletBridgeErrorClass, AppletBridgeVisibilityScope, AppletId,
        AppletIdentifier,
    };

    let cfg = &state.config;
    let realm = RealmId::new(realm_id.to_owned())
        .with_context(|| format!("invalid realm_id: {realm_id}"))?;
    let actor = Did::new(cfg.bot_actor_id.clone())
        .with_context(|| format!("invalid bot DID: {}", cfg.bot_actor_id))?;
    let applet_id = AppletId::new(cfg.applet_id.clone())
        .with_context(|| format!("invalid applet_id: {}", cfg.applet_id))?;
    // SDK S-13 reshaped the payload: `severity` → `error_class` /
    // `error_code` / `retriable` / `visibility_scope` /
    // `failed_transaction_ref`. An outbound submit failure has no inbound
    // transaction id, so anchor on the target realm (MUST NOT inline external
    // plaintext). A hard upstream rejection is terminal; other classes are
    // retriable.
    let failed_transaction_ref = format!("outbound:{realm_id}");
    let retriable = !matches!(code, "external_rejected");
    let mut builder = AppletBridgeErrorBuilder::new(
        realm.clone(),
        AppletIdentifier::Cx(applet_id),
        actor,
        failed_transaction_ref,
        AppletBridgeErrorClass::ExternalNetwork,
        code,
        retriable,
        AppletBridgeVisibilityScope::RealmAdmins,
    )
    .with_message(message);
    if let Some(ext) = external_ref {
        builder = builder.with_external_ref(ext);
    }
    let edge = applet_edge(state).await?;
    edge.submit_bridge_error(&realm, builder)
        .await
        .map_err(|err| anyhow::anyhow!("arkret bridge_error submit: {err}"))?;
    Ok(())
}

/// Build the outbound HTTP client for an applet config using DID-proof login.
async fn construct_applet_client(
    cfg: &savfox_channels::arkret::ArkretAppletConfig,
) -> anyhow::Result<savfox_channels::arkret::ArkretHttpClient> {
    if let Some(key_ref) = &cfg.key_ref {
        use savfox_channels::arkret::ArkretHttpClient;
        let vm = cfg
            .verification_method
            .clone()
            .unwrap_or_else(|| format!("{}#key-1", cfg.bot_actor_id));
        let audience = cfg.arkret_server_did.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "applet '{}' has key_ref but no arkret_server_did / arkretServerDid for DID-proof audience",
                cfg.id
            )
        })?;
        let challenge = cfg.login_challenge.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "applet '{}' has key_ref but no login_challenge / loginChallenge",
                cfg.id
            )
        })?;
        let signer = savfox_channels::arkret::load_ed25519_signer(key_ref, &cfg.bot_actor_id, &vm)?;
        let principal = arkret::Did::new(cfg.bot_actor_id.clone())
            .map_err(|err| anyhow::anyhow!("invalid bot DID: {err}"))?;
        // Applet configs keep the bot device optional for bearer-only mode.
        // DID-proof login still needs a protocol-valid runtime device id.
        let device = arkret::DeviceId::new(cfg.device_id.clone().unwrap_or_else(|| {
            savfox_channels::arkret::derive_arkret_device_id(&[
                "applet",
                &cfg.id,
                &cfg.applet_id,
                &cfg.bot_actor_id,
            ])
        }))
        .map_err(|err| anyhow::anyhow!("synth device_id: {err}"))?;
        let (client, _session) = ArkretHttpClient::login(
            &cfg.arkret_server_url,
            &signer,
            principal,
            device,
            challenge,
            audience,
        )
        .await?;
        Ok(client)
    } else {
        anyhow::bail!(
            "arkret applet '{}' requires key_ref; unsigned bearer-only outbound is retired",
            cfg.id
        )
    }
}

async fn applet_edge(
    state: &AppletChannelState,
) -> anyhow::Result<Arc<arkret_bridge_runtime::ArkretEdge>> {
    let mut slot = state.edge.lock().await;
    if let Some(edge) = slot.as_ref() {
        return Ok(edge.clone());
    }
    let edge = Arc::new(build_applet_edge(&state.config, state.seq.clone()).await?);
    *slot = Some(edge.clone());
    Ok(edge)
}

async fn refresh_applet_edge(
    state: &AppletChannelState,
) -> anyhow::Result<Arc<arkret_bridge_runtime::ArkretEdge>> {
    let edge = Arc::new(build_applet_edge(&state.config, state.seq.clone()).await?);
    *state.edge.lock().await = Some(edge.clone());
    Ok(edge)
}

async fn build_applet_edge(
    cfg: &savfox_channels::arkret::ArkretAppletConfig,
    seq: arkret_bridge_runtime::SeqAllocator,
) -> anyhow::Result<arkret_bridge_runtime::ArkretEdge> {
    let key_ref = cfg.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "arkret applet '{}' requires key_ref for signed outbound events",
            cfg.id
        )
    })?;
    let verification_method = cfg
        .verification_method
        .clone()
        .unwrap_or_else(|| format!("{}#key-1", cfg.bot_actor_id));
    let signer = savfox_channels::arkret::load_ed25519_signer(
        key_ref,
        &cfg.bot_actor_id,
        &verification_method,
    )?;
    let http = construct_applet_client(cfg).await?;
    let trusted_server_did = cfg
        .arkret_server_did
        .clone()
        .ok_or_else(|| anyhow::anyhow!("arkret applet '{}' missing server DID", cfg.id))?;
    let runtime_config: arkret_bridge_runtime::Config = serde_json::from_value(json!({
        "bridge": { "bridge_id": cfg.id },
        "arkret": {
            "server_url": cfg.arkret_server_url,
            "service_id": cfg.service_id,
            "applet_id": cfg.applet_id,
            "access_token": "",
            "signing_key_seed_hex": "",
            "verification_method_id": verification_method,
            "trusted_server_did": trusted_server_did,
        },
        "app": Value::Null,
    }))
    .context("build arkret runtime edge config")?;
    arkret_bridge_runtime::ArkretEdge::new_outbound(
        Arc::new(runtime_config),
        http.inner().clone(),
        signer,
        seq,
    )
    .map_err(|err| anyhow::anyhow!("build arkret outbound edge: {err}"))
}

/// Phase 8 (T8.E): if `grant_event_path` is set, load + verify the
/// capability grant and return its `event_id` for use as
/// `authorization_ref`. Logs and returns `None` on load failure (grant
/// is operator-managed; missing files shouldn't crash outbound).
async fn load_applet_grant_event_id(
    cfg: &savfox_channels::arkret::ArkretAppletConfig,
) -> Option<String> {
    let path = cfg.grant_event_path.as_ref()?;
    match savfox_channels::arkret::load_and_verify_grant(path, &cfg.bot_actor_id, None).await {
        Ok(grant) if grant.covers_action("ak.message.create") => Some(grant.event_id),
        Ok(_) => {
            warn!(
                "arkret applet '{}': capability grant at {} does not cover ak.message.create",
                cfg.id,
                path.display()
            );
            None
        }
        Err(err) => {
            warn!(
                "arkret applet '{}': capability grant load failed at {}: {err:#}",
                cfg.id,
                path.display()
            );
            None
        }
    }
}

// ─── Router ─────────────────────────────────────────────────────────────────

/// Routes mounted at `/_arkret/edge/applet/...` (direct).
pub(crate) fn arkret_applet_router() -> Router {
    Router::with_path("_arkret/edge/applet")
        .push(Router::with_path("ping").get(applet_ping))
        .push(Router::with_path("describe").get(applet_describe))
        .push(Router::with_path("transactions").post(applet_transactions))
        .push(Router::with_path("actors/{actor_id}").get(applet_actor))
        .push(Router::with_path("realms/{realm_id_or_alias}").get(applet_realm))
        .push(Router::with_path("protocols/{protocol}").get(applet_protocol))
        .push(Router::with_path("third_party/users").get(applet_third_party_users))
        .push(Router::with_path("third_party/locations").get(applet_third_party_locations))
}

/// Routes mounted at `/appservices/arkret/{config_id}/_arkret/edge/applet/...`.
pub(crate) fn arkret_appservices_router() -> Router {
    Router::with_path("appservices/arkret/{config_id}").push(arkret_applet_router())
}

// ─── Startup glue ───────────────────────────────────────────────────────────

/// Build the restart-safe monotonic [`SeqAllocator`] for an applet.
///
/// The backing [`SeqStore`] is a file under
/// `{savfox_home}/gateway/arkret-applet-seq/{config_id}.seq`; the allocator
/// is keyed `applet:{config_id}:actor_seq` so each applet has an independent
/// monotonic counter. Persisting the high-water mark makes `actor_seq`
/// restart-safe — the previous `timestamp_millis()` approach was neither
/// monotonic across rapid calls nor durable across restarts.
fn build_applet_seq_allocator(
    savfox_home: &std::path::Path,
    config_id: &str,
) -> anyhow::Result<arkret_bridge_runtime::SeqAllocator> {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("arkret-applet-seq");
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
    let store = arkret_bridge_runtime::FileSeqStore::shared(path)
        .map_err(|e| anyhow::anyhow!("arkret applet seq store: {e}"))?;
    Ok(arkret_bridge_runtime::SeqAllocator::new(
        store,
        format!("applet:{config_id}:actor_seq"),
    ))
}

/// Start (register) an Arkret Applet channel. Mounts no extra HTTP listener
/// — the routes are added to the main savfox-gateway-server `Router` in
/// `server.rs`. Returns once registry insertion is done.
pub(crate) async fn start_arkret_applet_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    _session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let applet_cfg = ArkretAppletConfig::from_channel_config(config).ok_or_else(|| {
        anyhow::anyhow!("Arkret applet channel '{}' missing or invalid", config.id)
    })?;
    applet_cfg.validate().with_context(|| {
        format!(
            "Arkret applet channel '{}' validation failed",
            applet_cfg.id
        )
    })?;

    // Restart-safe monotonic `actor_seq` source for outbound events. The
    // allocator persists its high-water mark in a per-applet file under the
    // savfox home dir, so sequence numbers never regress across restarts
    // (replacing the old `timestamp_millis()` hack). `SeqAllocator` is keyed
    // per-applet (`config.id`) so multiple applets don't share a counter.
    let savfox_home = channel.config().savfox_home.clone();
    let crypto_store = FileArkretCryptoStore::for_applet(&savfox_home, &applet_cfg.id);
    if let Err(err) = crypto_store.ensure_created() {
        warn!(
            "arkret: applet '{}' crypto state unavailable at {}: {err:#}",
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
        edge: tokio::sync::Mutex::new(None),
    };
    info!(
        "arkret: applet channel '{}' registered (applet_id={}, service_id={})",
        state.config.id, state.config.applet_id, state.config.service_id
    );
    register_channel(state)?;
    Ok(())
}

/// Loader used at gateway startup to count + log configured applet channels
/// without booting them (booting is `start_arkret_applet_channel`).
pub(crate) async fn log_arkret_applet_configs(savfox_home: &std::path::PathBuf) {
    match load_arkret_applet_configs(savfox_home).await {
        Ok(configs) => {
            for cfg in configs {
                info!(
                    "arkret applet config '{}': applet_id={}, service_id={}, protocols={:?}",
                    cfg.id, cfg.applet_id, cfg.service_id, cfg.protocols,
                );
            }
        }
        Err(err) => {
            warn!("arkret applet: failed to load configs: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use arkret::http_signature::{
        Component, ContentDigest, ContentDigestAlgorithm, SignedRequestParts, canonical_message,
        parse_signature_input, sign_message, signing_key_from_seed,
    };
    use arkret::signatures::PublicKeyMaterial;
    use savfox_channels::arkret::applet::ArkretAppletTrustedVerificationMethod;
    use savfox_core::config::channel_store::ChannelConfig;

    use super::*;

    fn valid_channel_config() -> ChannelConfig {
        ChannelConfig {
            id: "applet-test".into(),
            kind: "arkret".into(),
            slug: "applet".into(),
            name: "Applet".into(),
            enabled: true,
            config: json!({
                "mode": "applet",
                "appletId": "ak:applet:21532600-0000-7000-8000-000000000000",
                "serviceId": "did:webvh:bridge.example",
                "controllerId": "did:webvh:example.com:admin",
                "baseUrl": "https://savfox.example/applet-test",
                "botActorId": "did:webvh:bridge.example:bot",
                "arkretServerUrl": "https://arkret.example.org",
                "arkretServerDid": "did:webvh:arkret.example.org",
                "accessToken": "test-bearer",
                "keyRef": {"kind": "env", "var": "SAVFOX_ARKRET_APPLET_TEST_KEY"},
                "loginChallenge": "arkret-applet-test-login-challenge",
                "registrationEpoch": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "protocols": ["slack"],
                "namespaces": {
                    "actors": [{"pattern": "did:webvh:bridge.example:ghost:*", "exclusive": true}],
                    "realms": [{"pattern": "ak:realm:*", "exclusive": true}],
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
        let mut applet = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.trusted_verification_methods = vec![ArkretAppletTrustedVerificationMethod {
            verification_method: "did:webvh:arkret.example.org#key-1".to_owned(),
            public_key: PublicKeyMaterial::Ed25519Raw { bytes: public_key },
        }];
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            crypto_store: FileArkretCryptoStore::for_applet(tmp.path(), &applet.id),
            seq: build_applet_seq_allocator(tmp.path(), &applet.id).expect("seq allocator"),
            edge: tokio::sync::Mutex::new(None),
        }
    }

    fn signed_transaction_headers(body: &[u8], seed: [u8; 32]) -> (Vec<(String, String)>, Vec<u8>) {
        let signing_key = signing_key_from_seed(&seed);
        let public_key = signing_key.verifying_key().to_bytes().to_vec();
        let now = chrono::Utc::now().timestamp();
        let content_digest = ContentDigest::compute(body, ContentDigestAlgorithm::Sha256);
        let signature_input = format!(
            "sig1=(\"@method\" \"@target-uri\" \"@authority\" \
             \"source-service-id\" \"destination-service-id\" \
             \"content-digest\" \"idempotency-key\");created={now};expires={};\
             keyid=\"did:webvh:arkret.example.org#key-1\";alg=\"ed25519\"",
            now + 300
        );
        let mut headers = vec![
            ("host".to_owned(), "savfox.example".to_owned()),
            (
                SOURCE_SERVICE_ID_HEADER.to_owned(),
                "did:webvh:arkret.example.org".to_owned(),
            ),
            (
                DESTINATION_SERVICE_ID_HEADER.to_owned(),
                "did:webvh:bridge.example".to_owned(),
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
            Component::Header(SOURCE_SERVICE_ID_HEADER.to_owned()),
            Component::Header(DESTINATION_SERVICE_ID_HEADER.to_owned()),
            Component::Header("content-digest".to_owned()),
            Component::Header("idempotency-key".to_owned()),
        ]));
        let request = SignedRequestParts {
            method: "POST".to_owned(),
            target_uri: "https://savfox.example/_arkret/edge/applet/transactions".to_owned(),
            authority: "savfox.example".to_owned(),
            path: "/_arkret/edge/applet/transactions".to_owned(),
            headers: headers.clone(),
            body_digest: Some(content_digest.wire_value),
        };
        let message = canonical_message(&request, &parsed).expect("canonical message");
        let signature = sign_message(&message, &signing_key);
        headers.push(("signature".to_owned(), format!("sig1=:{signature}:")));
        (headers, public_key)
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
            welcome_hash: Hash::new(format!("sha256:{}", "cd".repeat(32))).unwrap(),
            ratchet_tree: None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn start_registers_applet_into_registry() {
        let cfg = valid_channel_config();
        // We can't easily build a full GatewayChannel/SessionStore in unit
        // scope — but `start_arkret_applet_channel` only uses them for
        // logging context and accepts &Arc<...>. We use placeholder Arcs.
        // Actually it doesn't use them at all in Phase 6, so dummies are fine.
        // We bypass by calling internals directly:
        let applet = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            crypto_store: FileArkretCryptoStore::for_applet(tmp.path(), &applet.id),
            seq: build_applet_seq_allocator(tmp.path(), &applet.id).expect("seq allocator"),
            edge: tokio::sync::Mutex::new(None),
        };
        register_channel(state).expect("register");
        let resolved = lookup_by_config_id(&applet.id)
            .expect("lookup")
            .expect("registered");
        assert_eq!(resolved.config.applet_id, applet.applet_id);
    }

    #[test]
    fn applet_transaction_event_records_nested_mls_welcome() {
        let cfg = valid_channel_config();
        let applet = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            crypto_store: FileArkretCryptoStore::for_applet(tmp.path(), &applet.id),
            seq: build_applet_seq_allocator(tmp.path(), &applet.id).expect("seq allocator"),
            edge: tokio::sync::Mutex::new(None),
        };
        let group_id = "group-applet-welcome";
        let event = arkret::Event::new(
            "ak.mls.welcome",
            arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new("ak:realm:01904100-0000-8000-8000-000000000123")
                    .unwrap(),
            },
            Did::new("did:webvh:acme:alice".to_owned()).unwrap(),
            1,
            arkret::Hlc::new("000000000000-0000-00000000").unwrap(),
            json!({
                "kind": "ak.mls.welcome",
                "content": mls_welcome_value(group_id)
            }),
        )
        .unwrap();

        assert!(record_applet_mls_welcome_from_event(&state, &event));
        let saved = state.crypto_store.load().expect("crypto state should load");
        assert!(saved.bootstrap.contains_key(group_id));
    }

    #[test]
    fn verifies_trusted_http_message_signature() {
        let body = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_id": "did:webvh:arkret.example.org",
            "events": []
        }))
        .expect("body should serialize");
        let (headers, public_key) = signed_transaction_headers(&body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        let verified = verify_applet_transaction_http_signature(
            &state,
            "POST",
            Some("https://savfox.example/_arkret/edge/applet/transactions"),
            Some("savfox.example"),
            "/_arkret/edge/applet/transactions",
            &headers,
            &body,
        )
        .expect("signature should verify")
        .expect("signature should be required");
        assert_eq!(verified.source_service_id, "did:webvh:arkret.example.org");
        assert_eq!(verified.destination_service_id, "did:webvh:bridge.example");
        assert_eq!(verified.signature_label, "sig1");
        assert_eq!(verified.key_id, "did:webvh:arkret.example.org#key-1");
        assert_eq!(verified.signature_algorithm, "ed25519");
        assert!(verified.verification_key_digest.starts_with("sha256:"));
        assert!(verified.content_digest.starts_with("sha-256=:"));
    }

    #[test]
    fn rejects_tampered_http_message_signature_body() {
        let body = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_id": "did:webvh:arkret.example.org",
            "events": []
        }))
        .expect("body should serialize");
        let (headers, public_key) = signed_transaction_headers(&body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        let tampered = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_service_id": "did:webvh:arkret.example.org",
            "events": [{"kind":"ak.message.create"}]
        }))
        .expect("tampered body should serialize");
        let err = verify_applet_transaction_http_signature(
            &state,
            "POST",
            Some("https://savfox.example/_arkret/edge/applet/transactions"),
            Some("savfox.example"),
            "/_arkret/edge/applet/transactions",
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
