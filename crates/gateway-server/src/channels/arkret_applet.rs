//! Arkret Applet Service management endpoints.
//!
//! HTTPS discovery and Station-signed authoring completion are independent of
//! group content delivery. Group Events and Signals require a native managed
//! Device; management transactions never dispatch them to a model.
//! Managed Principal custody and authoring use the shared bridge runtime.
//!
//! Routes use the canonical `/_arkret/edge/applet/...` paths.
//! The signed destination Service header selects the configured Applet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use arkret::{
    AppletActorView, AppletId, AppletPingOutcome, AppletProtocolMetadata, AppletRealmView,
    DidCoreId, ServiceDescribe, ServiceKind, TransportBinding, canonical,
};
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::arkret::AppletNamespacesExt;
use savfox_channels::arkret::applet::{ArkretAppletConfig, load_arkret_applet_configs};
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use super::render_error;
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

struct AppletChannelState {
    config: ArkretAppletConfig,
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
        "ambiguous Applet management target; provide Destination-Service-Id"
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
async fn applet_transactions(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    let body = match req
        .payload_with_max_size(arkret_bridge_runtime::web::MAX_APPLET_TRANSACTION_BODY_BYTES)
        .await
    {
        Ok(body) => body.to_vec(),
        Err(salvo::http::ParseError::PayloadTooLarge) => {
            render_error(
                res,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Applet completion body exceeds its limit",
            );
            return;
        }
        Err(_) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "schema_violation",
                "Applet completion body could not be read",
            );
            return;
        }
    };
    let receiver = async {
        let edge = applet_edge(&state).await?;
        arkret_bridge_runtime::web::receive_managed_completion_http(
            edge,
            req.method().clone(),
            req.uri().clone(),
            req.headers().clone(),
            body,
        )
        .await
        .map_err(anyhow::Error::from)
    }
    .await;
    match receiver {
        Ok((status, headers, body)) => {
            res.status_code(status);
            *res.headers_mut() = headers;
            res.body(body);
        }
        Err(error) => {
            warn!(%error, "Applet completion receiver unavailable");
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Applet completion receiver unavailable",
            );
        }
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
        .map(|(_, fragment)| format!("#{fragment}"))
        .context("Applet service method has no fragment")?;
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
                    if let Err(error) = arkret_bridge_runtime::managed_device::ManagedDeviceRuntime::bootstrap_if_configured(edge.clone()).await {
                        warn!(%error, "Applet native Device recovery failed");
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
                "managed_actor_authoring":{"principal_endpoint":"https://actors.example", "key_encryption_key_hex":"22".repeat(32)},
                "controllerPrincipalId": "ak:did_core:webvh:zAdminScid",
                "baseUrl": "https://savfox.example/applet-test",
                "arkretServerUrl": "https://arkret.example.org",
                "arkretServerDid": "did:webvh:z6mkstation:arkret.example.org",
                                "keyRef": {"kind": "env", "var": "SAVFOX_ARKRET_APPLET_TEST_KEY"},
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
        let applet = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        applet.validate().expect("validate");
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppletChannelState {
            config: applet.clone(),
            journal: build_applet_authoring_journal(tmp.path(), &applet.id)
                .expect("authoring journal"),
            edge: tokio::sync::Mutex::new(None),
        };
        register_channel(state).expect("register");
        let resolved = lookup_by_config_id(&applet.id)
            .expect("lookup")
            .expect("registered");
        assert_eq!(resolved.config.applet_id, applet.applet_id);
        remove_arkret_applet_channel(&applet.id).unwrap();
    }

    #[tokio::test]
    async fn completion_route_preserves_shared_selector_and_body_limits() {
        use salvo::test::{ResponseExt as _, TestClient};
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("service-key");
        std::fs::write(&key, [42u8; 32]).unwrap();
        let mut cfg = valid_channel_config();
        cfg.id = "completion-transport".into();
        cfg.config["serviceId"] = json!("ak:did_core:webvh:z6mkcompletion");
        cfg.config["service_did"] = json!("did:webvh:z6mkcompletion:bridge.example");
        cfg.config["verification_method"] = json!("did:webvh:z6mkcompletion:bridge.example#key-1");
        cfg.config["keyRef"] = json!({"kind":"file","path":key});
        let applet = ArkretAppletConfig::from_channel_config(&cfg).unwrap();
        applet.validate().unwrap();
        let journal = build_applet_authoring_journal(tmp.path(), &applet.id).unwrap();
        let edge = Arc::new(build_applet_edge(&applet, journal.clone()).await.unwrap());
        register_channel(AppletChannelState {
            journal,
            config: applet.clone(),
            edge: tokio::sync::Mutex::new(Some(edge)),
        })
        .unwrap();
        let service = Service::new(arkret_applet_router());
        let mut response =
            TestClient::post("https://savfox.example/_arkret/edge/applet/transactions")
                .add_header("Destination-Service-Id", &applet.service_id, true)
                .body("not-json")
                .send(&service)
                .await;
        assert_eq!(response.status_code, Some(StatusCode::BAD_REQUEST));
        let problem: arkret::Problem = response.take_json().await.unwrap();
        assert_eq!(problem.code(), "operation_selector_required");
        let response = TestClient::post("https://savfox.example/_arkret/edge/applet/transactions")
            .add_header("Destination-Service-Id", &applet.service_id, true)
            .add_header(
                "Arkret-Operation",
                "ak.edge.applet.command.transaction.v1",
                true,
            )
            .body(vec![
                b' ';
                arkret_bridge_runtime::web::MAX_APPLET_TRANSACTION_BODY_BYTES
                    + 1
            ])
            .send(&service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::PAYLOAD_TOO_LARGE));
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
}
