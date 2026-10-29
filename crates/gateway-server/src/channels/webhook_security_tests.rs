use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use salvo::prelude::*;
use salvo::test::TestClient;
use savfox_core::config::ConfigBuilder;
use serde_json::json;
use sha2::Sha256;

use crate::auth::GatewayAuth;
use crate::channel::{GatewayBridgeArgs, GatewayChannel};
use crate::config::GatewayConfig;
use crate::cron_service::CronService;
use crate::session::{GatewaySessionManager, SessionStore};

const PLATFORMS: &[&str] = &[
    "discord",
    "dingtalk",
    "telegram",
    "slack",
    "matrix",
    "msteams",
    "mattermost",
    "googlechat",
    "line",
    "feishu",
    "irc",
    "webhook",
    "whatsapp",
    "qq",
    "wechat",
    "signal",
    "zalo",
];

async fn service(home: &std::path::Path, gateway: &GatewayConfig) -> Service {
    for root in [home.to_path_buf(), home.join("skills")] {
        std::fs::create_dir_all(root.join("registry/github.com/savhub-ai/registry")).unwrap();
    }
    let config = ConfigBuilder::default()
        .savfox_home(home.to_path_buf())
        .build()
        .await
        .unwrap();
    let sessions = Arc::new(GatewaySessionManager::new());
    let store = Arc::new(SessionStore::from_home(home));
    let channel = Arc::new(GatewayChannel::new(GatewayBridgeArgs {
        config: Arc::new(config),
        session_store: store.clone(),
        cli_overrides: vec![],
        cloud_requirements: Default::default(),
        websocket_manager: (*sessions).clone(),
        channel_registry: super::create_channel_registry(),
        channel_recovery_registry: super::recovery::create_channel_recovery_registry(),
        channel_recovery_supervisors: super::recovery::create_channel_recovery_supervisors(),
    }));
    Service::new(crate::server::build_router(
        Arc::new(GatewayAuth::single_token("operator".to_owned())),
        sessions,
        channel,
        gateway,
        store,
        Arc::new(CronService::from_home(home)),
        home.to_path_buf(),
    ))
}

fn save_channel(home: &std::path::Path, platform: &str, enabled: bool, secret: Option<&str>) {
    std::fs::create_dir_all(home.join("channels")).unwrap();
    std::fs::write(
        home.join("channels").join(format!("{platform}.json")),
        json!({
            "id":platform,"kind":platform,"name":platform,"enabled":enabled,
            "config":{"webhook_secret":secret}
        })
        .to_string(),
    )
    .unwrap();
}

fn signature(body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[tokio::test]
async fn public_webhook_routes_require_enabled_channel_and_verification_material() {
    let home = tempfile::tempdir().unwrap();
    let service = service(home.path(), &GatewayConfig::default()).await;
    for platform in PLATFORMS {
        let response = TestClient::post(format!("http://localhost/webhooks/{platform}"))
            .add_header("content-type","application/json",true)
            .body(r#"{"action":"start_thread","channel":"attacker","prompt":"do work","message":"!savfox do work"}"#)
            .send(&service).await;
        assert_eq!(
            response.status_code,
            Some(StatusCode::SERVICE_UNAVAILABLE),
            "unconfigured {platform}"
        );
        save_channel(home.path(), platform, true, None);
    }
    for platform in PLATFORMS {
        let response = TestClient::post(format!("http://localhost/webhooks/{platform}"))
            .add_header("content-type", "application/json", true)
            .body("{}")
            .send(&service)
            .await;
        if *platform == "feishu" {
            assert!(
                !response
                    .status_code
                    .is_some_and(|status| status.is_success())
            );
        } else {
            assert_eq!(
                response.status_code,
                Some(StatusCode::UNAUTHORIZED),
                "enabled {platform} without verification material"
            );
        }
    }
}

#[tokio::test]
async fn signed_webhook_routes_reject_missing_forged_and_tampered_signatures() {
    let home = tempfile::tempdir().unwrap();
    let gateway: GatewayConfig = serde_json::from_value(json!({"channels":{
        "webhook":{"enabled":true,"secret":"test-secret"},
        "zalo":{"enabled":true,"app_id":"app","app_secret":"test-secret","access_token":"token"},
        "signal":{"enabled":true},
        "msteams":{"enabled":true,"app_id":"app","app_password":"test-secret"}
    }}))
    .unwrap();
    let service = service(home.path(), &gateway).await;
    let forged_bearer = TestClient::post("http://localhost/webhooks/msteams")
        .add_header("content-type", "application/json", true)
        .add_header("authorization", "Bearer arbitrary-unverified-token", true)
        .body("{}")
        .send(&service)
        .await;
    assert_eq!(forged_bearer.status_code, Some(StatusCode::UNAUTHORIZED));
    for platform in ["irc", "signal", "dingtalk", "matrix"] {
        save_channel(home.path(), platform, true, Some("test-secret"));
    }
    for platform in ["irc", "signal", "dingtalk", "matrix", "webhook", "zalo"] {
        let header = if platform == "zalo" {
            "x-zalooa-signature"
        } else {
            "x-hub-signature-256"
        };
        let valid = if platform == "zalo" {
            signature("{}").trim_start_matches("sha256=").to_owned()
        } else {
            signature("{}")
        };
        for (body, sig, expected) in [
            ("{}", None, StatusCode::UNAUTHORIZED),
            ("{}", Some("forged"), StatusCode::UNAUTHORIZED),
            (
                r#"{"changed":true}"#,
                Some(valid.as_str()),
                StatusCode::UNAUTHORIZED,
            ),
            ("{}", Some(valid.as_str()), StatusCode::OK),
        ] {
            let mut request = TestClient::post(format!("http://localhost/webhooks/{platform}"))
                .add_header("content-type", "application/json", true)
                .body(body);
            if let Some(sig) = sig {
                request = request.add_header(header, sig, true);
            }
            let response = request.send(&service).await;
            assert_eq!(
                response.status_code,
                Some(expected),
                "{platform} body={body} signature={sig:?}"
            );
        }
    }
    // A saved disabled instance overrides the legacy enabled configuration.
    save_channel(home.path(), "signal", false, Some("test-secret"));
    let response = TestClient::post("http://localhost/webhooks/signal")
        .add_header("content-type", "application/json", true)
        .add_header("x-hub-signature-256", signature("{}"), true)
        .body("{}")
        .send(&service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::SERVICE_UNAVAILABLE));
}
