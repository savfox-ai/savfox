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

use crate::auth::GatewayAuth;
use crate::bridge::{GatewayBridge, RuntimeBridgeSecrets};
use crate::chat_session::{
    abort_all_active_threads, abort_first_active_candidate, persist_chat_session_metadata,
    provider_from_model, resolve_abort_candidate_ids, validate_uuid_v7_session_id,
};
use crate::config::GatewayConfig;
use crate::cron_service::CronService;
use crate::otel::{self, GatewayMetrics};
use crate::plugin::{self, PluginState};
use crate::routing::{openai, openresponses};
use crate::session::{GatewaySessionManager, SessionStore, build_history_payload};
use crate::skills_api::{self, SkillsApiState};
use crate::ws::ws_handler;
use crate::{
    bridges, exec_approval, hooks, log_store, pairing_store, routing, static_assets, tools_invoke,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        .entry(plugin_id.to_string())
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
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return false;
        }
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
    bridge: Arc<GatewayBridge>,
    config: &GatewayConfig,
    session_store: Arc<SessionStore>,
    cron_service: Arc<CronService>,
    savfox_home: PathBuf,
) -> Router {
    let gateway_config = Arc::new(config.clone());
    let skills_state = Arc::new(SkillsApiState::new(&savfox_home));
    let metrics = Arc::new(GatewayMetrics::new());

    let state_router = Router::new()
        .hoop(affix_state::inject(auth.clone()))
        .hoop(affix_state::inject(session_mgr.clone()))
        .hoop(affix_state::inject(bridge.clone()))
        .hoop(affix_state::inject(gateway_config))
        .hoop(affix_state::inject(session_store))
        .hoop(affix_state::inject(cron_service))
        .hoop(affix_state::inject(skills_state))
        .hoop(affix_state::inject(metrics));

    let mut router = state_router
        // Health check
        .push(Router::with_path("health").get(health_handler))
        // API status
        .push(Router::with_path("api/status").get(status_handler))
        .push(Router::with_path("api/logs").get(logs_handler))
        // API config
        .push(Router::with_path("api/config").get(config_handler))
        // Metrics (OpenTelemetry observability)
        .push(Router::with_path("api/metrics").get(otel::metrics_handler))
        // Token validation
        .push(Router::with_path("api/token/validate").post(token_validate_handler))
        // Message sending endpoint (used by the `message` tool)
        .push(Router::with_path("api/message").post(message_handler))
        // Session listing endpoint (used by the `sessions` tool)
        .push(Router::with_path("api/sessions").get(sessions_handler))
        .push(Router::with_path("api/channels").get(channels_handler))
        .push(Router::with_path("api/nodes").get(nodes_handler))
        .push(Router::with_path("api/devices").get(devices_list_handler))
        .push(Router::with_path("api/devices/pair").post(devices_pair_handler))
        .push(Router::with_path("api/devices/<device_id>/revoke").post(devices_revoke_handler))
        // Gateway config management (Phase 2)
        .push(Router::with_path("api/config/patch").post(config_patch_handler))
        .push(Router::with_path("api/config/apply").post(config_apply_handler))
        .push(Router::with_path("api/restart").post(restart_handler))
        // Skills API endpoints
        .push(Router::with_path("api/skills").get(skills_api::skills_list_handler))
        .push(Router::with_path("api/skills/search").get(skills_api::skills_search_handler))
        .push(Router::with_path("api/skills/installed").get(skills_api::skills_installed_handler))
        .push(Router::with_path("api/skills/refresh").post(skills_api::skills_refresh_handler))
        .push(Router::with_path("api/skills/install").post(skills_api::skills_install_handler))
        .push(Router::with_path("api/skills/uninstall").post(skills_api::skills_uninstall_handler))
        // OpenAI-compatible API
        .push(Router::with_path("api/chat/completions").post(openai::chat_completions_handler))
        .push(Router::with_path("api/models/openai").get(openai::models_list_handler))
        // OpenResponses API (Phase 7)
        .push(Router::with_path("api/responses").post(openresponses::responses_handler))
        // Tools invoke endpoint (Phase 6)
        .push(Router::with_path("tools/invoke").post(tools_invoke::tools_invoke_handler))
        // Exec approval endpoints
        .push(Router::with_path("api/exec/approval/request").post(exec_approval::approval_request_handler))
        .push(Router::with_path("api/exec/approval/resolve").post(exec_approval::approval_resolve_handler))
        .push(Router::with_path("api/exec/approvals").get(exec_approval::approvals_list_handler))
        // Agent invocation endpoints (used by agent_step tool)
        .push(Router::with_path("api/agent").post(agent_handler))
        .push(Router::with_path("api/agent/wait").post(agent_wait_handler))
        .push(Router::with_path("api/sessions/<session_id>/history").get(session_history_handler))
        // WebSocket endpoint
        .push(Router::with_path("ws").goal(ws_handler))
        // Agents API
        .push(Router::with_path("api/agents").get(routing::agents_list_handler))
        .push(Router::with_path("api/agents").post(routing::agents_create_handler))
        .push(Router::with_path("api/agents/<agent_id>").get(routing::agents_get_handler))
        .push(Router::with_path("api/agents/<agent_id>").post(routing::agents_update_handler))
        .push(Router::with_path("api/agents/<agent_id>").delete(routing::agents_delete_handler))
        // Models API
        .push(Router::with_path("api/models").get(routing::models_list_handler))
        .push(Router::with_path("api/models/providers").get(routing::models_providers_handler))
        .push(Router::with_path("api/models/<model_id>").get(routing::models_get_handler))
        // Sessions management API
        .push(Router::with_path("api/sessions/preview").get(routing::sessions_preview_handler))
        .push(Router::with_path("api/sessions/<session_id>").post(routing::sessions_patch_handler))
        .push(Router::with_path("api/sessions/<session_id>/reset").post(routing::sessions_reset_handler))
        .push(Router::with_path("api/sessions/<session_id>").delete(routing::sessions_delete_handler))
        .push(Router::with_path("api/sessions/<session_id>/compact").post(routing::sessions_compact_handler))
        // Usage API
        .push(Router::with_path("api/usage/stats").get(routing::usage_stats_handler))
        .push(Router::with_path("api/usage/timeseries").get(routing::usage_timeseries_handler))
        .push(Router::with_path("api/usage/cost").get(routing::usage_cost_handler))
        // Cron API
        .push(Router::with_path("api/cron").get(routing::cron_list_handler))
        .push(Router::with_path("api/cron").post(routing::cron_add_handler))
        .push(Router::with_path("api/cron/<job_id>").get(routing::cron_get_handler))
        .push(Router::with_path("api/cron/<job_id>").post(routing::cron_update_handler))
        .push(Router::with_path("api/cron/<job_id>").delete(routing::cron_delete_handler))
        .push(Router::with_path("api/cron/<job_id>/run").post(routing::cron_run_handler))
        // TTS API
        .push(Router::with_path("api/tts/status").get(routing::tts_status_handler))
        .push(Router::with_path("api/tts/enable").post(routing::tts_enable_handler))
        .push(Router::with_path("api/tts/disable").post(routing::tts_disable_handler))
        .push(Router::with_path("api/tts/providers").get(routing::tts_providers_handler))
        // Voice Wake API
        .push(Router::with_path("api/voicewake/status").get(routing::voicewake_status_handler))
        .push(Router::with_path("api/voicewake/enable").post(routing::voicewake_enable_handler))
        .push(Router::with_path("api/voicewake/disable").post(routing::voicewake_disable_handler))
        // Chat abort API
        .push(Router::with_path("api/chat/abort").post(chat_abort_handler))
        // Plugin HTTP routes
        .push(Router::with_path("plugins/<plugin_id>").post(plugin_route_handler))
        .push(Router::with_path("plugins/<plugin_id>/<**plugin_route>").post(plugin_route_handler));

    // Webhook endpoints for chat platform bridges.
    let webhook_router = Router::with_path("webhooks")
        .push(Router::with_path("discord").post(bridges::discord::webhook_handler))
        .push(Router::with_path("dingtalk").post(bridges::dingtalk::webhook_handler))
        .push(Router::with_path("telegram").post(bridges::telegram::webhook_handler))
        .push(Router::with_path("slack").post(bridges::slack::webhook_handler))
        .push(Router::with_path("matrix").post(bridges::matrix::webhook_handler))
        .push(Router::with_path("msteams").post(bridges::msteams::webhook_handler))
        .push(Router::with_path("mattermost").post(bridges::mattermost::webhook_handler))
        .push(Router::with_path("googlechat").post(bridges::googlechat::webhook_handler))
        .push(Router::with_path("line").post(bridges::line::webhook_handler))
        .push(Router::with_path("feishu").post(bridges::feishu::webhook_handler))
        .push(Router::with_path("irc").post(bridges::irc::webhook_handler))
        .push(Router::with_path("webhook").post(bridges::webhook::webhook_handler))
        .push(Router::with_path("whatsapp").get(bridges::whatsapp::webhook_verification_handler))
        .push(Router::with_path("whatsapp").post(bridges::whatsapp::webhook_handler))
        .push(Router::with_path("signal").post(bridges::signal::webhook_handler))
        .push(Router::with_path("zalo").post(bridges::zalo::webhook_handler));

    router = router.push(webhook_router);

    // Hook mapping endpoints (Phase 8).
    let hooks_router = Router::with_path("hooks")
        .push(Router::with_path("wake").post(hooks::wake_handler))
        .push(Router::with_path("agent").post(hooks::agent_hook_handler))
        .push(Router::with_path("<mapping_id>").post(hooks::custom_hook_handler));

    router = router.push(hooks_router);

    // WebChat embeddable widget endpoints.
    router = router.push(webchat::webchat_router());

    // SPA fallback: serve the embedded web UI for all unmatched GET requests.
    // This must be the LAST route so API routes are matched first.
    router = router.push(Router::with_path("{**rest}").get(static_assets::spa_handler));
    router = router.push(Router::with_path("").get(static_assets::spa_handler));

    router
}

/// Start the Salvo server.
pub(crate) async fn start_server(
    config: &GatewayConfig,
    auth: Arc<GatewayAuth>,
    session_mgr: Arc<GatewaySessionManager>,
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
    cron_service: Arc<CronService>,
    savfox_home: PathBuf,
) -> std::io::Result<()> {
    bridges::runtime::set_response_footer_config(config.response_footer.clone());

    let router = build_router(
        auth,
        session_mgr,
        bridge,
        config,
        session_store,
        cron_service,
        savfox_home,
    );
    let bind_addr = format!("{}:{}", config.host, config.port);

    info!("gateway server starting on {bind_addr}");

    let acceptor = TcpListener::new(bind_addr).bind().await;
    Server::new(acceptor).serve(router).await;

    Ok(())
}

/// `GET /health`  - Simple health check.
#[handler]
async fn health_handler(res: &mut Response) {
    res.headers_mut()
        .insert(CONTENT_TYPE, "application/json".parse().unwrap());

    let body = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    });

    res.render(Text::Json(body.to_string()));
}

/// `GET /api/status`  - Server status (connected clients, etc.).
#[handler]
async fn status_handler(depot: &mut Depot, res: &mut Response) {
    let session_mgr = match depot.obtain::<Arc<GatewaySessionManager>>() {
        Ok(mgr) => mgr.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let count = session_mgr.session_count().await;
    let session_ids = session_mgr.session_ids().await;
    let audit_summary = crate::security_audit::run_audit(&bridge.config().savfox_home)
        .await
        .summary;
    let plugins = plugin::discover_snapshot(&bridge.config().savfox_home)
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
    let session_mgr = match depot.obtain::<Arc<GatewaySessionManager>>() {
        Ok(mgr) => mgr.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let count = session_mgr.session_count().await;

    // Return sanitized config  - no tokens/secrets
    let body = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "connected_clients": count,
        "bridges": {
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
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    // Accept token from Authorization header or JSON body.
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_owned())
        .or_else(|| {
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

    match auth.validate(&token).await {
        Some(info) => {
            res.render(Text::Json(
                json!({
                    "valid": true,
                    "label": info.label,
                    "scopes": info.scopes,
                })
                .to_string(),
            ));
        }
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(json!({"valid": false}).to_string()));
        }
    }
}

/// `POST /api/message`  - Send a message to a chat platform channel.
///
/// Accepts JSON body: `{"channel": "discord:12345", "text": "Hello!"}`
#[handler]
async fn message_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    let channel = body.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");

    if channel.is_empty() || text.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "both 'channel' and 'text' are required"}).to_string(),
        ));
        return;
    }

    match bridge
        .send_platform_message(channel, text, None, None, None)
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
    let session_mgr = match depot.obtain::<Arc<GatewaySessionManager>>() {
        Ok(mgr) => mgr.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let runtime = bridge.runtime_bridge_secrets().await;
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

fn config_toml_path(bridge: &GatewayBridge) -> std::path::PathBuf {
    bridge.config().savfox_home.join("config.toml")
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
        Value::String(s) => {
            if s.contains('$') {
                *s = expand_env_string(s)?;
            }
        }
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
                        let resolved = std::env::var(var_name)
                            .ok()
                            .and_then(|v| if v.is_empty() { None } else { Some(v) });
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
                                        err_msg.to_string()
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
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        let orig: String = chars[i..end].iter().collect();
                        result.push_str(&orig);
                    }
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
    use serde_json::json;

    use super::{expand_env_string, substitute_env_vars};

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
    let toml_value = savfox_utils_json_to_toml::json_to_toml(config.clone());
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

fn runtime_bridge_secrets_from_config(config: &Value) -> RuntimeBridgeSecrets {
    RuntimeBridgeSecrets {
        discord_bot_token: json_path_str(config, &["gateway", "bridges", "discord", "bot_token"])
            .map(ToOwned::to_owned),
        telegram_bot_token: json_path_str(config, &["gateway", "bridges", "telegram", "bot_token"])
            .map(ToOwned::to_owned),
        slack_bot_token: json_path_str(config, &["gateway", "bridges", "slack", "bot_token"])
            .map(ToOwned::to_owned),
        slack_signing_secret: json_path_str(
            config,
            &["gateway", "bridges", "slack", "signing_secret"],
        )
        .map(ToOwned::to_owned),
        webhook_secret: json_path_str(config, &["gateway", "bridges", "webhook", "secret"])
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
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(bridge) => bridge.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    let path = config_toml_path(&bridge);
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

    let runtime_secrets = runtime_bridge_secrets_from_config(&current);
    let runtime_updates = count_runtime_secrets(&runtime_secrets);
    bridge.set_runtime_bridge_secrets(runtime_secrets).await;

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
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(bridge) => bridge.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    let path = config_toml_path(&bridge);
    if let Err(err) = write_config_json(&path, &new_config).await {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(json!({"error": err}).to_string()));
        return;
    }

    let runtime_secrets = runtime_bridge_secrets_from_config(&new_config);
    let runtime_updates = count_runtime_secrets(&runtime_secrets);
    bridge.set_runtime_bridge_secrets(runtime_secrets).await;

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
#[handler]
async fn restart_handler(req: &mut Request, res: &mut Response) {
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
    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };
    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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
        status: "running".to_string(),
        result: None,
        error: None,
        updated_at_ms: now_ms(),
    })
    .await;

    // Spawn the agent invocation in the background.
    let bridge_clone = bridge.clone();
    let session_store_clone = session_store.clone();
    let message_owned = message.to_owned();
    let model_owned = model.to_owned();
    let session_id_owned = session_id.clone();
    let run_id_clone = run_id.clone();

    tokio::spawn(async move {
        match bridge_clone
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
                    &bridge_clone.config().savfox_home,
                    &result.session_id,
                    &result.session_id,
                    &model_owned,
                    &provider,
                    result.rollout_path.as_deref(),
                    result.last_token_usage.as_ref(),
                )
                .await;

                let reply = result.reply;
                upsert_agent_run(AgentRunRecord {
                    run_id: run_id_clone.clone(),
                    status: "completed".to_string(),
                    result: Some(reply.clone()),
                    error: None,
                    updated_at_ms: now_ms(),
                })
                .await;
                info!(run_id = %run_id_clone, reply_len = reply.len(), "agent run completed");
            }
            Err(err) => {
                upsert_agent_run(AgentRunRecord {
                    run_id: run_id_clone.clone(),
                    status: "failed".to_string(),
                    result: None,
                    error: Some(err.to_string()),
                    updated_at_ms: now_ms(),
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

    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(bridge) => bridge.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let payload = build_history_payload(&session_id, limit, None, &session_store, &bridge).await;
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

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(bridge) => bridge.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let candidates =
        resolve_abort_candidate_ids(session_store.as_ref(), Some(thread_id), Some(session_id))
            .await;
    if let Some(aborted_thread_id) =
        abort_first_active_candidate(bridge.as_ref(), &candidates).await
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

    let aborted_count = abort_all_active_threads(bridge.as_ref()).await;

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

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let plugins = match plugin::discover_snapshot(&bridge.config().savfox_home).await {
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
            "route": if route_tail.is_empty() { "/".to_string() } else { format!("/{route_tail}") },
            "request": payload,
        })
        .to_string(),
    ));
}
