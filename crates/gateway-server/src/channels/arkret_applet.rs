//! Arkret Applet Service management endpoints.
//!
//! HTTPS discovery and Station-signed authoring completion are independent of
//! group content delivery. Group Events and Signals require a native managed
//! Device; management transactions never dispatch them to a model.
//! Fresh managed identity custody is currently unavailable in this host.
//!
//! Direct routes use `/_arkret/edge/applet/...`; per-configuration routes use
//! `/appservices/arkret/{config_id}/_arkret/edge/applet/...`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use arkret::http_signature::{
    Component, HttpMessageVerificationError, HttpSignatureScenario, SignaturePolicyError,
    SignatureVerificationPolicy, parse_signature_input, public_key_from_bytes,
    verify_signed_http_message,
};
use arkret::{
    AppletActorView, AppletId, AppletPingOutcome, AppletProtocolMetadata, AppletRealmView,
    AppletTransactionOutcome, AppletTransactionRequestBody, DidCoreId, Hash, IdempotencyClaim,
    IdempotencyDirection, IdempotencyIdentity, IdempotencyWindow, ServiceDescribe, ServiceKind,
    ServiceOperationId, TransportBinding, canonical,
};
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::arkret::AppletNamespacesExt;
use savfox_channels::arkret::applet::{ArkretAppletConfig, load_arkret_applet_configs};
use serde_json::{Map, Value, json};
use tracing::{debug, info, warn};

use super::render_error;
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
    /// Exact signed publications and accepted Realm/full Actor frontiers,
    /// persisted under the Savfox home directory and shared across edge refresh.
    journal: arkret_bridge_runtime::AuthoringJournal,
    /// Authenticated outbound edge, initialized lazily and refreshed after a
    /// failed submission so expired DID-proof grants cannot wedge the applet.
    edge: tokio::sync::Mutex<Option<Arc<arkret_bridge_runtime::ArkretEdge>>>,
}

// `AuthoringJournal` is not `Debug`; provide a manual impl that elides it so
// `AppletChannelState` keeps a `Debug` representation for tracing/asserts.
impl std::fmt::Debug for AppletChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppletChannelState")
            .field("config", &self.config)
            .field("runtime", &self.runtime)
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
const MAX_APPLET_TRANSACTION_BODY_BYTES: usize = 16 * 1024 * 1024;
const SOURCE_SERVICE_ID_HEADER: &str = "source-service-id";
const DESTINATION_SERVICE_ID_HEADER: &str = "destination-service-id";

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
/// reconfigured, so stale Service management state cannot survive removal.
/// Mirrors `matrix::remove_matrix_appservice_channel`.
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

fn select_applet_management_target(
    req: &Request,
) -> anyhow::Result<Option<Arc<AppletChannelState>>> {
    if let Some(id) = req.param::<String>("config_id") {
        return lookup_by_config_id(&id);
    }
    let destination = req.header::<String>(DESTINATION_SERVICE_ID_HEADER);
    let registry = applet_registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("applet registry poisoned"))?;
    let mut targets = registry.values().filter(|state| {
        destination
            .as_ref()
            .is_none_or(|destination| &state.config.service_id == destination)
    });
    let target = targets.next().cloned();
    anyhow::ensure!(
        targets.next().is_none(),
        "ambiguous Applet management target; use its registered configuration URL"
    );
    Ok(target)
}

fn resolve_public_applet(req: &Request, res: &mut Response) -> Option<Arc<AppletChannelState>> {
    match select_applet_management_target(req) {
        Ok(Some(state)) => Some(state),
        Ok(None) => {
            render_error(
                res,
                StatusCode::NOT_FOUND,
                "applet_not_found",
                "Applet management target is unconfigured",
            );
            None
        }
        Err(error) => {
            render_state_unavailable(res, &error);
            None
        }
    }
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[handler]
async fn applet_ping(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let body = AppletPingOutcome {
        applet_id: AppletId::new(state.config.applet_id.clone())
            .expect("applet_id validated at channel registration"),
        // `service_id` is strictly validated (DidCoreId::new) in
        // `ArkretAppletConfig::validate()` before the channel is registered,
        // so a registered applet always has a parseable DID here. No silent
        // `applet.unknown` fallback that would mask a config error.
        service_id: DidCoreId::new(state.config.service_id.clone())
            .expect("service_id validated at channel registration"),
        protocol_version: "1.0".to_owned(),
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_describe(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let cfg = &state.config;
    let body = ServiceDescribe::development(
        cfg.service_did.clone(),
        cfg.trust_domain.clone(),
        ServiceKind::AppletService,
        vec!["ak.operation_bundle.applet_service.describe.v1".to_owned()],
        vec![TransportBinding::HttpJson {
            base_url: cfg.base_url.clone(),
            extension_profile_required: (),
        }],
    );
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_managed_actor_author_unavailable(req: &mut Request, res: &mut Response) {
    if resolve_public_applet(req, res).is_none() {
        return;
    }
    render_error(
        res,
        StatusCode::SERVICE_UNAVAILABLE,
        "capability_denied",
        "Managed identity authoring custody is not connected in this runtime",
    );
}

#[handler]
async fn applet_transactions(req: &mut Request, _depot: &mut Depot, res: &mut Response) {
    let state = match select_applet_management_target(req) {
        Ok(Some(state)) => state,
        Ok(None) => {
            render_error(
                res,
                StatusCode::NOT_FOUND,
                "applet_not_found",
                "Applet management target is unconfigured",
            );
            return;
        }
        Err(error) => {
            render_state_unavailable(res, &error);
            return;
        }
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

    let source_service_id = body.source_id().as_str().to_owned();
    if let Some(expected_source) = state.config.arkret_server_did.as_deref()
        && arkret::Did::new(expected_source.to_owned())
            .and_then(|did| arkret::project_did_to_core_id(&did))
            .map(|core| core.as_str() != source_service_id)
            .unwrap_or(true)
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
    let Ok(destination_service_id) = DidCoreId::new(state.config.service_id.clone()) else {
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_service_id",
            "Arkret applet channel service_id is not a valid DID core id",
        );
        return;
    };
    let identity = IdempotencyIdentity::applet_transaction(
        IdempotencyDirection::NodeToApplet,
        body.source_id().clone(),
        destination_service_id,
        idempotency_key.clone(),
    );
    let delivery_authentication_record = json!({
        "operation_id": ServiceOperationId::EDGE_APPLET_COMMAND_TRANSACTION_V1,
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

    if let Err(error) = body.validate() {
        render_error(
            res,
            StatusCode::BAD_REQUEST,
            "invalid_transaction",
            error.to_string(),
        );
        return;
    }
    if let AppletTransactionRequestBody::Events(events) = &body {
        if !events.events.is_empty() {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "uncommitted_event",
                "Station-to-Applet transaction requires committed Event and Commit pairs",
            );
            return;
        }
        if !events.signals.is_empty() {
            render_error(
                res,
                StatusCode::NOT_IMPLEMENTED,
                "unsupported_transaction_branch",
                "Applet Signal transaction handling is unavailable",
            );
            return;
        }
    }

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

    let body = match &body {
        AppletTransactionRequestBody::Authoring(completion) => {
            let attester = &completion
                .authoring_context
                .managed_actor_signer_evidence
                .attester_signer_evidence;
            let result = async {
                let method = state
                    .config
                    .trusted_verification_methods
                    .iter()
                    .find(|method| {
                        method.verification_method == attester.verification_method.as_str()
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("completion attester is not a configured own-Station key")
                    })?;
                let edge = applet_edge(&state).await?;
                edge.install_managed_authoring_completion(
                    completion,
                    &arkret::DidUrl::new(method.verification_method.clone())
                        .map_err(anyhow::Error::msg)?,
                    &method.public_key,
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))
            }
            .await;
            if let Err(error) = result {
                release_transaction_claim(&state, &identity);
                render_error(
                    res,
                    StatusCode::BAD_REQUEST,
                    "invalid_authoring_context",
                    &error.to_string(),
                );
                return;
            }
            let outcome = AppletTransactionOutcome::Accepted {
                committed_event_refs: Vec::new(),
                rejections: Vec::new(),
                retry_after_ms: None,
            };
            complete_transaction_claim(&state, &identity, outcome.clone());
            res.status_code(StatusCode::OK);
            res.render(Json(outcome));
            return;
        }
        AppletTransactionRequestBody::Events(events) => events,
    };

    let _ = body;
    release_transaction_claim(&state, &identity);
    render_error(
        res,
        StatusCode::FORBIDDEN,
        "capability_denied",
        "Station-to-Applet Event and Signal subscriptions are prohibited; use an authorized managed Device",
    );
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
    let expected_source_core =
        arkret::project_did_to_core_id(&arkret::Did::new(expected_source.to_owned())?)?;
    let source_core = DidCoreId::new(source.clone())?;
    if source_core != expected_source_core {
        anyhow::bail!(
            "HTTP signature source service identity '{source}' does not match trusted server DID '{expected_source}'"
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
    if header_value_from(headers, "arkret-operation").as_deref()
        != Some("ak.edge.applet.command.transaction.v1")
    {
        anyhow::bail!("Arkret-Operation must name the Applet transaction operation");
    }
    let policy =
        SignatureVerificationPolicy::for_scenario(HttpSignatureScenario::AppletTransactionV1, &[])?;
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
            SignaturePolicyError::UnregisteredConditionalComponent(component),
        ) => anyhow::anyhow!("HTTP signature uses unregistered conditional component: {component}"),
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

#[handler]
async fn applet_actor(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
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
        // Namespace ownership does not establish an accepted managed Account.
        exists: false,
        actor_id: None,
        display_name: None,
        external_ref: None,
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_realm(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
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
        exists: false,
        realm_id: None,
        title: None,
        external_ref: None,
    };
    res.status_code(StatusCode::OK);
    res.render(Json(body));
}

#[handler]
async fn applet_protocol(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
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

#[handler]
async fn applet_third_party_users(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let fields = third_party_query_fields(req);
    let Some(protocol) = ensure_supported_protocol(&state.config, &fields, res) else {
        return;
    };
    let external_ref = third_party_external_ref(&protocol, fields.clone());
    let _ = external_ref;
    // A namespace match is not an accepted Ghost mapping. This runtime has no
    // verified mapping lookup yet, so it cannot advertise a synthetic identity.
    res.status_code(StatusCode::OK);
    res.render(Json(AppletActorView {
        exists: false,
        actor_id: None,
        display_name: None,
        external_ref: None,
    }));
}

#[handler]
async fn applet_third_party_locations(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let fields = third_party_query_fields(req);
    let Some(protocol) = ensure_supported_protocol(&state.config, &fields, res) else {
        return;
    };
    let external_ref = third_party_external_ref(&protocol, fields.clone());
    // A claimed location namespace is not an accepted Portal Realm mapping.
    let realm_id: Option<String> = None;
    let exists = false;

    res.status_code(StatusCode::OK);
    res.render(Json(json!({
        "realm_id": &realm_id,
        "space_id": &realm_id,
        "exists": exists,
        "external_ref": external_ref,
    })));
}

async fn applet_edge(
    state: &AppletChannelState,
) -> anyhow::Result<Arc<arkret_bridge_runtime::ArkretEdge>> {
    let mut slot = state.edge.lock().await;
    if let Some(edge) = slot.as_ref() {
        return Ok(edge.clone());
    }
    let edge = Arc::new(build_applet_edge(&state.config, state.journal.clone()).await?);
    *slot = Some(edge.clone());
    Ok(edge)
}

async fn build_applet_edge(
    cfg: &savfox_channels::arkret::ArkretAppletConfig,
    journal: arkret_bridge_runtime::AuthoringJournal,
) -> anyhow::Result<arkret_bridge_runtime::ArkretEdge> {
    let key_ref = cfg.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "arkret applet '{}' requires key_ref for signed outbound events",
            cfg.id
        )
    })?;
    cfg.validate()?;
    let verification_method = cfg
        .verification_method
        .clone()
        .context("Applet service verification_method is required")?;
    let verification_method_fragment = verification_method
        .split_once('#')
        .map(|(_, fragment)| fragment)
        .context("Applet service method has no fragment")?;
    let signer_resolution_evidence_ref =
        cfg.signer_resolution_evidence_ref.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "arkret applet '{}' requires signer_resolution_evidence_ref for outbound events",
                cfg.id
            )
        })?;
    let signer = savfox_channels::arkret::load_ed25519_signer(
        key_ref,
        cfg.service_did.as_str(),
        &verification_method,
    )?;
    let http =
        arkret::http_client::Client::builder(url::Url::parse(&cfg.arkret_server_url)?).build()?;
    let trusted_server_did = cfg
        .arkret_server_did
        .clone()
        .ok_or_else(|| anyhow::anyhow!("arkret applet '{}' missing server DID", cfg.id))?;
    let runtime_config: arkret_bridge_runtime::Config = serde_json::from_value(json!({
        "bridge": { "bridge_id": cfg.id },
        "arkret": {
            "server_url": cfg.arkret_server_url,
            "applet_base_url": cfg.base_url,
            "service_did": cfg.service_did,
            "trust_domain": cfg.trust_domain,
            "applet_id": cfg.applet_id,
            "signing_key_seed_hex": savfox_channels::arkret::load_ed25519_seed_hex(key_ref)?,
            "verification_method_fragment": verification_method_fragment,
            "applet_namespaces": cfg.namespaces,
            "managed_actor_authoring": cfg.managed_actor_authoring,
            "signer_resolution_evidence_ref": signer_resolution_evidence_ref,
            "trusted_server_did": trusted_server_did,
        },
        "app": Value::Null,
    }))
    .context("build arkret runtime edge config")?;
    arkret_bridge_runtime::ArkretEdge::new_outbound(Arc::new(runtime_config), http, signer, journal)
        .map_err(|err| anyhow::anyhow!("build arkret outbound edge: {err}"))
}

// ─── Router ─────────────────────────────────────────────────────────────────

/// Routes mounted at `/_arkret/edge/applet/...` (direct).
pub(crate) fn arkret_applet_router() -> Router {
    Router::with_path("_arkret/edge/applet")
        .push(Router::with_path("ping").get(applet_ping))
        .push(Router::with_path("describe").get(applet_describe))
        .push(Router::with_path("transactions").post(applet_transactions))
        .push(
            Router::with_path("managed-actors/author")
                .post(applet_managed_actor_author_unavailable),
        )
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

/// Open the Applet's durable ordinary publication journal. Each Realm/full Actor
/// has its own accepted frontier; pending signed bytes survive edge refresh/restart.
fn build_applet_authoring_journal(
    savfox_home: &std::path::Path,
    config_id: &str,
) -> anyhow::Result<arkret_bridge_runtime::AuthoringJournal> {
    let dir = savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("arkret-applet-authoring");
    // Encode the complete config id reversibly; punctuation replacement would
    // collide between independent Applets such as `a/b` and `a:b`.
    let safe_id: String = config_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = dir.join(format!("{safe_id}.redb"));
    let store = arkret_bridge_runtime::FileAuthoringStore::shared(path)
        .map_err(|e| anyhow::anyhow!("arkret applet authoring journal: {e}"))?;
    Ok(arkret_bridge_runtime::AuthoringJournal::new(store))
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

    // Restore immutable pending publications and accepted Actor frontiers.
    let savfox_home = channel.config().savfox_home.clone();
    let journal = build_applet_authoring_journal(&savfox_home, &applet_cfg.id)?;

    let state = AppletChannelState {
        config: applet_cfg,
        runtime: Mutex::new(AppletRuntimeState::default()),
        journal,
        edge: tokio::sync::Mutex::new(None),
    };
    info!(
        "arkret: applet channel '{}' registered (applet_id={}, service_id={})",
        state.config.id, state.config.applet_id, state.config.service_id
    );
    let config_id = state.config.id.clone();
    register_channel(state)?;
    let registered = lookup_by_config_id(&config_id)?
        .ok_or_else(|| anyhow::anyhow!("registered Applet disappeared"))?;
    let weak = Arc::downgrade(&registered);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            match applet_edge(&state).await {
                Ok(edge) => {
                    if let Err(error) = edge.retry_pending_publications().await {
                        warn!(%error, "Applet publication recovery failed");
                    }
                }
                Err(error) => warn!(%error, "Applet publication transport unavailable"),
            }
        }
    });
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
                "serviceId": "ak:did_core:webvh:z6mkbridge",
                "service_did":"did:webvh:z6mkbridge:bridge.example",
                "trust_domain":"ak:trust_domain:example.net",
                "verification_method":"did:webvh:z6mkbridge:bridge.example#key-1",
                "signer_resolution_evidence_ref": format!("ak:signer_evidence:sha256:{}", "11".repeat(32)),
                "managed_actor_authoring":{"principal_endpoint":"https://actors.example", "key_encryption_key_hex":"22".repeat(32)},
                "controllerPrincipalId": "ak:did_core:webvh:zAdminScid",
                "baseUrl": "https://savfox.example/applet-test",
                "bot_account_id": {"principal_id":"ak:did_core:web:bridge.example:bot", "station_id":"ak:did_core:webvh:z6mkstation"},
                "arkretServerUrl": "https://arkret.example.org",
                "arkretServerDid": "did:webvh:z6mkstation:arkret.example.org",
                                "keyRef": {"kind": "env", "var": "SAVFOX_ARKRET_APPLET_TEST_KEY"},
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
            verification_method: "did:webvh:z6mkstation:arkret.example.org#key-1".to_owned(),
            public_key: PublicKeyMaterial::Ed25519Raw { bytes: public_key },
        }];
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        AppletChannelState {
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            journal: build_applet_authoring_journal(tmp.path(), &applet.id)
                .expect("authoring journal"),
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
             \"arkret-operation\" \"source-service-id\" \"destination-service-id\" \
             \"content-digest\" \"idempotency-key\");created={now};expires={};\
             keyid=\"did:webvh:z6mkstation:arkret.example.org#key-1\";alg=\"ed25519\"",
            now + 300
        );
        let mut headers = vec![
            ("host".to_owned(), "savfox.example".to_owned()),
            (
                "arkret-operation".to_owned(),
                "ak.edge.applet.command.transaction.v1".to_owned(),
            ),
            (
                SOURCE_SERVICE_ID_HEADER.to_owned(),
                "ak:did_core:webvh:z6mkstation".to_owned(),
            ),
            (
                DESTINATION_SERVICE_ID_HEADER.to_owned(),
                "ak:did_core:webvh:z6mkbridge".to_owned(),
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
            Component::Header("arkret-operation".to_owned()),
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
            journal: build_applet_authoring_journal(tmp.path(), &applet.id)
                .expect("authoring journal"),
            edge: tokio::sync::Mutex::new(None),
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
            "source_id": "ak:did_core:webvh:z6mkstation",
            "applet_id": "ak:applet:21532600-0000-7000-8000-000000000000",
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
        assert_eq!(verified.source_service_id, "ak:did_core:webvh:z6mkstation");
        assert_eq!(
            verified.destination_service_id,
            "ak:did_core:webvh:z6mkbridge"
        );
        assert_eq!(verified.signature_label, "sig1");
        assert_eq!(
            verified.key_id,
            "did:webvh:z6mkstation:arkret.example.org#key-1"
        );
        assert_eq!(verified.signature_algorithm, "ed25519");
        assert!(verified.verification_key_digest.starts_with("sha256:"));
        assert!(verified.content_digest.starts_with("sha-256=:"));
    }

    #[test]
    fn rejects_missing_or_cross_operation_http_signature() {
        let body = b"{}";
        let (headers, public_key) = signed_transaction_headers(body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        for operation in [None, Some("ak.self.applet.bot.command.provision.v1")] {
            let mut invalid = headers.clone();
            invalid.retain(|(name, _)| name != "arkret-operation");
            if let Some(operation) = operation {
                invalid.push(("arkret-operation".to_owned(), operation.to_owned()));
            }
            let error = verify_applet_transaction_http_signature(
                &state,
                "POST",
                Some("https://savfox.example/_arkret/edge/applet/transactions"),
                Some("savfox.example"),
                "/_arkret/edge/applet/transactions",
                &invalid,
                body,
            )
            .expect_err("a transaction must bind its exact operation");
            assert!(error.to_string().contains("Arkret-Operation"));
        }
    }

    #[test]
    fn rejects_tampered_http_message_signature_body() {
        let body = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_id": "ak:did_core:webvh:z6mkstation",
            "applet_id": "ak:applet:21532600-0000-7000-8000-000000000000",
            "events": []
        }))
        .expect("body should serialize");
        let (headers, public_key) = signed_transaction_headers(&body, [9u8; 32]);
        let state = state_with_trusted_http_signature_key(public_key);
        let tampered = serde_json::to_vec(&json!({
            "transaction_id": "txn-1",
            "source_id": "ak:did_core:webvh:z6mkstation",
            "applet_id": "ak:applet:21532600-0000-7000-8000-000000000000",
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

    #[tokio::test]
    async fn authoring_journal_reopens_without_reserving_actor_positions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let journal = build_applet_authoring_journal(tmp.path(), "applet-test").expect("journal");
        assert!(journal.pending_events().await.unwrap().is_empty());
        drop(journal);
        assert!(
            build_applet_authoring_journal(tmp.path(), "applet-test")
                .unwrap()
                .pending_events()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
