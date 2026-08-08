use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use salvo::affix_state;
use salvo::http::header::CONTENT_TYPE;
use salvo::prelude::*;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::auth::{GatewayAuth, TokenInfo, TokenScope};
use crate::channel::{GatewayChannel, RuntimeBridgeSecrets};
use crate::chat_session::{
    abort_all_active_threads, abort_first_active_candidate, persist_chat_session_metadata,
    provider_from_model, resolve_abort_candidate_ids, validate_uuid_v7_session_id,
};
use crate::config::GatewayConfig;
use crate::cron_service::CronService;
use crate::home_paths::config_toml_path as gateway_config_toml_path;
use crate::otel::{self, GatewayMetrics};
use crate::plugin::{self, PluginState};
use crate::routing::{openai, openresponses};
use crate::session::{GatewaySessionManager, SessionStore, build_history_payload};
use crate::skills_api::{self, SkillsApiState};
use crate::ws::ws_handler;
use crate::{
    channels, exec_approval, hooks, log_store, pairing_store, routing, static_assets, tools_invoke,
    webchat,
};

#[derive(Debug, Clone)]
struct AgentRunRecord {
    run_id: String,
    status: String,
    result: Option<String>,
    error: Option<String>,
    updated_at_ms: u64,
}

fn agent_run_store() -> &'static Mutex<HashMap<String, AgentRunRecord>> {
    static STORE: OnceLock<Mutex<HashMap<String, AgentRunRecord>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

const PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE: u32 = 60;

#[derive(Debug, Clone)]
struct PluginRateLimitBucket {
    count: u32,
    window_start: Instant,
}

fn plugin_route_rate_limit_store() -> &'static Mutex<HashMap<String, PluginRateLimitBucket>> {
    static STORE: OnceLock<Mutex<HashMap<String, PluginRateLimitBucket>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn plugin_route_within_limit(plugin_id: &str) -> bool {
    let mut store = plugin_route_rate_limit_store().lock().await;
    let now = Instant::now();
    let bucket = store
        .entry(plugin_id.to_owned())
        .or_insert(PluginRateLimitBucket {
            count: 0,
            window_start: now,
        });

    if now.duration_since(bucket.window_start) >= Duration::from_secs(60) {
        bucket.count = 0;
        bucket.window_start = now;
    }

    if bucket.count >= PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE {
        return false;
    }

    bucket.count += 1;
    true
}

async fn authenticate_plugin_route(req: &Request, depot: &mut Depot, res: &mut Response) -> bool {
    let auth = if let Ok(a) = depot.get_typed::<Arc<GatewayAuth>>() {
        a.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return false;
    };

    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let Some(token) = token else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": "missing Authorization header"}).to_string(),
        ));
        return false;
    };

    if auth.validate(token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"error": "invalid token"}).to_string()));
        return false;
    }

    true
}

/// Validate the bearer token and return the matching `TokenInfo`, or `None`
/// after writing a 401 to `res`.
///
/// Unlike `authenticate_plugin_route` this returns the token info so callers
/// can perform additional scope checks (e.g. `Admin` on `/api/restart`).
async fn authenticate_with_info(
    req: &Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Option<TokenInfo> {
    let auth = depot.get_typed::<Arc<GatewayAuth>>().ok()?.clone();

    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let Some(token) = token else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": "missing Authorization header"}).to_string(),
        ));
        return None;
    };

    if let Some(info) = auth.validate(token).await {
        Some(info)
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"error": "invalid token"}).to_string()));
        None
    }
}

/// Returns `true` when the request path is **not** subject to the global
/// bearer-auth hoop. Externally signed callbacks (chat-platform webhooks,
/// OAuth redirects, the verification GET that WhatsApp expects) carry their
/// own per-protocol verification; the WS upgrade path uses a sec-websocket
/// subprotocol header for auth; SPA / static assets are public.
fn is_public_anonymous_path(path: &str) -> bool {
    if path == "/health" || path == "/" || path.is_empty() {
        return true;
    }
    if path.starts_with("/webhooks/") {
        return true;
    }
    if path.starts_with("/_discord/callback") {
        return true; // Discord OAuth2 redirect — not a Bearer caller.
    }
    if path.starts_with("/_matrix/") {
        return true; // Matrix appservice — Matrix-server signed.
    }
    if path == "/ws" {
        return true; // WS upgrade carries auth in sec-websocket-protocol.
    }
    false
}

/// Returns `true` when the request path is one of the well-known authenticated
/// API surfaces. Anything matching this set gets the bearer-auth hoop applied.
fn is_protected_api_path(path: &str) -> bool {
    path.starts_with("/api/")
        || path.starts_with("/v1/")
        || path.starts_with("/tools/")
        || path.starts_with("/hooks/")
        || path.starts_with("/plugins/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpAuthorization {
    AnyAuthenticated,
    Scope(TokenScope),
}

// Keep route registration and least-privilege authorization in one declaration.
// Public routes (health, WebSocket upgrade, callbacks and webhooks) deliberately
// stay outside this table because they use different authentication mechanisms.
macro_rules! protected_http_routes {
    ($consumer:ident) => {
        $consumer! {
            (get, "api/status", HttpAuthorization::Scope(TokenScope::OperatorRead), status_handler),
            (get, "api/logs", HttpAuthorization::Scope(TokenScope::OperatorRead), logs_handler),
            (get, "api/config", HttpAuthorization::Scope(TokenScope::OperatorRead), config_handler),
            (get, "api/metrics", HttpAuthorization::Scope(TokenScope::OperatorRead), otel::metrics_handler),
            (post, "api/token/validate", HttpAuthorization::AnyAuthenticated, token_validate_handler),
            (post, "api/message", HttpAuthorization::Scope(TokenScope::Chat), message_handler),
            (get, "api/sessions", HttpAuthorization::Scope(TokenScope::OperatorRead), sessions_handler),
            (get, "api/channels", HttpAuthorization::Scope(TokenScope::OperatorRead), channels_handler),
            (get, "api/nodes", HttpAuthorization::Scope(TokenScope::OperatorPairing), nodes_handler),
            (get, "api/devices", HttpAuthorization::Scope(TokenScope::OperatorPairing), devices_list_handler),
            (post, "api/devices/pair", HttpAuthorization::Scope(TokenScope::OperatorPairing), devices_pair_handler),
            (post, "api/devices/<device_id>/revoke", HttpAuthorization::Scope(TokenScope::OperatorPairing), devices_revoke_handler),
            (post, "api/config/patch", HttpAuthorization::Scope(TokenScope::OperatorAdmin), config_patch_handler),
            (post, "api/config/apply", HttpAuthorization::Scope(TokenScope::OperatorAdmin), config_apply_handler),
            (post, "api/restart", HttpAuthorization::Scope(TokenScope::OperatorAdmin), restart_handler),
            (get, "api/skills", HttpAuthorization::Scope(TokenScope::OperatorRead), skills_api::skills_list_handler),
            (get, "api/skills/search", HttpAuthorization::Scope(TokenScope::OperatorRead), skills_api::skills_search_handler),
            (get, "api/skills/installed", HttpAuthorization::Scope(TokenScope::OperatorRead), skills_api::skills_installed_handler),
            (post, "api/skills/refresh", HttpAuthorization::Scope(TokenScope::OperatorWrite), skills_api::skills_refresh_handler),
            (post, "api/skills/install", HttpAuthorization::Scope(TokenScope::OperatorAdmin), skills_api::skills_install_handler),
            (post, "api/skills/uninstall", HttpAuthorization::Scope(TokenScope::OperatorAdmin), skills_api::skills_uninstall_handler),
            (post, "v1/chat/completions", HttpAuthorization::Scope(TokenScope::Chat), openai::chat_completions_handler),
            (get, "v1/models", HttpAuthorization::Scope(TokenScope::OperatorRead), openai::models_list_handler),
            (post, "v1/responses", HttpAuthorization::Scope(TokenScope::Chat), openresponses::responses_handler),
            (post, "api/chat/completions", HttpAuthorization::Scope(TokenScope::Chat), openai::chat_completions_handler),
            (get, "api/models/openai", HttpAuthorization::Scope(TokenScope::OperatorRead), openai::models_list_handler),
            (post, "api/responses", HttpAuthorization::Scope(TokenScope::Chat), openresponses::responses_handler),
            (post, "tools/invoke", HttpAuthorization::Scope(TokenScope::OperatorAdmin), tools_invoke::tools_invoke_handler),
            (post, "api/exec/approval/request", HttpAuthorization::Scope(TokenScope::OperatorApprovalsRequest), exec_approval::approval_request_handler),
            (post, "api/exec/approval/resolve", HttpAuthorization::Scope(TokenScope::OperatorApprovalsResolve), exec_approval::approval_resolve_handler),
            (get, "api/exec/approvals", HttpAuthorization::Scope(TokenScope::OperatorApprovalsRead), exec_approval::approvals_list_handler),
            (post, "api/agent", HttpAuthorization::Scope(TokenScope::OperatorWrite), agent_handler),
            (post, "api/agent/wait", HttpAuthorization::Scope(TokenScope::OperatorRead), agent_wait_handler),
            (get, "api/sessions/<session_id>/history", HttpAuthorization::Scope(TokenScope::OperatorRead), session_history_handler),
            (get, "api/agents", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::agents_list_handler),
            (post, "api/agents", HttpAuthorization::Scope(TokenScope::OperatorAdmin), routing::agents_create_handler),
            (get, "api/agents/<agent_id>", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::agents_get_handler),
            (post, "api/agents/<agent_id>", HttpAuthorization::Scope(TokenScope::OperatorAdmin), routing::agents_update_handler),
            (delete, "api/agents/<agent_id>", HttpAuthorization::Scope(TokenScope::OperatorAdmin), routing::agents_delete_handler),
            (get, "api/sessions/preview", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::sessions_preview_handler),
            (post, "api/sessions/<session_id>", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::sessions_patch_handler),
            (post, "api/sessions/<session_id>/reset", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::sessions_reset_handler),
            (delete, "api/sessions/<session_id>", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::sessions_delete_handler),
            (post, "api/sessions/<session_id>/compact", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::sessions_compact_handler),
            (get, "api/usage/stats", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::usage_stats_handler),
            (get, "api/usage/timeseries", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::usage_timeseries_handler),
            (get, "api/usage/cost", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::usage_cost_handler),
            (get, "api/cron", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::cron_list_handler),
            (post, "api/cron", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::cron_add_handler),
            (get, "api/cron/<job_id>", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::cron_get_handler),
            (post, "api/cron/<job_id>", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::cron_update_handler),
            (delete, "api/cron/<job_id>", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::cron_delete_handler),
            (post, "api/cron/<job_id>/run", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::cron_run_handler),
            (get, "api/tts/status", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::tts_status_handler),
            (post, "api/tts/enable", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::tts_enable_handler),
            (post, "api/tts/disable", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::tts_disable_handler),
            (get, "api/tts/providers", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::tts_providers_handler),
            (get, "api/voicewake/status", HttpAuthorization::Scope(TokenScope::OperatorRead), routing::voicewake_status_handler),
            (post, "api/voicewake/enable", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::voicewake_enable_handler),
            (post, "api/voicewake/disable", HttpAuthorization::Scope(TokenScope::OperatorWrite), routing::voicewake_disable_handler),
            (post, "api/chat/abort", HttpAuthorization::Scope(TokenScope::Chat), chat_abort_handler),
            (post, "plugins/<plugin_id>", HttpAuthorization::Scope(TokenScope::OperatorAdmin), plugin_route_handler),
            (post, "plugins/<plugin_id>/<**plugin_route>", HttpAuthorization::Scope(TokenScope::OperatorAdmin), plugin_route_handler),
            (post, "hooks/wake", HttpAuthorization::Scope(TokenScope::OperatorAdmin), hooks::wake_handler),
            (post, "hooks/agent", HttpAuthorization::Scope(TokenScope::OperatorAdmin), hooks::agent_hook_handler),
            (post, "hooks/<mapping_id>", HttpAuthorization::Scope(TokenScope::OperatorAdmin), hooks::custom_hook_handler),
        }
    };
}

macro_rules! collect_protected_http_authorizations {
    ($(($method:ident, $path:literal, $authorization:expr, $handler:path),)*) => {
        &[$((stringify!($method), concat!("/", $path), $authorization)),*]
    };
}

const PROTECTED_HTTP_AUTHORIZATIONS: &[(&str, &str, HttpAuthorization)] =
    protected_http_routes!(collect_protected_http_authorizations);

fn protected_route_matches(template: &str, path: &str) -> bool {
    let mut template_segments = template.trim_matches('/').split('/');
    let mut path_segments = path.trim_matches('/').split('/');

    loop {
        match (template_segments.next(), path_segments.next()) {
            (None, None) => return true,
            (Some(segment), _) if segment.starts_with("<**") => return true,
            (Some(segment), Some(_)) if segment.starts_with('<') && segment.ends_with('>') => {}
            (Some(template_segment), Some(path_segment)) if template_segment == path_segment => {}
            _ => return false,
        }
    }
}

/// Return the least privilege required for an authenticated HTTP endpoint.
///
/// This is deliberately fail-closed: an unknown path under a protected prefix
/// is Admin-only. Registered routes and their authorization are generated from
/// the same declaration, so they cannot drift independently.
fn required_http_authorization(method: &str, path: &str) -> HttpAuthorization {
    let path = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };

    for &(route_method, template, authorization) in PROTECTED_HTTP_AUTHORIZATIONS {
        if method.eq_ignore_ascii_case(route_method) && protected_route_matches(template, path) {
            return authorization;
        }
    }

    HttpAuthorization::Scope(TokenScope::OperatorAdmin)
}

fn has_http_authorization(info: &TokenInfo, required: HttpAuthorization) -> bool {
    match required {
        HttpAuthorization::AnyAuthenticated => true,
        HttpAuthorization::Scope(scope) => info.has_scope(scope),
    }
}

/// Global Salvo hoop: enforce Bearer-auth on every protected endpoint.
///
/// The hoop short-circuits the request with `401 Unauthorized` when the
/// `Authorization: Bearer <token>` header is missing or invalid. Endpoints
/// that intentionally accept anonymous callers (health check, chat-platform
/// webhooks, OAuth callbacks, WS upgrade, SPA / static assets) are listed
/// in [`is_public_anonymous_path`] and bypass the hoop.
///
/// Per-method scope enforcement (Admin / Approvals / Pairing) is layered on
/// inside individual handlers — see [`require_admin_scope`] for the pattern.
#[handler]
async fn bearer_auth_hoop(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    let path = req.uri().path();
    if is_public_anonymous_path(path) || !is_protected_api_path(path) {
        return;
    }
    // Per-IP request rate limit (S12). Runs *before* auth so an attacker
    // hammering an unauthenticated endpoint with bad tokens still gets
    // throttled rather than amplifying our token-validation work. Only
    // applies to the protected API surface — the public anonymous paths
    // (webhooks, OAuth callbacks, SPA assets) are not rate-limited here.
    let limiter = global_rate_limiter(depot);
    if let Some(ip) = client_ip(req, depot)
        && !limiter.check_ip(ip).await
    {
        res.status_code(StatusCode::TOO_MANY_REQUESTS);
        res.render(Text::Json(
            json!({"error": "request rate limit exceeded"}).to_string(),
        ));
        ctrl.skip_rest();
        return;
    }
    // Unix-socket clients (or anything we cannot classify by IP) skip only
    // rate limiting. They still pass through authentication below.
    match authenticate_with_info(req, depot, res).await {
        Some(info) => {
            let required = required_http_authorization(req.method().as_str(), path);
            if !has_http_authorization(&info, required) {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Text::Json(
                    json!({"error": "insufficient token scope"}).to_string(),
                ));
                ctrl.skip_rest();
                return;
            }
            depot.insert_typed(info);
        }
        None => {
            ctrl.skip_rest();
        }
    }
}

/// Process-wide singleton holding the configured [`RateLimiter`].
///
/// Call sites that have a [`Depot`] (the bearer hoop, the WS upgrade)
/// access it via [`global_rate_limiter`] which transparently
/// initialises it from the operator's [`GatewayConfig`] on first use.
/// Background tasks that don't have a depot — the maintenance loop in
/// `run_main` — should call [`init_global_rate_limiter`] at startup so
/// they later see the same configured instance via
/// [`global_rate_limiter_uninitialised_default`].
fn rate_limiter_cell() -> &'static std::sync::OnceLock<crate::security::rate_limit::RateLimiter> {
    static LIMITER: std::sync::OnceLock<crate::security::rate_limit::RateLimiter> =
        std::sync::OnceLock::new();
    &LIMITER
}

/// Initialise the global rate limiter from the operator's
/// [`GatewayConfig`]. Idempotent — only the first call sets the value.
/// `lib.rs::run_main` calls this at startup so the maintenance task
/// can see the configured instance even before the first request.
pub(crate) fn init_global_rate_limiter(config: &GatewayConfig) {
    let cfg = crate::security::rate_limit::RateLimitConfig {
        max_requests: config.rate_limit.max_requests,
        window: std::time::Duration::from_secs(config.rate_limit.window_secs.max(1)),
        max_connections_per_ip: config.rate_limit.max_connections_per_ip,
    };
    let _ = rate_limiter_cell().set(crate::security::rate_limit::RateLimiter::new(cfg));
}

/// Get the process-wide [`RateLimiter`]. If [`init_global_rate_limiter`]
/// has not yet been called the limiter is lazily initialised from the
/// `GatewayConfig` carried by the depot, falling back to
/// [`RateLimitConfig::default()`] if the depot is empty (tests).
pub(crate) fn global_rate_limiter(
    depot: &Depot,
) -> &'static crate::security::rate_limit::RateLimiter {
    rate_limiter_cell().get_or_init(|| {
        let cfg = depot
            .get_typed::<Arc<GatewayConfig>>()
            .ok()
            .map(|g| crate::security::rate_limit::RateLimitConfig {
                max_requests: g.rate_limit.max_requests,
                window: std::time::Duration::from_secs(g.rate_limit.window_secs.max(1)),
                max_connections_per_ip: g.rate_limit.max_connections_per_ip,
            })
            .unwrap_or_default();
        crate::security::rate_limit::RateLimiter::new(cfg)
    })
}

/// Get the process-wide [`RateLimiter`] from a context that has no depot
/// (e.g. background maintenance tasks). The limiter MUST have been
/// initialised via [`init_global_rate_limiter`] before this is called;
/// if not, returns a default-config limiter for safety.
pub(crate) fn global_rate_limiter_uninitialised_default()
-> &'static crate::security::rate_limit::RateLimiter {
    rate_limiter_cell().get_or_init(|| {
        crate::security::rate_limit::RateLimiter::new(
            crate::security::rate_limit::RateLimitConfig::default(),
        )
    })
}

/// Extract the effective client IP, honouring the operator's
/// `trust_x_forwarded_for` setting in [`GatewayConfig`].
///
/// When the gateway sits behind a reverse proxy that injects
/// `X-Forwarded-For`, every connection's `remote_addr` is the proxy.
/// Operators that explicitly opt in to trusting that header get the
/// **left-most** address (the original client) returned here. By
/// default we ignore the header so a directly-reachable gateway cannot
/// be spoofed by a client setting `X-Forwarded-For: 8.8.8.8`.
pub(crate) fn client_ip(req: &Request, depot: &Depot) -> Option<std::net::IpAddr> {
    let trust_xff = depot
        .get_typed::<Arc<GatewayConfig>>()
        .ok()
        .is_some_and(|g| g.trust_x_forwarded_for);
    if trust_xff
        && let Some(xff) = req.headers().get("x-forwarded-for")
        && let Ok(s) = xff.to_str()
        && let Some(first) = s.split(',').next()
        && let Ok(ip) = first.trim().parse::<std::net::IpAddr>()
    {
        return Some(ip);
    }
    req.remote_addr().clone().into_std().map(|s| s.ip())
}

/// Inside an authenticated handler, additionally require the `Admin` scope.
///
/// Writes a `403 Forbidden` and returns `false` if the bearer does not carry
/// `OperatorAdmin`. Returns `true` to let the handler continue.
async fn require_admin_scope(req: &Request, depot: &mut Depot, res: &mut Response) -> bool {
    let Some(info) = authenticate_with_info(req, depot, res).await else {
        return false;
    };
    if !info.has_scope(TokenScope::OperatorAdmin) {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Text::Json(
            json!({"error": "admin scope required"}).to_string(),
        ));
        return false;
    }
    true
}

async fn upsert_agent_run(record: AgentRunRecord) {
    let mut lock = agent_run_store().lock().await;
    if lock.len() > 2048 {
        let mut entries: Vec<_> = lock.values().cloned().collect();
        entries.sort_by_key(|v| v.updated_at_ms);
        let remove_count = entries.len() / 2;
        for entry in entries.into_iter().take(remove_count) {
            lock.remove(&entry.run_id);
        }
    }
    lock.insert(record.run_id.clone(), record);
}

async fn get_agent_run(run_id: &str) -> Option<AgentRunRecord> {
    let lock = agent_run_store().lock().await;
    lock.get(run_id).cloned()
}

/// Build the Salvo router with all gateway endpoints.
pub(crate) fn build_router(
    auth: Arc<GatewayAuth>,
    session_mgr: Arc<GatewaySessionManager>,
    channel: Arc<GatewayChannel>,
    config: &GatewayConfig,
    session_store: Arc<SessionStore>,
    cron_service: Arc<CronService>,
    savfox_home: PathBuf,
) -> Router {
    let gateway_config = Arc::new(config.clone());
    let skills_state = Arc::new(SkillsApiState::new(&savfox_home));
    let metrics = Arc::new(GatewayMetrics::new());

    let state_router = Router::new()
        .hoop(affix_state::inject(auth))
        .hoop(affix_state::inject(session_mgr))
        .hoop(affix_state::inject(channel))
        .hoop(affix_state::inject(gateway_config))
        .hoop(affix_state::inject(session_store))
        .hoop(affix_state::inject(cron_service))
        .hoop(affix_state::inject(skills_state))
        .hoop(affix_state::inject(metrics))
        // Global Bearer-auth hoop. The hoop itself decides per-path whether
        // to require the Authorization header (see `is_protected_api_path`)
        // or pass through (see `is_public_anonymous_path`). This protects
        // every /api/*, /tools/*, /hooks/*, /plugins/* endpoint without
        // requiring each handler to repeat the boilerplate.
        .hoop(bearer_auth_hoop);

    let mut router = state_router
        .push(Router::with_path("health").get(health_handler))
        .push(Router::with_path("ws").goal(ws_handler));

    macro_rules! register_protected_http_routes {
        ($(($method:ident, $path:literal, $authorization:expr, $handler:path),)*) => {
            $(
                router = router.push(Router::with_path($path).$method($handler));
            )*
        };
    }
    protected_http_routes!(register_protected_http_routes);

    // Webhook endpoints for chat platform channels.
    let webhook_router = Router::with_path("webhooks")
        .push(Router::with_path("discord").post(channels::discord::webhook_handler))
        .push(Router::with_path("dingtalk").post(channels::dingtalk::webhook_handler))
        .push(Router::with_path("telegram").post(channels::telegram::webhook_handler))
        .push(Router::with_path("slack").post(channels::slack::webhook_handler))
        .push(Router::with_path("matrix").post(channels::matrix::webhook_handler))
        .push(Router::with_path("msteams").post(channels::msteams::webhook_handler))
        .push(Router::with_path("mattermost").post(channels::mattermost::webhook_handler))
        .push(Router::with_path("googlechat").post(channels::googlechat::webhook_handler))
        .push(Router::with_path("line").post(channels::line::webhook_handler))
        .push(Router::with_path("feishu").post(channels::feishu::webhook_handler))
        .push(Router::with_path("irc").post(channels::irc::webhook_handler))
        .push(Router::with_path("webhook").post(channels::webhook::webhook_handler))
        .push(Router::with_path("whatsapp").get(channels::whatsapp::webhook_verification_handler))
        .push(Router::with_path("whatsapp").post(channels::whatsapp::webhook_handler))
        .push(Router::with_path("qq").post(channels::qq::webhook_handler))
        .push(Router::with_path("wechat").post(channels::wechat::webhook_handler))
        .push(Router::with_path("signal").post(channels::signal::webhook_handler))
        .push(Router::with_path("zalo").post(channels::zalo::webhook_handler));

    router = router.push(webhook_router);

    // Discord OAuth2 callback endpoint.
    router = router.push(
        Router::with_path("_discord/callback").get(channels::discord::oauth_callback_handler),
    );

    router = router.push(channels::matrix::matrix_appservice_router());
    router = router.push(channels::matrix::appservice_router());

    #[cfg(feature = "arkret")]
    {
        router = router.push(channels::arkret_applet::arkret_applet_router());
        router = router.push(channels::arkret_applet::arkret_appservices_router());
    }

    // WebChat embeddable widget endpoints.
    router = router.push(webchat::webchat_router());

    // SPA fallback: serve the embedded web UI for all unmatched GET requests.
    // This must be the LAST route so API routes are matched first.
    router = router.push(Router::with_path("{**rest}").get(static_assets::spa_handler));
    router = router.push(Router::with_path("").get(static_assets::spa_handler));

    router
}

/// Start the Salvo server.
/// Maximum size of an HTTP request body the gateway will read directly
/// (8 MiB). Larger bodies are rejected before being copied into memory
/// (M15 follow-up). Note: this only protects byte-oriented body reads.
/// Multipart / streaming uploads write to a temporary file and are not
/// bounded here — those must be enforced per-handler.
const MAX_HTTP_BODY_SIZE: usize = 8 << 20;

pub(crate) async fn start_server(
    config: &GatewayConfig,
    auth: Arc<GatewayAuth>,
    session_mgr: Arc<GatewaySessionManager>,
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    cron_service: Arc<CronService>,
    savfox_home: PathBuf,
) -> std::io::Result<()> {
    channels::runtime::set_response_footer_config(config.response_footer.clone());

    // Install the global HTTP body size cap before any handler runs.
    salvo::http::request::set_global_secure_max_size(MAX_HTTP_BODY_SIZE);

    let router = build_router(
        auth,
        session_mgr,
        channel,
        config,
        session_store,
        cron_service,
        savfox_home,
    );
    let bind_addr = format!("{}:{}", config.host, config.port);

    info!("gateway server starting on {bind_addr}");

    let acceptor = TcpListener::new(bind_addr).bind().await;
    let server = Server::new(acceptor);
    let shutdown_handle = server.handle();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("gateway shutdown signal received");
                shutdown_handle.stop_graceful(std::time::Duration::from_secs(10));
            }
            Err(err) => {
                warn!(error = %err, "failed to install gateway shutdown signal");
            }
        }
    });
    server.serve(router).await;

    Ok(())
}

/// `GET /health`  - Simple health check.
#[handler]
async fn health_handler(res: &mut Response) {
    res.headers_mut().insert(
        CONTENT_TYPE,
        "application/json"
            .parse()
            .expect("json content-type header should parse"),
    );

    let body = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    });

    res.render(Text::Json(body.to_string()));
}

/// `GET /api/status`  - Server status (connected clients, etc.).
#[handler]
async fn status_handler(depot: &mut Depot, res: &mut Response) {
    let session_mgr = if let Ok(mgr) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        mgr.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let count = session_mgr.session_count().await;
    let session_ids = session_mgr.session_ids().await;
    let audit_summary = crate::security_audit::run_audit(&channel.config().savfox_home)
        .await
        .summary;
    let plugins = plugin::discover_snapshot(&channel.config().savfox_home)
        .await
        .unwrap_or_default();
    let plugin_routes = plugin::describe_http_routes(&plugins, PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE);

    let body = json!({
        "connected_clients": count,
        "session_ids": session_ids,
        "security_audit": audit_summary,
        "plugin_routes": plugin_routes,
    });

    res.render(Text::Json(body.to_string()));
}

/// `GET /api/logs`  - In-memory gateway logs.
#[handler]
async fn logs_handler(req: &mut Request, res: &mut Response) {
    let limit = req.query::<usize>("lines").unwrap_or(50);
    let items = log_store::list_logs(limit).await;
    res.render(Text::Json(
        json!({
            "logs": items,
            "count": items.len(),
        })
        .to_string(),
    ));
}

/// `GET /api/config`  - Gateway configuration (sanitized, no secrets).
#[handler]
async fn config_handler(depot: &mut Depot, res: &mut Response) {
    let session_mgr = if let Ok(mgr) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        mgr.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let count = session_mgr.session_count().await;

    // Return sanitized config  - no tokens/secrets
    let body = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "connected_clients": count,
        "channels": {
            "discord": "configured",
            "telegram": "configured",
            "slack": "configured",
            "webhook": "configured",
        },
        "endpoints": {
            "ws": "/ws",
            "health": "/health",
            "status": "/api/status",
            "config": "/api/config",
            "message": "/api/message",
            "sessions": "/api/sessions",
            "token_validate": "/api/token/validate",
        },
    });

    res.render(Text::Json(body.to_string()));
}

/// `POST /api/token/validate`  - Validate a bearer token.
#[handler]
async fn token_validate_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let auth = if let Ok(a) = depot.get_typed::<Arc<GatewayAuth>>() {
        a.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    // Accept token from Authorization header or JSON body.
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_owned())
        .or({
            // Try reading from body as {"token": "..."}
            None
        });

    let Some(token) = token else {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"valid": false, "error": "no token provided"}).to_string(),
        ));
        return;
    };

    if let Some(info) = auth.validate(&token).await {
        res.render(Text::Json(
            json!({
                "valid": true,
                "label": info.label,
                "scopes": info.scopes,
            })
            .to_string(),
        ));
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"valid": false}).to_string()));
    }
}

/// `POST /api/message`  - Send a message to a chat platform channel.
///
/// Accepts JSON body: `{"channel": "discord:12345", "text": "Hello!"}`
#[handler]
async fn message_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid request body: {err}")}).to_string(),
            ));
            return;
        }
    };

    let channel_id = body.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");

    if channel_id.is_empty() || text.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "both 'channel' and 'text' are required"}).to_string(),
        ));
        return;
    }

    match channel
        .send_platform_message(channel_id, text, None, None, None)
        .await
    {
        Ok(()) => {
            res.render(Text::Json(json!({"status": "sent"}).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": format!("failed to send message: {err}")}).to_string(),
            ));
        }
    }
}

/// `GET /api/sessions`  - List active sessions.
#[handler]
async fn sessions_handler(depot: &mut Depot, res: &mut Response) {
    let session_mgr = if let Ok(mgr) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        mgr.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_ids = session_mgr.session_ids().await;
    let count = session_mgr.session_count().await;

    let body = json!({
        "sessions": session_ids,
        "count": count,
    });

    res.render(Text::Json(body.to_string()));
}

/// `GET /api/channels`  - List configured channel integrations.
#[handler]
async fn channels_handler(depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let runtime = channel.runtime_channel_secrets().await;
    let body = json!({
        "channels": [
            {
                "id": "discord",
                "status": if runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok() { "configured" } else { "missing_credentials" }
            },
            { "id": "dingtalk", "status": "available" },
            {
                "id": "telegram",
                "status": if runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok() { "configured" } else { "missing_credentials" }
            },
            {
                "id": "slack",
                "status": if runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok() { "configured" } else { "missing_credentials" }
            },
            { "id": "matrix", "status": "available" },
            { "id": "mattermost", "status": "available" },
            { "id": "googlechat", "status": "available" },
            { "id": "line", "status": "available" },
            { "id": "feishu", "status": "available" },
            { "id": "irc", "status": "available" },
            { "id": "webhook", "status": "available" }
        ]
    });
    res.render(Text::Json(body.to_string()));
}

/// `GET /api/nodes`  - List known node IDs from pairing records.
#[handler]
async fn nodes_handler(res: &mut Response) {
    match pairing_store::list_requests().await {
        Ok(records) => {
            let mut nodes: Vec<String> = records.into_iter().map(|r| r.node_id).collect();
            nodes.sort();
            nodes.dedup();
            res.render(Text::Json(json!({ "nodes": nodes }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err }).to_string()));
        }
    }
}

/// `GET /api/devices`  - List paired devices.
#[handler]
async fn devices_list_handler(res: &mut Response) {
    match pairing_store::list_devices().await {
        Ok(records) => {
            res.render(Text::Json(json!({ "devices": records }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err }).to_string()));
        }
    }
}

/// `POST /api/devices/pair`  - Create a new device pairing request.
#[handler]
async fn devices_pair_handler(req: &mut Request, res: &mut Response) {
    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(json!({}));
    let node_id = body
        .get("node_id")
        .and_then(|v| v.as_str())
        .unwrap_or("gateway");
    let device_id_owned = body
        .get("device_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let device_name = body.get("name").and_then(|v| v.as_str());

    match pairing_store::create_request(node_id, Some(device_id_owned.as_str()), device_name).await
    {
        Ok(record) => {
            res.render(Text::Json(json!({ "device": record }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err }).to_string()));
        }
    }
}

/// `POST /api/devices/:device_id/revoke`  - Revoke a device token.
#[handler]
async fn devices_revoke_handler(req: &mut Request, res: &mut Response) {
    let device_id = req.param::<String>("device_id").unwrap_or_default();
    if device_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing device_id" }).to_string(),
        ));
        return;
    }
    match pairing_store::revoke_device_token(&device_id).await {
        Ok(record) => {
            res.render(Text::Json(json!({ "device": record }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
        }
    }
}

fn merge_json_patch(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target_map), Value::Object(patch_map)) => {
            for (key, patch_value) in patch_map {
                if patch_value.is_null() {
                    target_map.remove(key);
                    continue;
                }
                match target_map.get_mut(key) {
                    Some(existing) => merge_json_patch(existing, patch_value),
                    None => {
                        target_map.insert(key.clone(), patch_value.clone());
                    }
                }
            }
        }
        (target_slot, patch_value) => {
            *target_slot = patch_value.clone();
        }
    }
}

fn config_toml_path(channel: &GatewayChannel) -> std::path::PathBuf {
    gateway_config_toml_path(&channel.config().savfox_home)
}

async fn read_config_json(path: &std::path::Path) -> Result<Value, String> {
    // Load .env file if present alongside config
    if let Some(parent) = path.parent() {
        let env_path = parent.join(".env");
        if env_path.exists() {
            let _ = dotenvy::from_path(&env_path);
        }
    }

    let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    let parsed_toml = content
        .parse::<toml::Value>()
        .map_err(|err| format!("failed to parse existing config.toml: {err}"))?;
    let mut value = serde_json::to_value(parsed_toml)
        .map_err(|err| format!("failed to convert existing TOML to JSON: {err}"))?;

    // Apply environment variable substitution
    substitute_env_vars(&mut value)
        .map_err(|err| format!("failed to substitute environment variables: {err}"))?;

    Ok(value)
}

/// Recursively substitute `$ENV_VAR`, `${VAR}`, `${VAR:-default}`, and
/// `${VAR:?error}` in JSON string values.
fn substitute_env_vars(value: &mut Value) -> Result<(), String> {
    match value {
        Value::String(s) if s.contains('$') => {
            *s = expand_env_string(s)?;
        }
        Value::String(_) => {}
        Value::Object(map) => {
            for v in map.values_mut() {
                substitute_env_vars(v)?;
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                substitute_env_vars(v)?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Expand env vars in a string.
///
/// Supported forms:
/// - `$VAR`
/// - `${VAR}`
/// - `${VAR:-default}`
/// - `${VAR:?error message}`
fn expand_env_string(input: &str) -> Result<String, String> {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                // ${VAR}, ${VAR:-default}, or ${VAR:?error}
                if let Some(close) = chars[i + 2..].iter().position(|&c| c == '}') {
                    let inner: String = chars[i + 2..i + 2 + close].iter().collect();
                    let (var_name, default_val, required_err) = if let Some(pos) = inner.find(":-")
                    {
                        (&inner[..pos], Some(&inner[pos + 2..]), None)
                    } else if let Some(pos) = inner.find(":?") {
                        (&inner[..pos], None, Some(&inner[pos + 2..]))
                    } else {
                        (inner.as_str(), None, None)
                    };

                    let var_name = var_name.trim();
                    if var_name.is_empty() {
                        let orig: String = chars[i..i + 2 + close + 1].iter().collect();
                        result.push_str(&orig);
                    } else {
                        let resolved = std::env::var(var_name).ok().filter(|v| !v.is_empty());
                        match resolved {
                            Some(val) => result.push_str(&val),
                            None => {
                                if let Some(def) = default_val {
                                    result.push_str(def);
                                } else if let Some(err_msg) = required_err {
                                    let message = if err_msg.trim().is_empty() {
                                        format!(
                                            "required environment variable '{var_name}' is missing"
                                        )
                                    } else {
                                        err_msg.to_owned()
                                    };
                                    return Err(message);
                                } else {
                                    // `${VAR}` expands to empty when unset.
                                    result.push_str("");
                                }
                            }
                        }
                    }
                    i += 2 + close + 1;
                    continue;
                }
            } else if chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_' {
                // $VAR_NAME
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                let var_name: String = chars[start..end].iter().collect();
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val)
                } else {
                    let orig: String = chars[i..end].iter().collect();
                    result.push_str(&orig);
                }
                i = end;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use salvo::Service;
    use salvo::conn::SocketAddr;
    use salvo::http::Method;
    use salvo::http::uri::{Scheme, Uri};
    #[cfg(windows)]
    use salvo::prelude::{Listener, Server};
    use salvo::prelude::{Request, Response, Router, StatusCode};
    use serde_json::json;

    use super::{
        GatewayAuth, HttpAuthorization, TokenInfo, TokenScope, bearer_auth_hoop, expand_env_string,
        has_http_authorization, is_protected_api_path, is_public_anonymous_path,
        required_http_authorization, substitute_env_vars,
    };

    #[cfg(windows)]
    #[tokio::test]
    async fn graceful_stop_releases_port_for_same_config_restart() {
        let reservation =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral port");
        let address = reservation.local_addr().expect("reserved local address");
        drop(reservation);

        let acceptor = salvo::conn::TcpListener::new(address).bind().await;
        let server = Server::new(acceptor);
        let handle = server.handle();
        let serving = tokio::spawn(server.serve(Router::new()));

        tokio::net::TcpStream::connect(address)
            .await
            .expect("server should accept connections");
        handle.stop_graceful(std::time::Duration::from_secs(1));
        tokio::time::timeout(std::time::Duration::from_secs(3), serving)
            .await
            .expect("server should stop before the graceful timeout")
            .expect("server task should join");

        let rebound = std::net::TcpListener::bind(address)
            .expect("the same gateway address should be reusable after shutdown");
        drop(rebound);
    }

    #[test]
    fn anonymous_paths_skip_auth_hoop() {
        for path in [
            "/health",
            "/",
            "",
            "/webhooks/discord",
            "/webhooks/telegram",
            "/_discord/callback",
            "/_matrix/appservice/transactions/abc",
            "/ws",
        ] {
            assert!(
                is_public_anonymous_path(path),
                "path should be anonymous: {path}"
            );
        }
    }

    #[test]
    fn protected_paths_require_auth() {
        for path in [
            "/api/agent",
            "/api/restart",
            "/api/exec/approval/resolve",
            "/api/devices/pair",
            "/api/sessions/abc/history",
            "/v1/chat/completions",
            "/v1/models",
            "/v1/responses",
            "/tools/invoke",
            "/hooks/wake",
            "/plugins/some-id",
        ] {
            assert!(
                is_protected_api_path(path),
                "path should be protected: {path}"
            );
            assert!(
                !is_public_anonymous_path(path),
                "path must not be anonymous: {path}"
            );
        }
    }

    #[test]
    fn protected_http_routes_have_least_privilege_scope_mappings() {
        use HttpAuthorization::AnyAuthenticated;
        use HttpAuthorization::Scope;

        for (method, path, expected) in [
            ("POST", "/api/token/validate", AnyAuthenticated),
            ("GET", "/api/status", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/logs", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/config", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/metrics", Scope(TokenScope::OperatorRead)),
            ("POST", "/api/message", Scope(TokenScope::Chat)),
            ("GET", "/api/sessions", Scope(TokenScope::OperatorRead)),
            (
                "GET",
                "/api/sessions/session-1/history",
                Scope(TokenScope::OperatorRead),
            ),
            (
                "POST",
                "/api/sessions/session-1/compact",
                Scope(TokenScope::OperatorWrite),
            ),
            ("GET", "/api/channels", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/nodes", Scope(TokenScope::OperatorPairing)),
            (
                "POST",
                "/api/devices/device-1/revoke",
                Scope(TokenScope::OperatorPairing),
            ),
            (
                "POST",
                "/api/config/patch",
                Scope(TokenScope::OperatorAdmin),
            ),
            ("GET", "/api/skills/search", Scope(TokenScope::OperatorRead)),
            (
                "POST",
                "/api/skills/refresh",
                Scope(TokenScope::OperatorWrite),
            ),
            (
                "POST",
                "/api/skills/install",
                Scope(TokenScope::OperatorAdmin),
            ),
            ("GET", "/v1/models", Scope(TokenScope::OperatorRead)),
            ("POST", "/v1/responses", Scope(TokenScope::Chat)),
            ("POST", "/tools/invoke", Scope(TokenScope::OperatorAdmin)),
            (
                "POST",
                "/api/exec/approval/request",
                Scope(TokenScope::OperatorApprovalsRequest),
            ),
            (
                "POST",
                "/api/exec/approval/resolve",
                Scope(TokenScope::OperatorApprovalsResolve),
            ),
            (
                "GET",
                "/api/exec/approvals",
                Scope(TokenScope::OperatorApprovalsRead),
            ),
            ("POST", "/api/agent", Scope(TokenScope::OperatorWrite)),
            ("POST", "/api/agent/wait", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/agents/a", Scope(TokenScope::OperatorRead)),
            ("DELETE", "/api/agents/a", Scope(TokenScope::OperatorAdmin)),
            ("GET", "/api/usage/cost", Scope(TokenScope::OperatorRead)),
            ("GET", "/api/cron", Scope(TokenScope::OperatorRead)),
            (
                "POST",
                "/api/cron/job-1/run",
                Scope(TokenScope::OperatorWrite),
            ),
            ("GET", "/api/tts/status", Scope(TokenScope::OperatorRead)),
            ("POST", "/api/tts/enable", Scope(TokenScope::OperatorWrite)),
            ("POST", "/api/chat/abort", Scope(TokenScope::Chat)),
            (
                "POST",
                "/hooks/custom-mapping",
                Scope(TokenScope::OperatorAdmin),
            ),
            (
                "POST",
                "/plugins/example/route",
                Scope(TokenScope::OperatorAdmin),
            ),
        ] {
            assert_eq!(
                required_http_authorization(method, path),
                expected,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn unknown_protected_http_routes_fail_closed_to_admin() {
        assert_eq!(
            required_http_authorization("GET", "/api/future-capability"),
            HttpAuthorization::Scope(TokenScope::OperatorAdmin)
        );
        assert_eq!(
            required_http_authorization("PUT", "/api/sessions/session-1"),
            HttpAuthorization::Scope(TokenScope::OperatorAdmin)
        );
    }

    #[test]
    fn http_scope_check_preserves_role_separation() {
        let info = |scope| TokenInfo {
            label: "test".to_owned(),
            scopes: vec![scope],
        };

        let viewer = info(TokenScope::Viewer);
        assert!(has_http_authorization(
            &viewer,
            HttpAuthorization::Scope(TokenScope::OperatorRead)
        ));
        assert!(!has_http_authorization(
            &viewer,
            HttpAuthorization::Scope(TokenScope::OperatorWrite)
        ));
        assert!(!has_http_authorization(
            &viewer,
            HttpAuthorization::Scope(TokenScope::Chat)
        ));

        let chat = info(TokenScope::Chat);
        assert!(has_http_authorization(
            &chat,
            HttpAuthorization::Scope(TokenScope::Chat)
        ));
        assert!(has_http_authorization(
            &chat,
            HttpAuthorization::AnyAuthenticated
        ));
        assert!(!has_http_authorization(
            &chat,
            HttpAuthorization::Scope(TokenScope::OperatorRead)
        ));

        let requester = info(TokenScope::OperatorApprovalsRequest);
        assert!(has_http_authorization(
            &requester,
            HttpAuthorization::Scope(TokenScope::OperatorApprovalsRequest)
        ));
        assert!(!has_http_authorization(
            &requester,
            HttpAuthorization::Scope(TokenScope::OperatorApprovalsRead)
        ));

        let operator = info(TokenScope::Operator);
        for scope in [
            TokenScope::OperatorRead,
            TokenScope::OperatorWrite,
            TokenScope::OperatorAdmin,
            TokenScope::OperatorPairing,
            TokenScope::OperatorApprovalsRequest,
            TokenScope::OperatorApprovalsRead,
            TokenScope::OperatorApprovalsResolve,
            TokenScope::Chat,
        ] {
            assert!(has_http_authorization(
                &operator,
                HttpAuthorization::Scope(scope)
            ));
        }
    }

    #[salvo::handler]
    async fn authorization_probe(res: &mut Response) {
        res.status_code(StatusCode::NO_CONTENT);
    }

    async fn call_authorization_probe(
        auth: Arc<GatewayAuth>,
        method: Method,
        path: &str,
        token: Option<&str>,
    ) -> Response {
        let router = Router::new()
            .hoop(salvo::affix_state::inject(auth))
            .hoop(bearer_auth_hoop)
            .push(Router::with_path("api/status").get(authorization_probe))
            .push(Router::with_path("api/sessions").get(authorization_probe))
            .push(Router::with_path("api/message").post(authorization_probe))
            .push(Router::with_path("api/config/patch").post(authorization_probe));
        let service = Service::new(router);
        let mut req = Request::new();
        *req.method_mut() = method;
        req.set_uri(path.parse::<Uri>().expect("valid test URI"));
        if let Some(token) = token {
            req.headers_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .expect("valid auth header"),
            );
        }

        service
            .hyper_handler(
                SocketAddr::Unknown,
                SocketAddr::Unknown,
                Scheme::HTTP,
                None,
                salvo::conn::ConnCtrl::default(),
                None,
            )
            .handle(req)
            .await
    }

    #[tokio::test]
    async fn bearer_auth_hoop_enforces_http_scope_matrix() {
        let auth = Arc::new(GatewayAuth::new(HashMap::from([
            (
                "viewer-token".to_owned(),
                TokenInfo {
                    label: "viewer".to_owned(),
                    scopes: vec![TokenScope::Viewer],
                },
            ),
            (
                "chat-token".to_owned(),
                TokenInfo {
                    label: "chat".to_owned(),
                    scopes: vec![TokenScope::Chat],
                },
            ),
            (
                "admin-token".to_owned(),
                TokenInfo {
                    label: "admin".to_owned(),
                    scopes: vec![TokenScope::OperatorAdmin],
                },
            ),
        ])));

        for (method, path, token, expected) in [
            (Method::GET, "/api/status", None, StatusCode::UNAUTHORIZED),
            (
                Method::GET,
                "/api/status",
                Some("viewer-token"),
                StatusCode::NO_CONTENT,
            ),
            (
                Method::POST,
                "/api/config/patch",
                Some("viewer-token"),
                StatusCode::FORBIDDEN,
            ),
            (
                Method::POST,
                "/api/message",
                Some("chat-token"),
                StatusCode::NO_CONTENT,
            ),
            (
                Method::GET,
                "/api/sessions",
                Some("chat-token"),
                StatusCode::FORBIDDEN,
            ),
            (
                Method::POST,
                "/api/config/patch",
                Some("admin-token"),
                StatusCode::NO_CONTENT,
            ),
        ] {
            let response = call_authorization_probe(auth.clone(), method, path, token).await;
            assert_eq!(response.status_code, Some(expected), "{path}");
        }
    }

    #[test]
    fn unrelated_paths_are_neither_public_nor_protected() {
        // SPA fallback / static asset paths are not in either list — they
        // pass through the hoop because is_protected_api_path() returns false.
        for path in ["/index.html", "/static/app.js", "/dashboard"] {
            assert!(!is_protected_api_path(path), "unexpected protected: {path}");
            assert!(
                !is_public_anonymous_path(path),
                "unexpected anonymous: {path}"
            );
        }
    }

    #[test]
    fn expand_env_string_default_fallback_when_unset() {
        let input = "${SAVFOX_TEST_UNSET_DEFAULT_3D2D8040:-fallback-value}";
        let output = expand_env_string(input).expect("default fallback should not fail");
        assert_eq!(output, "fallback-value");
    }

    #[test]
    fn expand_env_string_required_errors_when_unset() {
        let input = "${SAVFOX_TEST_UNSET_REQUIRED_6B3B9F69:?required env missing}";
        let err = expand_env_string(input).expect_err("required var should fail");
        assert_eq!(err, "required env missing");
    }

    #[test]
    fn substitute_env_vars_recurses_and_propagates_error() {
        let mut value = json!({
            "model": "${SAVFOX_TEST_UNSET_MODEL_2D8E45A7:-gpt-4o-mini}",
            "token": "${SAVFOX_TEST_UNSET_TOKEN_1C9F5E12:?token required}",
            "nested": ["${SAVFOX_TEST_UNSET_OTHER_9F6B4A22:-ok}"]
        });

        let err = substitute_env_vars(&mut value).expect_err("required env should fail");
        assert_eq!(err, "token required");
        assert_eq!(value["model"], "gpt-4o-mini");
    }
}

async fn write_config_json(path: &std::path::Path, config: &Value) -> Result<(), String> {
    let toml_value = savfox_utils::json_to_toml::json_to_toml(config.clone());
    let toml_str = toml::to_string(&toml_value)
        .map_err(|err| format!("failed to serialize config as TOML: {err}"))?;
    tokio::fs::write(path, toml_str)
        .await
        .map_err(|err| format!("failed to write config.toml: {err}"))
}

fn json_path_str<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = value;
    for seg in path {
        cur = cur.get(*seg)?;
    }
    cur.as_str()
}

fn json_path_str_any<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths.iter().find_map(|path| json_path_str(value, path))
}

fn runtime_channel_secrets_from_config(config: &Value) -> RuntimeBridgeSecrets {
    RuntimeBridgeSecrets {
        discord_bot_token: json_path_str_any(
            config,
            &[
                &["gateway", "channels", "discord", "bot_token"],
                &["gateway", "bridges", "discord", "bot_token"],
            ],
        )
        .map(ToOwned::to_owned),
        telegram_bot_token: json_path_str_any(
            config,
            &[
                &["gateway", "channels", "telegram", "bot_token"],
                &["gateway", "bridges", "telegram", "bot_token"],
            ],
        )
        .map(ToOwned::to_owned),
        slack_bot_token: json_path_str_any(
            config,
            &[
                &["gateway", "channels", "slack", "bot_token"],
                &["gateway", "bridges", "slack", "bot_token"],
            ],
        )
        .map(ToOwned::to_owned),
        slack_signing_secret: json_path_str_any(
            config,
            &[
                &["gateway", "channels", "slack", "signing_secret"],
                &["gateway", "bridges", "slack", "signing_secret"],
            ],
        )
        .map(ToOwned::to_owned),
        webhook_secret: json_path_str_any(
            config,
            &[
                &["gateway", "channels", "webhook", "secret"],
                &["gateway", "bridges", "webhook", "secret"],
            ],
        )
        .map(ToOwned::to_owned),
    }
}

fn count_runtime_secrets(secrets: &RuntimeBridgeSecrets) -> usize {
    [
        secrets.discord_bot_token.as_ref(),
        secrets.telegram_bot_token.as_ref(),
        secrets.slack_bot_token.as_ref(),
        secrets.slack_signing_secret.as_ref(),
        secrets.webhook_secret.as_ref(),
    ]
    .iter()
    .filter(|v| v.is_some())
    .count()
}

/// `POST /api/config/patch`  - Merge-patch the gateway configuration.
#[handler]
async fn config_patch_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() {
        channel.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid body: {err}")}).to_string(),
            ));
            return;
        }
    };

    let patch_config = body
        .get("config")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let note = body
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("(no note)");

    let path = config_toml_path(&channel);
    let mut current = match read_config_json(&path).await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({"error": err}).to_string()));
            return;
        }
    };
    merge_json_patch(&mut current, &patch_config);
    if let Err(err) = write_config_json(&path, &current).await {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(json!({"error": err}).to_string()));
        return;
    }

    let runtime_secrets = runtime_channel_secrets_from_config(&current);
    let runtime_updates = count_runtime_secrets(&runtime_secrets);
    channel.set_runtime_channel_secrets(runtime_secrets).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "note": note,
            "message": "config.patch applied",
            "config_path": path.to_string_lossy(),
            "runtime_secrets_applied": runtime_updates,
        })
        .to_string(),
    ));
}

/// `POST /api/config/apply`  - Replace the gateway configuration entirely.
#[handler]
async fn config_apply_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() {
        channel.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid body: {err}")}).to_string(),
            ));
            return;
        }
    };

    let new_config = body
        .get("config")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let note = body
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("(no note)");

    if !new_config.is_object() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "'config' must be a JSON object"}).to_string(),
        ));
        return;
    }

    let path = config_toml_path(&channel);
    if let Err(err) = write_config_json(&path, &new_config).await {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(json!({"error": err}).to_string()));
        return;
    }

    let runtime_secrets = runtime_channel_secrets_from_config(&new_config);
    let runtime_updates = count_runtime_secrets(&runtime_secrets);
    channel.set_runtime_channel_secrets(runtime_secrets).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "note": note,
            "message": "config.apply replaced configuration",
            "config_path": path.to_string_lossy(),
            "runtime_secrets_applied": runtime_updates,
        })
        .to_string(),
    ));
}

/// `POST /api/restart`  - Request a gateway restart.
///
/// **Authentication:** Bearer token required (enforced by the global hoop)
/// **plus** the `OperatorAdmin` scope (additional check below). A regular
/// `OperatorWrite` token cannot kill the daemon.
#[handler]
async fn restart_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !require_admin_scope(req, depot, res).await {
        return;
    }
    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("operator request");
    let delay_ms = body.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(0);

    info!(reason = reason, delay_ms = delay_ms, "restart requested");
    log_store::append_log(
        "info",
        "api/restart",
        format!("restart requested: reason={reason}, delay_ms={delay_ms}"),
    )
    .await;

    // Minimal executable restart semantics:
    // schedule process exit; supervisor/service manager should restart the process.
    tokio::spawn(async move {
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        std::process::exit(0);
    });

    res.render(Text::Json(
        json!({
            "status": "ok",
            "reason": reason,
            "delay_ms": delay_ms,
            "message": "restart scheduled",
        })
        .to_string(),
    ));
}

/// `POST /api/agent`  - Invoke the agent with a text prompt.
///
/// Body: `{"session_id": "...", "message": "...", "model": "...", "system": "..."}`
#[handler]
async fn agent_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let session_store = if let Ok(store) = depot.get_typed::<Arc<SessionStore>>() {
        store.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid JSON: {err}")}).to_string(),
            ));
            return;
        }
    };

    let message = body.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("default");
    let session_id =
        match validate_uuid_v7_session_id(body.get("session_id").and_then(|v| v.as_str())) {
            Ok(value) => value,
            Err(message) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Json(json!({"error": message}).to_string()));
                return;
            }
        };

    if message.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "'message' is required"}).to_string(),
        ));
        return;
    }

    let run_id = uuid::Uuid::now_v7().to_string();
    upsert_agent_run(AgentRunRecord {
        run_id: run_id.clone(),
        status: "running".to_owned(),
        result: None,
        error: None,
        updated_at_ms: crate::json_store::now_ms(),
    })
    .await;

    // Spawn the agent invocation in the background.
    let channel_clone = channel.clone();
    let session_store_clone = session_store.clone();
    let message_owned = message.to_owned();
    let model_owned = model.to_owned();
    let session_id_owned = session_id.clone();
    let run_id_clone = run_id.clone();

    tokio::spawn(async move {
        match channel_clone
            .invoke_agent_text_in_session_with_metadata(
                &message_owned,
                &model_owned,
                session_id_owned.as_deref(),
            )
            .await
        {
            Ok(result) => {
                let provider = provider_from_model(&model_owned);
                persist_chat_session_metadata(
                    session_store_clone.as_ref(),
                    &channel_clone.config().savfox_home,
                    &result.session_id,
                    &result.thread_id,
                    &model_owned,
                    &provider,
                    result.rollout_path.as_deref(),
                    result.last_token_usage.as_ref(),
                )
                .await;

                let reply = result.reply;
                upsert_agent_run(AgentRunRecord {
                    run_id: run_id_clone.clone(),
                    status: "completed".to_owned(),
                    result: Some(reply.clone()),
                    error: None,
                    updated_at_ms: crate::json_store::now_ms(),
                })
                .await;
                info!(run_id = %run_id_clone, reply_len = reply.len(), "agent run completed");
            }
            Err(err) => {
                upsert_agent_run(AgentRunRecord {
                    run_id: run_id_clone.clone(),
                    status: "failed".to_owned(),
                    result: None,
                    error: Some(err.to_string()),
                    updated_at_ms: crate::json_store::now_ms(),
                })
                .await;
                warn!(run_id = %run_id_clone, "agent run failed: {err}");
            }
        }
    });

    res.render(Text::Json(
        json!({
            "status": "started",
            "run_id": run_id,
        })
        .to_string(),
    ));
}

/// `POST /api/agent/wait`  - Wait for an agent run to complete.
///
/// Body: `{"run_id": "...", "timeout_secs": 60}`
#[handler]
async fn agent_wait_handler(req: &mut Request, res: &mut Response) {
    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let run_id = body.get("run_id").and_then(|r| r.as_str()).unwrap_or("");
    let timeout_secs = body
        .get("timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap_or(60);

    if run_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "'run_id' is required"}).to_string(),
        ));
        return;
    }

    let timeout = std::time::Duration::from_secs(timeout_secs.min(120));
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let Some(record) = get_agent_run(run_id).await else {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Json(
                json!({
                    "status": "not_found",
                    "run_id": run_id,
                })
                .to_string(),
            ));
            return;
        };

        if record.status != "running" {
            res.render(Text::Json(
                json!({
                    "status": record.status,
                    "run_id": record.run_id,
                    "result": record.result,
                    "error": record.error,
                    "updated_at_ms": record.updated_at_ms,
                })
                .to_string(),
            ));
            return;
        }

        if tokio::time::Instant::now() >= deadline {
            res.render(Text::Json(
                json!({
                    "status": "running",
                    "run_id": record.run_id,
                    "result": record.result,
                    "error": record.error,
                    "updated_at_ms": record.updated_at_ms,
                })
                .to_string(),
            ));
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// `GET /api/sessions/:session_id/history`  - Get session chat history.
#[handler]
async fn session_history_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<String>("session_id").unwrap_or_default();
    let limit = req.query::<usize>("limit").unwrap_or(50);

    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "missing session_id in path"}).to_string(),
        ));
        return;
    }

    let session_store = if let Ok(store) = depot.get_typed::<Arc<SessionStore>>() {
        store.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let channel = if let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() {
        channel.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let payload = build_history_payload(&session_id, limit, None, &session_store, &channel).await;
    res.render(Text::Json(payload.to_string()));
}

#[handler]
async fn chat_abort_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(json!({}));
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let thread_id = body.get("thread_id").and_then(|v| v.as_str()).unwrap_or("");

    if session_id.is_empty() && thread_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id or thread_id" }).to_string(),
        ));
        return;
    }

    let channel = if let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() {
        channel.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_store = if let Ok(store) = depot.get_typed::<Arc<SessionStore>>() {
        store.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let candidates =
        resolve_abort_candidate_ids(session_store.as_ref(), Some(thread_id), Some(session_id))
            .await;
    if let Some(aborted_thread_id) =
        abort_first_active_candidate(channel.as_ref(), &candidates).await
    {
        res.render(Text::Json(
            json!({
                "status": "ok",
                "session_id": session_id,
                "thread_id": aborted_thread_id,
                "message": "chat aborted",
            })
            .to_string(),
        ));
        return;
    }

    let aborted_count = abort_all_active_threads(channel.as_ref()).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "aborted_count": aborted_count,
            "message": "chat abort attempted",
        })
        .to_string(),
    ));
}

#[handler]
async fn plugin_route_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let plugin_id = req.param::<String>("plugin_id").unwrap_or_default();
    if plugin_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "missing plugin_id in route"}).to_string(),
        ));
        return;
    }

    if !authenticate_plugin_route(req, depot, res).await {
        return;
    }

    if !plugin_route_within_limit(&plugin_id).await {
        res.status_code(StatusCode::TOO_MANY_REQUESTS);
        res.render(Text::Json(
            json!({
                "error": "plugin route rate limit exceeded",
                "plugin_id": plugin_id,
                "limit_per_minute": PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE
            })
            .to_string(),
        ));
        return;
    }

    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let plugins = match plugin::discover_snapshot(&channel.config().savfox_home).await {
        Ok(plugins) => plugins,
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({
                    "error": "failed to load plugins",
                    "details": err
                })
                .to_string(),
            ));
            return;
        }
    };

    let Some(found) = plugins.iter().find(|p| p.info.id == plugin_id) else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({"error": "plugin not found", "plugin_id": plugin_id}).to_string(),
        ));
        return;
    };

    if found.state != PluginState::Enabled {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Text::Json(
            json!({
                "error": "plugin is not enabled",
                "plugin_id": plugin_id,
                "state": found.state,
            })
            .to_string(),
        ));
        return;
    }

    let route_tail = req.param::<String>("plugin_route").unwrap_or_default();
    let payload: Value = req.parse_json().await.unwrap_or_else(|_| json!({}));

    res.render(Text::Json(
        json!({
            "status": "ok",
            "plugin_id": plugin_id,
            "route": if route_tail.is_empty() { "/".to_owned() } else { format!("/{route_tail}") },
            "request": payload,
        })
        .to_string(),
    ));
}
