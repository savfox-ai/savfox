use std::sync::Arc;

use salvo::prelude::*;
use savfox_protocol::SessionId;
use serde_json::json;

use crate::auth::GatewayAuth;
use crate::channel::GatewayChannel;
use crate::claude_code::Runtime;
use crate::session::{GatewaySessionManager, SessionStore};

/// Use the same credential subject and ownership policy as WebSocket RPC and
/// history reads. Scope checks alone do not grant access to another owner's
/// private Claude session.
fn session_access(req: &Request, depot: &Depot, res: &mut Response) -> Option<(Runtime, String)> {
    let Some(token) = req
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        res.status_code(StatusCode::UNAUTHORIZED);
        return None;
    };
    let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return None;
    };
    Some((
        channel.claude_runtime.clone(),
        GatewayAuth::token_subject(token),
    ))
}

async fn authorize_http_session(
    req: &Request,
    depot: &Depot,
    res: &mut Response,
    session_id: &str,
) -> bool {
    let Some((runtime, principal)) = session_access(req, depot, res) else {
        return false;
    };
    if let Err(error) = runtime
        .authorize_session(Some(&principal), session_id)
        .await
    {
        res.status_code(StatusCode::FORBIDDEN);
        res.render(Text::Json(
            json!({ "error": error.to_string() }).to_string(),
        ));
        return false;
    }
    true
}

#[derive(serde::Deserialize)]
struct SessionPatch {
    label: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
}

#[handler]
pub async fn sessions_patch_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<String>("session_id").unwrap_or_default();
    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    }

    if !authorize_http_session(req, depot, res, &session_id).await {
        return;
    }

    let body = match req.parse_json::<SessionPatch>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({ "error": format!("invalid JSON: {err}") }).to_string(),
            ));
            return;
        }
    };

    let session_store = if let Ok(store) = depot.get_typed::<Arc<SessionStore>>() {
        store.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let updated = session_store
        .try_update(&session_id, |entry| {
            if let Some(label) = body.label {
                entry.label = Some(label);
            }
            if let Some(pinned) = body.pinned {
                entry.pinned = pinned;
            }
            if let Some(archived) = body.archived {
                entry.archived = archived;
            }
        })
        .await;
    let updated = match updated {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(%session_id, %error, "failed to persist session patch");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({ "error": "failed to persist session patch" }).to_string(),
            ));
            return;
        }
    };
    if updated.is_none() {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({ "error": "session not found" }).to_string(),
        ));
        return;
    }

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session patched",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_reset_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(session_id) = req.param::<SessionId>("session_id") else {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    };
    if !authorize_http_session(req, depot, res, &session_id.to_string()).await {
        return;
    }

    let session_mgr = if let Ok(mgr) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        mgr.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    session_mgr.remove_session(&session_id).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session reset",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_delete_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(session_id) = req.param::<SessionId>("session_id") else {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    };
    if !authorize_http_session(req, depot, res, &session_id.to_string()).await {
        return;
    }

    let session_mgr = if let Ok(mgr) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        mgr.clone()
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

    session_mgr.remove_session(&session_id).await;
    session_store.delete(&session_id).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session deleted",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_preview_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let limit = req.query::<usize>("limit").unwrap_or(10).min(500);
    let Some((runtime, principal)) = session_access(req, depot, res) else {
        return;
    };

    let session_store = if let Ok(store) = depot.get_typed::<Arc<SessionStore>>() {
        store.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let mut candidates = session_store.list().await;
    candidates.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
    let mut sessions = Vec::new();
    for entry in candidates {
        if sessions.len() == limit {
            break;
        }
        if runtime
            .authorize_session(Some(&principal), &entry.session_id)
            .await
            .is_ok()
        {
            sessions.push(entry);
        }
    }

    res.render(Text::Json(
        json!({
            "sessions": sessions,
            "count": sessions.len(),
        })
        .to_string(),
    ));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use salvo::test::{ResponseExt as _, TestClient};
    use savfox_core::config::ConfigBuilder;
    use serde_json::Value;

    use super::*;
    use crate::auth::{TokenInfo, TokenScope};
    use crate::channel::GatewayBridgeArgs;
    use crate::claude_code::Execution;
    use crate::config::GatewayConfig;
    use crate::cron_service::CronService;
    use crate::session::{ClientSession, SessionEntry};

    struct Fixture {
        home: tempfile::TempDir,
        channel: Arc<GatewayChannel>,
        manager: Arc<GatewaySessionManager>,
        store: Arc<SessionStore>,
        auth: Arc<GatewayAuth>,
        service: Service,
    }

    impl Fixture {
        async fn new() -> Self {
            let home = tempfile::tempdir().expect("tempdir");
            for root in [home.path().to_path_buf(), home.path().join("skills")] {
                std::fs::create_dir_all(root.join("registry/github.com/savhub-ai/registry"))
                    .unwrap();
            }
            let config = ConfigBuilder::default()
                .savfox_home(home.path().to_path_buf())
                .build()
                .await
                .expect("config");
            let manager = Arc::new(GatewaySessionManager::new());
            let store = Arc::new(SessionStore::from_home(home.path()));
            let channel = Arc::new(GatewayChannel::new(GatewayBridgeArgs {
                config: Arc::new(config),
                session_store: store.clone(),
                cli_overrides: Vec::new(),
                cloud_requirements: Default::default(),
                websocket_manager: (*manager).clone(),
                channel_registry: crate::channels::create_channel_registry(),
                channel_recovery_registry:
                    crate::channels::recovery::create_channel_recovery_registry(),
                channel_recovery_supervisors:
                    crate::channels::recovery::create_channel_recovery_supervisors(),
            }));
            let auth = Arc::new(GatewayAuth::new(HashMap::from([
                ("owner-token".to_owned(), Self::token()),
                ("other-token".to_owned(), Self::token()),
            ])));
            let service = Service::new(crate::server::build_router(
                auth.clone(),
                manager.clone(),
                channel.clone(),
                &GatewayConfig::default(),
                store.clone(),
                Arc::new(CronService::from_home(home.path())),
                home.path().to_path_buf(),
            ));
            Self {
                home,
                channel,
                manager,
                store,
                auth,
                service,
            }
        }

        fn token() -> TokenInfo {
            // Same labels must not confer ownership of another credential's session.
            TokenInfo {
                label: "same-label".to_owned(),
                scopes: vec![TokenScope::OperatorRead, TokenScope::OperatorWrite],
            }
        }

        async fn seed(&self, token: &str, updated_at: u64) -> String {
            let mut entry = SessionEntry::new();
            entry.updated_at = updated_at;
            let id = entry.session_id.clone();
            self.store.upsert(entry).await;
            let owner = GatewayAuth::token_subject(token);
            self.channel
                .claude_runtime
                .begin(
                    Execution {
                        owner: owner.clone(),
                        request_id: id.clone(),
                        session_id: id.clone(),
                        agent_id: "claude".to_owned(),
                        approval_client: false,
                        trusted_context: None,
                    },
                    "test-input".to_owned(),
                )
                .await
                .expect("claim ownership");
            let (sender, _receiver) = tokio::sync::mpsc::channel(1);
            let mut client = ClientSession::new(
                SessionId::from_string(&id).expect("session id"),
                Self::token(),
                sender,
            );
            client.principal = owner;
            self.manager.add_session(client).await;
            id
        }
    }

    #[tokio::test]
    async fn rest_session_routes_enforce_owner_and_persist_ui_state() {
        let fixture = Fixture::new().await;
        let own = fixture.seed("owner-token", 1).await;
        let other = fixture.seed("other-token", 2).await;
        let unauthenticated = TestClient::get("http://localhost/api/sessions/preview")
            .send(&fixture.service)
            .await;
        assert_eq!(unauthenticated.status_code, Some(StatusCode::UNAUTHORIZED));
        let mut preview = TestClient::get("http://localhost/api/sessions/preview?limit=1")
            .add_header("authorization", "Bearer owner-token", true)
            .send(&fixture.service)
            .await;
        assert_eq!(preview.status_code, Some(StatusCode::OK));
        let preview: Value = preview.take_json().await.expect("preview");
        assert_eq!(preview["count"], 1);
        assert_eq!(preview["sessions"][0]["sessionId"], own);
        assert_eq!(preview["sessions"][0]["pinned"], false);

        let patch = TestClient::post(format!("http://localhost/api/sessions/{other}"))
            .add_header("authorization", "Bearer owner-token", true)
            .add_header("content-type", "application/json", true)
            .body(r#"{"label":"stolen","pinned":true,"archived":true}"#)
            .send(&fixture.service)
            .await;
        assert_eq!(patch.status_code, Some(StatusCode::FORBIDDEN));
        let reset = TestClient::post(format!("http://localhost/api/sessions/{other}/reset"))
            .add_header("authorization", "Bearer owner-token", true)
            .send(&fixture.service)
            .await;
        assert_eq!(reset.status_code, Some(StatusCode::FORBIDDEN));
        let delete = TestClient::delete(format!("http://localhost/api/sessions/{other}"))
            .add_header("authorization", "Bearer owner-token", true)
            .send(&fixture.service)
            .await;
        assert_eq!(delete.status_code, Some(StatusCode::FORBIDDEN));
        let preserved = fixture
            .store
            .get(&other)
            .await
            .expect("other session preserved");
        assert!(preserved.label.is_none());
        assert!(!preserved.pinned);
        assert!(!preserved.archived);
        assert!(
            fixture
                .manager
                .session_ids()
                .await
                .iter()
                .any(|id| id.to_string() == other)
        );

        let patch = TestClient::post(format!("http://localhost/api/sessions/{own}"))
            .add_header("authorization", "Bearer owner-token", true)
            .add_header("content-type", "application/json", true)
            .body(r#"{"label":"mine","pinned":true,"archived":true}"#)
            .send(&fixture.service)
            .await;
        assert_eq!(patch.status_code, Some(StatusCode::OK));
        let persisted = SessionStore::from_home(fixture.home.path())
            .get(&own)
            .await
            .expect("persisted");
        assert_eq!(persisted.label.as_deref(), Some("mine"));
        assert!(persisted.pinned);
        assert!(persisted.archived);
        let reset = TestClient::post(format!("http://localhost/api/sessions/{own}/reset"))
            .add_header("authorization", "Bearer owner-token", true)
            .send(&fixture.service)
            .await;
        assert_eq!(reset.status_code, Some(StatusCode::OK));
        assert!(
            !fixture
                .manager
                .session_ids()
                .await
                .iter()
                .any(|id| id.to_string() == own)
        );
        let delete = TestClient::delete(format!("http://localhost/api/sessions/{own}"))
            .add_header("authorization", "Bearer owner-token", true)
            .send(&fixture.service)
            .await;
        assert_eq!(delete.status_code, Some(StatusCode::OK));
        assert!(fixture.store.get(&own).await.is_none());
        assert!(fixture.store.get(&other).await.is_some());
    }

    #[tokio::test]
    async fn browser_dispatch_denies_process_configuration_before_side_effects() {
        let fixture = Fixture::new().await;
        let cron = Arc::new(CronService::from_home(fixture.home.path()));
        for method in [
            "browser.profiles.create",
            "browser.profiles.delete",
            "browser.profiles.default.set",
            "browser.download",
        ] {
            let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":{
                "profile":"unsafe", "executable_path":"arbitrary-program", "args":["untrusted"],
                "user_data_dir":fixture.home.path().join("must-not-exist").display().to_string(),
                "download_dir":fixture.home.path().join("must-not-exist").display().to_string(), "selector":"a"
            }}).to_string();
            let reply = crate::ws_rpc::dispatch_rpc(
                &request,
                "not-connected",
                &fixture.auth,
                &fixture.manager,
                &fixture.channel,
                &fixture.store,
                &cron,
                &Fixture::token(),
            )
            .await;
            let reply: Value = serde_json::from_str(&reply).expect("RPC reply");
            assert_eq!(
                reply["error"]["code"],
                savfox_gateway_shared::PERMISSION_DENIED,
                "{method}: {reply}"
            );
        }
        assert!(!fixture.home.path().join("browser-profiles.json").exists());
        assert!(!fixture.home.path().join("must-not-exist").exists());
    }

    #[tokio::test]
    async fn rest_session_patch_reports_disk_failure_without_changing_cached_flags() {
        let fixture = Fixture::new().await;
        let own = fixture.seed("owner-token", 1).await;
        let metadata = fixture.home.path().join("gateway-session-store.json");
        std::fs::remove_file(&metadata).expect("remove metadata file");
        std::fs::create_dir(&metadata).expect("block durable replacement");
        let response = TestClient::post(format!("http://localhost/api/sessions/{own}"))
            .add_header("authorization", "Bearer owner-token", true)
            .add_header("content-type", "application/json", true)
            .body(r#"{"label":"must-not-stick","pinned":true,"archived":true}"#)
            .send(&fixture.service)
            .await;
        assert_eq!(
            response.status_code,
            Some(StatusCode::INTERNAL_SERVER_ERROR)
        );
        let cached = fixture.store.get(&own).await.expect("cached session");
        assert!(cached.label.is_none());
        assert!(!cached.pinned);
        assert!(!cached.archived);
    }

    #[tokio::test]
    async fn browser_request_dispatch_blocks_private_post_and_custom_header_get() {
        let fixture = Fixture::new().await;
        let cron = Arc::new(CronService::from_home(fixture.home.path()));
        for method in ["GET", "POST"] {
            let request = json!({"jsonrpc":"2.0","id":1,"method":"browser.request","params":{
                "url":"http://127.0.0.1:9/private", "method":method,
                "use_browser_context":false, "headers":{}
            }})
            .to_string();
            let reply = crate::ws_rpc::dispatch_rpc(
                &request,
                "not-connected",
                &fixture.auth,
                &fixture.manager,
                &fixture.channel,
                &fixture.store,
                &cron,
                &Fixture::token(),
            )
            .await;
            let reply: Value = serde_json::from_str(&reply).expect("RPC reply");
            assert_eq!(
                reply["error"]["code"],
                savfox_gateway_shared::INVALID_PARAMS,
                "{method}: {reply}"
            );
            assert!(
                reply["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("blocked")
            );
        }
    }

    #[tokio::test]
    async fn production_health_and_dispatcher_return_protocol_responses() {
        let fixture = Fixture::new().await;
        let mut response = TestClient::get("http://localhost/health")
            .send(&fixture.service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let health: Value = response.take_json().await.expect("health JSON");
        assert_eq!(health["status"], "ok");
        assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
        let cron = Arc::new(CronService::from_home(fixture.home.path()));
        let operator = TokenInfo {
            label: "operator".to_owned(),
            scopes: vec![TokenScope::Operator],
        };
        let reply = crate::ws_rpc::dispatch_rpc(
            r#"{"jsonrpc":"2.0","id":"unknown-request","method":"quality.unknown","params":{}}"#,
            "not-connected",
            &fixture.auth,
            &fixture.manager,
            &fixture.channel,
            &fixture.store,
            &cron,
            &operator,
        )
        .await;
        let reply: Value = serde_json::from_str(&reply).expect("RPC JSON");
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], "unknown-request");
        assert_eq!(
            reply["error"]["code"],
            savfox_gateway_shared::METHOD_NOT_FOUND
        );
        assert!(reply.get("result").is_none());
    }

    #[tokio::test]
    async fn authenticated_websocket_dispatches_models_list() {
        use futures_util::{SinkExt as _, StreamExt as _};
        use salvo::conn::Acceptor as _;
        use tokio_tungstenite::tungstenite::Message;

        let fixture = Fixture::new().await;
        let router = crate::server::build_router(
            fixture.auth.clone(),
            fixture.manager.clone(),
            fixture.channel.clone(),
            &GatewayConfig::default(),
            fixture.store.clone(),
            Arc::new(CronService::from_home(fixture.home.path())),
            fixture.home.path().to_path_buf(),
        );
        let acceptor = TcpListener::new("127.0.0.1:0").bind().await;
        let address = acceptor.holdings()[0]
            .local_addr
            .clone()
            .into_std()
            .expect("TCP address");
        let server = Server::new(acceptor);
        let shutdown = server.handle();
        let server_task = tokio::spawn(async move { server.serve(router).await });
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/ws?token=owner-token"))
                .await
                .expect("authenticated connection");
        let connected = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("connected timeout")
            .expect("connected frame")
            .expect("read");
        let connected: Value =
            serde_json::from_str(connected.to_text().expect("text")).expect("connected JSON");
        assert_eq!(connected["type"], "connected");
        socket
            .send(Message::text(
                r#"{"jsonrpc":"2.0","id":"models-request","method":"models.list","params":{}}"#,
            ))
            .await
            .expect("RPC request");
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("RPC timeout")
            .expect("RPC frame")
            .expect("read");
        let reply: Value = serde_json::from_str(reply.to_text().expect("text")).expect("RPC JSON");
        assert_eq!(reply["id"], "models-request");
        assert!(reply["result"]["models"].is_array(), "{reply}");
        assert!(reply.get("error").is_none());
        socket.close(None).await.expect("close socket");
        shutdown.stop_graceful(None);
        server_task.await.expect("server stopped");
    }
}
