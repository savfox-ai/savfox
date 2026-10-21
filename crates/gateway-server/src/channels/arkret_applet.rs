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

use anyhow::Context as _;
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::arkret::applet::{ArkretAppletConfig, load_arkret_applet_configs};
use serde_json::{Value, json};
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
    host: tokio::sync::OnceCell<Arc<arkret_bridge_runtime::web::ManagedAppletHttpHost>>,
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
    anyhow::ensure!(
        reg.values().all(|old| old.config.id == state.config.id
            || old.config.service_id != state.config.service_id),
        "Applet Service is already registered by another channel"
    );
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

async fn applet_host(
    state: &AppletChannelState,
) -> anyhow::Result<&Arc<arkret_bridge_runtime::web::ManagedAppletHttpHost>> {
    state
        .host
        .get_or_try_init(|| async {
            let edge = applet_edge(state).await?;
            arkret_bridge_runtime::run_blocking(move || {
                arkret_bridge_runtime::web::ManagedAppletHttpHost::new(edge).map(Arc::new)
            })
            .await
            .map_err(anyhow::Error::from)
        })
        .await
}

async fn forward_applet(state: &AppletChannelState, req: &mut Request, res: &mut Response) {
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
                "Applet request body exceeds its limit",
            );
            return;
        }
        Err(_) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "schema_violation",
                "Applet request body could not be read",
            );
            return;
        }
    };
    let result = async {
        applet_host(state)
            .await?
            .receive(
                req.method().clone(),
                req.uri().clone(),
                req.headers().clone(),
                body,
            )
            .await
            .map_err(anyhow::Error::from)
    }
    .await;
    match result {
        Ok((status, headers, body)) => {
            res.status_code(status);
            *res.headers_mut() = headers;
            res.body(body);
        }
        Err(error) => {
            warn!(%error, "Applet shared HTTP host unavailable");
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Applet HTTP host unavailable",
            );
        }
    }
}

#[handler]
async fn applet_request(req: &mut Request, res: &mut Response) {
    let Some(state) = resolve_public_applet(req, res) else {
        return;
    };
    forward_applet(&state, req, res).await;
}

#[handler]
async fn applet_service_resolution(req: &mut Request, res: &mut Response) {
    let target = req.param::<String>("service_id");
    let state = applet_registry().lock().ok().and_then(|registry| {
        registry
            .values()
            .find(|state| Some(&state.config.service_id) == target.as_ref())
            .cloned()
    });
    if let Some(state) = state {
        forward_applet(&state, req, res).await;
    } else {
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "not_found",
            "Applet Service is unconfigured",
        );
    }
}

fn is_native_resource_path(path: &str) -> bool {
    if path.starts_with("/webvh/") && (path.ends_with("/did.json") || path.ends_with("/did.jsonl"))
    {
        return true;
    }
    applet_registry().lock().is_ok_and(|registry| {
        registry.values().any(|state| {
            let did = &state.config.service_did;
            [
                arkret::identity::DidWebvhResolver::document_url(did),
                arkret::identity::DidWebvhResolver::log_url(did),
                arkret::identity::DidWebvhResolver::witness_url(did),
            ]
            .into_iter()
            .filter_map(Result::ok)
            .filter_map(|url| url::Url::parse(&url).ok())
            .any(|url| url.path() == path)
        })
    })
}

/// Try only the configured native publication surfaces, never a remote resolver.
#[handler]
async fn applet_native_resource(req: &mut Request, res: &mut Response) {
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
        let result = async {
            applet_host(&state)
                .await?
                .receive(
                    req.method().clone(),
                    req.uri().clone(),
                    req.headers().clone(),
                    Vec::new(),
                )
                .await
                .map_err(anyhow::Error::from)
        }
        .await;
        match result {
            Ok((StatusCode::NOT_FOUND, ..)) => {}
            Ok(response) if response.0.is_success() => {
                if found.is_some() {
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return;
                }
                found = Some(response);
            }
            Ok(_) | Err(_) => unavailable = true,
        }
    }
    if let Some((status, headers, body)) = found {
        res.status_code(status);
        *res.headers_mut() = headers;
        res.body(body);
    } else {
        res.status_code(if unavailable {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NOT_FOUND
        });
    }
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
            "service_resolution": serde_json::from_slice::<arkret::AuthenticatedServiceResolution>(&tokio::fs::read(&cfg.service_resolution_path).await.context("read Applet service_resolution_path")?)?,
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

/// Native DID resources and canonical Service discovery share the verified host.
pub(crate) fn arkret_principal_router() -> Router {
    Router::new()
        .push(Router::with_path("_arkret/describe").get(applet_request))
        .push(
            Router::with_path("_arkret/open/services/{service_id}/resolution")
                .get(applet_service_resolution),
        )
        .push(
            Router::with_path("{**resource}")
                .filter_fn(|req, _| is_native_resource_path(req.uri().path()))
                .get(applet_native_resource),
        )
}

pub(crate) fn arkret_applet_router() -> Router {
    Router::with_path("_arkret/edge/applet/{**operation}")
        .get(applet_request)
        .post(applet_request)
}

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
        host: tokio::sync::OnceCell::new(),
    };
    applet_host(&state)
        .await
        .context("initialize verified Applet HTTP host")?;
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
                "service_resolution_path":"service-resolution.json",
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
            host: tokio::sync::OnceCell::new(),
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
        let now = chrono::Utc::now();
        let mut identity = arkret_test_kit::service_identity::ServiceIdentityFixture::webvh(
            &"https://savfox.example/service/".parse().unwrap(),
            812,
            [42u8; 32],
            "key-1",
            now - chrono::Duration::minutes(1),
            now,
        )
        .unwrap();
        identity.resolution.service_kind = "applet_service".into();
        let resolution = tmp.path().join("resolution.json");
        std::fs::write(
            &resolution,
            serde_json::to_vec(&identity.resolution).unwrap(),
        )
        .unwrap();
        cfg.config["serviceId"] = json!(identity.service_id);
        cfg.config["service_did"] = json!(identity.resolution.normalized_did_document.id);
        cfg.config["verification_method"] = json!(format!(
            "{}#key-1",
            identity.resolution.normalized_did_document.id
        ));
        cfg.config["service_resolution_path"] = json!(resolution);
        cfg.config["keyRef"] = json!({"kind":"file","path":key});
        let applet = ArkretAppletConfig::from_channel_config(&cfg).unwrap();
        applet.validate().unwrap();
        let journal = build_applet_authoring_journal(tmp.path(), &applet.id).unwrap();
        let edge = Arc::new(build_applet_edge(&applet, journal.clone()).await.unwrap());
        register_channel(AppletChannelState {
            journal,
            config: applet.clone(),
            edge: tokio::sync::Mutex::new(Some(edge)),
            host: tokio::sync::OnceCell::new(),
        })
        .unwrap();
        let service = Service::new(
            Router::new()
                .push(arkret_applet_router())
                .push(arkret_principal_router()),
        );
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
        for path in ["configuration", "managed-actors/author"] {
            let mut response =
                TestClient::post(format!("https://savfox.example/_arkret/edge/applet/{path}"))
                    .add_header("Destination-Service-Id", &applet.service_id, true)
                    .body("not-json")
                    .send(&service)
                    .await;
            assert_eq!(
                response.status_code,
                Some(StatusCode::BAD_REQUEST),
                "{path}"
            );
            let problem: arkret::Problem = response.take_json().await.unwrap();
            assert_eq!(problem.code(), "operation_selector_required");
        }
        let mut response = TestClient::get("https://savfox.example/_arkret/describe")
            .add_header("Destination-Service-Id", &applet.service_id, true)
            .add_header(
                "Arkret-Operation",
                arkret::ServiceOperationId::SERVER_READ_DESCRIBE_V1,
                true,
            )
            .send(&service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let description: arkret::ServiceDescribe = response.take_json().await.unwrap();
        description.validate().unwrap();
        assert_eq!(description.service_resolution.did, applet.service_did);
        let mut response = TestClient::get(format!(
            "https://savfox.example{}",
            arkret::canonical_service_resolution_path(&identity.service_id)
        ))
        .add_header(
            "Arkret-Operation",
            arkret::ServiceOperationId::OPEN_SERVICE_READ_RESOLUTION_V1,
            true,
        )
        .send(&service)
        .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let published: arkret::AuthenticatedServiceResolution = response.take_json().await.unwrap();
        arkret::identity::verify_authenticated_service_resolution_history(
            &published,
            &identity.service_id,
            now,
        )
        .unwrap();
        let mut response = TestClient::get(
            arkret::identity::DidWebvhResolver::log_url(&applet.service_did).unwrap(),
        )
        .send(&service)
        .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        arkret::identity::verify_did_webvh_v1_chain_bytes(
            &applet.service_did,
            &response.take_bytes(None).await.unwrap(),
        )
        .unwrap();
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
