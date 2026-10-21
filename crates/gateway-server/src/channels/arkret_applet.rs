//! Arkret Applet Service management endpoints.
//!
//! HTTPS discovery and Station-signed authoring completion are independent of
//! group content delivery. Group Events and Signals require a native managed
//! Device; management transactions never dispatch them to a model.
//! Managed Principal custody and authoring use the shared bridge runtime.
//!
//! Direct routes use `/_arkret/edge/applet/...`; per-configuration routes use
//! `/appservices/arkret/{config_id}/_arkret/edge/applet/...`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
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

/// Short-lived coordination of simultaneous deliveries. The shared durable
/// authoring journal retains immutable creation, full delivery authentication
/// records and the fixed accepted outcome across process restarts.
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
    render_completion_problem(res, StatusCode::UNAUTHORIZED, code, message);
}

fn render_completion_problem(
    res: &mut Response,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) {
    let problem = arkret::Problem::from_code(code, message)
        .with_status(status.as_u16())
        .with_instance(arkret::new_prefixed_uuid7("ak:request:"));
    let mut output = salvo::http::Problem::new(status)
        .kind(problem.problem_type)
        .title(problem.title)
        .detail(problem.detail)
        .with_extensions(
            problem
                .extensions
                .into_iter()
                .collect::<serde_json::Map<_, _>>(),
        );
    if let Some(instance) = problem.instance {
        output = output.instance(instance);
    }
    res.render(output);
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
async fn applet_managed_actor_author(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let edge = match applet_edge(&state).await {
        Ok(edge) => edge,
        Err(error) => {
            warn!(%error, "Applet authoring transport initialization failed");
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Managed identity authoring is unavailable",
            );
            return;
        }
    };
    let author = arkret_bridge_runtime::run_blocking(move || {
        arkret_bridge_runtime::managed_actor_authoring::ManagedActorAuthoringService::new(edge)
    })
    .await;
    match author {
        Ok(author) => {
            let body = match req
                .parse_json::<arkret::AppletManagedActorAuthorRequestBody>()
                .await
            {
                Ok(body) => body,
                Err(_) => {
                    render_error(
                        res,
                        StatusCode::BAD_REQUEST,
                        "schema_violation",
                        "Invalid managed actor author request",
                    );
                    return;
                }
            };
            match author.author(&body.authoring_request).await {
                Ok(create_event) => res.render(Json(arkret::AppletManagedActorAuthorOutcome {
                    create_event,
                })),
                Err(error) => {
                    let status = match &error {
                        arkret_bridge_runtime::BridgeError::App(message)
                            if message.contains("expired") =>
                        {
                            StatusCode::GONE
                        }
                        arkret_bridge_runtime::BridgeError::App(_)
                        | arkret_bridge_runtime::BridgeError::Arkret(_) => StatusCode::BAD_REQUEST,
                        _ => StatusCode::INTERNAL_SERVER_ERROR,
                    };
                    warn!(%error, "Managed actor authoring failed");
                    render_error(
                        res,
                        status,
                        "managed_actor_authoring_failed",
                        "Managed identity authoring failed",
                    );
                }
            }
        }
        Err(error) => {
            warn!(%error, "Applet authoring custody initialization failed");
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Managed identity custody is unavailable",
            );
        }
    }
}

#[handler]
async fn applet_transactions(req: &mut Request, _depot: &mut Depot, res: &mut Response) {
    let state = match select_applet_management_target(req) {
        Ok(Some(state)) => state,
        Ok(None) => {
            render_completion_problem(
                res,
                StatusCode::NOT_FOUND,
                "not_found",
                "Applet management target is unconfigured",
            );
            return;
        }
        Err(error) => {
            warn!("applet transaction state unavailable: {error:#}");
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Applet transaction state unavailable",
            );
            return;
        }
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
            render_completion_problem(
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
            render_completion_problem(
                res,
                StatusCode::BAD_REQUEST,
                "json_invalid",
                format!("failed to read AppletTransactionRequestBody: {err}"),
            );
            return;
        }
    };

    if let Err(error) = arkret_bridge_runtime::precheck_applet_completion_envelope(
        &request_headers,
        body_bytes.as_ref(),
    ) {
        render_unauthorized(res, error.code, error.message);
        return;
    }
    if header_value_from(&request_headers, "idempotency-key").is_none() {
        render_unauthorized(
            res,
            "http_signature_invalid",
            "transaction signature requires Idempotency-Key",
        );
        return;
    }
    let body: AppletTransactionRequestBody = match serde_json::from_slice(body_bytes) {
        Ok(body) => body,
        Err(err) => {
            render_completion_problem(
                res,
                StatusCode::UNPROCESSABLE_ENTITY,
                "schema_violation",
                format!("invalid AppletTransactionRequestBody: {err}"),
            );
            return;
        }
    };

    if let Err(error) = body.validate() {
        render_completion_problem(
            res,
            StatusCode::UNPROCESSABLE_ENTITY,
            "schema_violation",
            error.to_string(),
        );
        return;
    }
    let AppletTransactionRequestBody::Authoring(completion) = &body else {
        render_completion_problem(
            res,
            StatusCode::FORBIDDEN,
            "capability_denied",
            "Station-to-Applet Event and Signal subscriptions are prohibited; use an authorized managed Device",
        );
        return;
    };
    let edge = match applet_edge(&state).await {
        Ok(edge) => edge,
        Err(error) => {
            warn!("applet transaction state unavailable: {error:#}");
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Applet transaction state unavailable",
            );
            return;
        }
    };
    let (Some(authority), Some(target_uri)) = (signature_authority, signature_target_uri) else {
        render_unauthorized(
            res,
            "http_signature_invalid",
            "request authority/target URI is required",
        );
        return;
    };
    let request = arkret::http_signature::SignedRequestParts {
        method: signature_method,
        target_uri,
        authority,
        path: signature_path,
        headers: request_headers,
        body_digest: None,
    };
    // This call always precedes the cache. The shared runtime independently
    // authenticates current governance/HTTP key and current manager, the
    // original Service signer epoch, and the still-live exact material.
    let verified_completion = match edge
        .admit_managed_authoring_delivery(&request, body_bytes.as_ref(), completion)
        .await
    {
        Ok(completion) => completion,
        Err(error) => {
            let status = if error.code == "duplicate_conflict" {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNAUTHORIZED
            };
            render_completion_problem(res, status, error.code, error.message);
            return;
        }
    };
    let Some(record) = verified_completion.delivery_authentication_record() else {
        render_completion_problem(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "completion admission returned no authentication record",
        );
        return;
    };
    let body_hash = match canonical::canonical_sha256(&body)
        .map_err(|error| error.to_string())
        .and_then(|hash| Hash::new(hash).map_err(|error| error.to_string()))
    {
        Ok(hash) => hash,
        Err(error) => {
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error.to_string(),
            );
            return;
        }
    };
    let identity = IdempotencyIdentity::applet_transaction(
        IdempotencyDirection::NodeToApplet,
        record.source_id.clone(),
        record.destination_id.clone(),
        record.idempotency_key.clone(),
    );
    let delivery_authentication_record_digest = match record.stable_digest() {
        Ok(digest) => digest.to_string(),
        Err(error) => {
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error,
            );
            return;
        }
    };

    if let Some(outcome) = verified_completion.retained_outcome().cloned() {
        if let Err(error) = edge.install_verified_managed_completion(verified_completion) {
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error.to_string(),
            );
            return;
        }
        res.status_code(StatusCode::OK);
        res.render(Json(outcome));
        return;
    }

    // Coordinate simultaneous first deliveries before installing the original.
    // The journal's terminal receipt above provides durable replay recovery.
    {
        let runtime_state = if let Ok(runtime_state) = state.runtime.lock() {
            runtime_state
        } else {
            render_completion_problem(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
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
                if let Err(error) = edge.install_verified_managed_completion(verified_completion) {
                    render_completion_problem(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        error.to_string(),
                    );
                    return;
                }
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
                render_completion_problem(
                    res,
                    StatusCode::CONFLICT,
                    "duplicate_conflict",
                    "Idempotency-Key already used for a different request body or delivery authentication record digest",
                );
                return;
            }
            IdempotencyClaim::InFlight { .. } => {
                render_completion_problem(
                    res,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily_unavailable",
                    "duplicate transaction is still being processed; retry to receive the outcome",
                );
                return;
            }
        }
    }

    if let Err(error) = edge.install_verified_managed_completion(verified_completion) {
        release_transaction_claim(&state, &identity);
        render_completion_problem(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            error.to_string(),
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
            "trusted_verification_methods": cfg.trusted_verification_methods.iter().map(|method| json!({
                "verification_method": method.verification_method,
                "public_key": method.public_key,
            })).collect::<Vec<_>>(),
        },
        "app": Value::Null,
    }))
    .context("build arkret runtime edge config")?;
    arkret_bridge_runtime::ArkretEdge::new_outbound(Arc::new(runtime_config), http, signer, journal)
        .map_err(|err| anyhow::anyhow!("build arkret outbound edge: {err}"))
}

/// Publish native Principal resources from the configured hosts' durable custody.
/// No management transaction, private key, or group content is returned here.
async fn applet_principal_resource(req: &Request, res: &mut Response, history: bool) {
    let local_id = req.param::<String>("local_id").unwrap_or_default();
    let states = match applet_registry().lock() {
        Ok(registry) => registry.values().cloned().collect::<Vec<_>>(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };
    let mut found = None;
    let mut unavailable = false;
    for state in states {
        let edge = match applet_edge(&state).await {
            Ok(edge) => edge,
            Err(error) => {
                warn!(%error,"Principal publication transport unavailable");
                unavailable = true;
                continue;
            }
        };
        let author =
            match arkret_bridge_runtime::managed_actor_authoring::ManagedActorAuthoringService::new(
                edge,
            ) {
                Ok(author) => author,
                Err(error) => {
                    warn!(%error,"Principal publication custody unavailable");
                    unavailable = true;
                    continue;
                }
            };
        match author.published_principal(&local_id).await {
            Ok(Some(material)) => {
                if found.is_some() {
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return;
                }
                found = Some(material);
            }
            Ok(None) => {}
            Err(error) => {
                warn!(%error,"Principal publication evidence unavailable");
                unavailable = true;
            }
        }
    }
    let Some((document, entries)) = found else {
        res.status_code(if unavailable {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NOT_FOUND
        });
        return;
    };
    res.headers_mut().insert(
        "cache-control",
        "no-store".parse().expect("static cache policy"),
    );
    if history {
        let mut body = Vec::new();
        for entry in entries {
            match canonical::canonical_json_bytes(&entry) {
                Ok(bytes) => {
                    body.extend(bytes);
                    body.push(b'\n');
                }
                Err(_) => {
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return;
                }
            }
        }
        res.headers_mut().insert(
            "content-type",
            "application/jsonl".parse().expect("static content type"),
        );
        if let Err(error) = res.write_body(body) {
            warn!(%error,"Principal native history response write failed");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        }
    } else {
        res.render(Json(document));
    }
}

#[handler]
async fn applet_principal_document(req: &mut Request, res: &mut Response) {
    applet_principal_resource(req, res, false).await;
}
#[handler]
async fn applet_principal_history(req: &mut Request, res: &mut Response) {
    applet_principal_resource(req, res, true).await;
}

/// Native DID-method resources use their canonical paths, outside the management tree.
pub(crate) fn arkret_principal_router() -> Router {
    Router::with_path("webvh/{local_id}")
        .push(Router::with_path("did.json").get(applet_principal_document))
        .push(Router::with_path("did.jsonl").get(applet_principal_history))
}

// ─── Router ─────────────────────────────────────────────────────────────────

/// Routes mounted at `/_arkret/edge/applet/...` (direct).
pub(crate) fn arkret_applet_router() -> Router {
    Router::with_path("_arkret/edge/applet")
        .push(Router::with_path("ping").get(applet_ping))
        .push(Router::with_path("describe").get(applet_describe))
        .push(Router::with_path("transactions").post(applet_transactions))
        .push(Router::with_path("managed-actors/author").post(applet_managed_actor_author))
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
        let mut device_workers = None;
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
                    if device_workers.is_none()
                        && state.config.managed_actor_authoring.device.is_some()
                    {
                        match arkret_bridge_runtime::managed_device::ManagedDeviceWorkers::new(
                            edge.clone(),
                        ) {
                            Ok(workers) => device_workers = Some(workers),
                            Err(error) => warn!(%error, "Applet native Device workers unavailable"),
                        }
                    }
                    if let Some(workers) = device_workers.as_mut() {
                        if let Err(error) = workers.tick().await {
                            warn!(%error, "Applet native Device recovery failed");
                        }
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

    #[tokio::test]
    async fn transaction_route_rejects_group_content_even_with_a_matching_cached_outcome() {
        use salvo::test::{ResponseExt, TestClient};
        let mut channel = valid_channel_config();
        channel.id = "applet-group-cache-rejection".into();
        let applet = ArkretAppletConfig::from_channel_config(&channel).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let state = AppletChannelState {
            journal: build_applet_authoring_journal(temporary.path(), &applet.id).unwrap(),
            config: applet.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            edge: tokio::sync::Mutex::new(None),
        };
        let event = arkret_wire::test_support::raw_event(
            "ak.message.create",
            arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new(
                    "ak:realm:AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                )
                .unwrap(),
            },
            DidCoreId::new("ak:did_core:webvh:zsender").unwrap(),
            DidCoreId::new("ak:did_core:webvh:z6mkstation").unwrap(),
            json!({"body":"no group subscription"}),
        )
        .unwrap();
        let body =
            AppletTransactionRequestBody::Events(arkret::AppletEventTransactionRequestBody {
                applet_id: AppletId::new(applet.applet_id.clone()).unwrap(),
                source_id: DidCoreId::new("ak:did_core:webvh:z6mkstation").unwrap(),
                events: vec![event],
                committed_events: Vec::new(),
                signals: Vec::new(),
            });
        let identity = IdempotencyIdentity::applet_transaction(
            IdempotencyDirection::NodeToApplet,
            body.source_id().clone(),
            DidCoreId::new(applet.service_id.clone()).unwrap(),
            "cached-group",
        );
        let hash = Hash::new(canonical::canonical_sha256(&body).unwrap()).unwrap();
        {
            let runtime = state.runtime.lock().unwrap();
            assert!(matches!(
                runtime.txn_dedupe.claim(&identity, &hash, "prior"),
                IdempotencyClaim::Fresh
            ));
            assert!(runtime.txn_dedupe.complete(
                &identity,
                AppletTransactionOutcome::Accepted {
                    committed_event_refs: Vec::new(),
                    rejections: Vec::new(),
                    retry_after_ms: None,
                }
            ));
        }
        register_channel(state).unwrap();
        let service = salvo::Service::new(arkret_appservices_router());
        let url = format!(
            "https://savfox.example/appservices/arkret/{}/_arkret/edge/applet/transactions",
            applet.id
        );
        let raw = canonical::canonical_json_bytes(&body).unwrap();
        let digest = arkret::http_signature::ContentDigest::compute(
            &raw,
            arkret::http_signature::ContentDigestAlgorithm::Sha256,
        );
        let mut parts = arkret::http_signature::SignedRequestParts {
            method: "POST".into(),
            target_uri: url.clone(),
            authority: "savfox.example".into(),
            path: format!(
                "/appservices/arkret/{}/_arkret/edge/applet/transactions",
                applet.id
            ),
            headers: vec![
                (
                    "arkret-operation".into(),
                    ServiceOperationId::EDGE_APPLET_COMMAND_TRANSACTION_V1.into(),
                ),
                ("source-service-id".into(), body.source_id().to_string()),
                ("destination-service-id".into(), applet.service_id.clone()),
                ("content-digest".into(), digest.wire_value.clone()),
                ("idempotency-key".into(), "cached-group".into()),
            ],
            body_digest: Some(digest.wire_value),
        };
        let key = arkret::http_signature::signing_key_from_seed(&[0x62; 32]);
        let signed = arkret::http_signature::sign_http_message_for_scenario(
            &parts,
            arkret::http_signature::HttpSignatureScenario::AppletTransactionV1,
            &[],
            "sig1",
            "did:webvh:z6mkstation:station.example#key-1",
            chrono::Utc::now().timestamp(),
            &key,
        )
        .unwrap();
        parts
            .headers
            .push(("signature-input".into(), signed.signature_input_header));
        parts
            .headers
            .push(("signature".into(), signed.signature_header));
        let mut request = TestClient::post(&url).body(raw);
        for (name, value) in parts.headers {
            request = request.add_header(
                salvo::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value,
                true,
            );
        }
        let mut response = request.send(&service).await;
        assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
        assert!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/problem+json"))
        );
        let problem: arkret::Problem = response.take_json().await.unwrap();
        assert_eq!(
            problem.problem_type,
            "https://arkret.org/problems/capability_denied"
        );
        assert_eq!(problem.code(), "capability_denied");
        remove_arkret_applet_channel(&applet.id).unwrap();
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

    #[tokio::test]
    async fn completion_route_prioritizes_exact_digest_and_canonical_wire_without_opening_edge() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use arkret::http_signature::{ContentDigest, ContentDigestAlgorithm};
        use salvo::test::{ResponseExt, TestClient};
        struct UntouchedJournal(AtomicUsize);
        impl arkret_bridge_runtime::AuthoringStore for UntouchedJournal {
            fn load(&self, _: &str) -> arkret_bridge_runtime::Result<Option<String>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
            fn compare_exchange_batch(
                &self,
                _: &[(String, Option<String>)],
                _: &[(String, Option<String>)],
            ) -> arkret_bridge_runtime::Result<bool> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }
            fn entries_with_prefix(
                &self,
                _: &str,
            ) -> arkret_bridge_runtime::Result<Vec<(String, String)>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
        }
        let mut channel = valid_channel_config();
        channel.id = "applet-envelope-priority".into();
        let config = ArkretAppletConfig::from_channel_config(&channel).unwrap();
        let store = Arc::new(UntouchedJournal(AtomicUsize::new(0)));
        register_channel(AppletChannelState {
            config: config.clone(),
            runtime: Mutex::new(AppletRuntimeState::default()),
            journal: arkret_bridge_runtime::AuthoringJournal::new(store.clone()),
            edge: tokio::sync::Mutex::new(None),
        })
        .unwrap();
        let state = lookup_by_config_id(&config.id).unwrap().unwrap();
        let service = salvo::Service::new(arkret_appservices_router());
        let url = format!(
            "https://savfox.example/appservices/arkret/{}/_arkret/edge/applet/transactions",
            config.id
        );
        for (label, body, mutation, code) in [
            ("all-missing", "{invalid", "all", "http_signature_required"),
            (
                "signature-missing",
                "{}",
                "signature",
                "http_signature_required",
            ),
            (
                "signature-input-missing",
                "{}",
                "signature-input",
                "http_signature_required",
            ),
            (
                "only-digest-missing",
                "{invalid",
                "content-digest",
                "http_signature_invalid",
            ),
            (
                "wrong-exact-digest",
                "{invalid",
                "wrong-digest",
                "http_signature_invalid",
            ),
            ("malformed-json", "{invalid", "", "http_signature_invalid"),
            ("noncanonical-json", "{ }", "", "http_signature_invalid"),
            (
                "source-missing",
                "{}",
                "source-service-id",
                "http_signature_invalid",
            ),
            (
                "destination-missing",
                "{}",
                "destination-service-id",
                "http_signature_invalid",
            ),
            (
                "idempotency-missing",
                "{}",
                "idempotency-key",
                "http_signature_invalid",
            ),
        ] {
            let parsed = url::Url::parse(&url).unwrap();
            let authority = parsed.host_str().unwrap().to_owned();
            let digest = ContentDigest::compute(body.as_bytes(), ContentDigestAlgorithm::Sha256);
            let mut parts = arkret::http_signature::SignedRequestParts {
                method: "POST".into(),
                target_uri: url.clone(),
                authority,
                path: parsed.path().into(),
                headers: vec![
                    (
                        "arkret-operation".into(),
                        ServiceOperationId::EDGE_APPLET_COMMAND_TRANSACTION_V1.into(),
                    ),
                    (
                        "source-service-id".into(),
                        "ak:did_core:webvh:z6mkstation".into(),
                    ),
                    (
                        "destination-service-id".into(),
                        "ak:did_core:webvh:z6mkbridge".into(),
                    ),
                    ("idempotency-key".into(), "envelope-before-json".into()),
                    ("content-digest".into(), digest.wire_value.clone()),
                ],
                body_digest: Some(digest.wire_value),
            };
            let signed = arkret::http_signature::sign_http_message_for_scenario(
                &parts,
                arkret::http_signature::HttpSignatureScenario::AppletTransactionV1,
                &[],
                "sig1",
                "did:webvh:z6mkstation:station.example#key-1",
                chrono::Utc::now().timestamp(),
                &arkret::http_signature::signing_key_from_seed(&[0x62; 32]),
            )
            .unwrap();
            parts
                .headers
                .push(("signature-input".into(), signed.signature_input_header));
            parts
                .headers
                .push(("signature".into(), signed.signature_header));
            if mutation == "all" {
                parts.headers.retain(|(name, _)| name == "arkret-operation");
            } else if mutation == "wrong-digest" {
                parts.headers.retain(|(name, _)| name != "content-digest");
                parts.headers.push((
                    "content-digest".into(),
                    ContentDigest::compute(b"{}", ContentDigestAlgorithm::Sha256).wire_value,
                ));
            } else if !mutation.is_empty() {
                parts.headers.retain(|(name, _)| name != mutation);
            }
            let mut request = TestClient::post(&url).body(body);
            for (name, value) in parts.headers {
                request = request.add_header(
                    salvo::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    value,
                    true,
                );
            }
            let mut response = request.send(&service).await;
            assert_eq!(
                response.status_code,
                Some(StatusCode::UNAUTHORIZED),
                "{label}"
            );
            assert!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("application/problem+json"))
            );
            let problem: arkret::Problem = response.take_json().await.unwrap();
            assert_eq!(
                problem.problem_type,
                format!("https://arkret.org/problems/{code}")
            );
            assert_eq!(problem.code(), code);
            if mutation == "content-digest" || mutation == "wrong-digest" {
                assert!(
                    problem.detail.to_ascii_lowercase().contains("digest"),
                    "Digest failure must precede malformed business JSON: {label}: {}",
                    problem.detail
                );
            }
            assert_eq!(problem.status, StatusCode::UNAUTHORIZED.as_u16());
            assert!(
                state.edge.try_lock().unwrap().is_none(),
                "no authority/network initialization"
            );
            assert_eq!(
                store.0.load(Ordering::SeqCst),
                0,
                "no local journal read or write"
            );
        }
        remove_arkret_applet_channel(&config.id).unwrap();
    }
}
