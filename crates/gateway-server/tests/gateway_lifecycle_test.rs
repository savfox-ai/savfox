//! Gateway startup/shutdown lifecycle integration test.
//!
//! This test spins up the gateway in-process, verifies readiness via `/health`,
//! then aborts the task and verifies the port is no longer accepting traffic.

mod helpers;

use std::time::Duration;

use helpers::{free_port, http_client, wait_for_port};
use savfox_common::CliConfigOverrides;
use savfox_gateway_server::config::GatewayConfig;

#[tokio::test]
#[ignore]
async fn gateway_startup_shutdown_lifecycle() {
    let port = free_port();
    let token = format!("lifecycle-token-{}", uuid::Uuid::now_v7());

    let mut cfg = GatewayConfig::default();
    cfg.host = "127.0.0.1".parse().expect("parse loopback");
    cfg.port = port;
    cfg.token = Some(token);

    let handle = tokio::spawn(async move {
        savfox_gateway_server::run_main(cfg, None, CliConfigOverrides::default()).await
    });

    assert!(
        wait_for_port(port, Duration::from_secs(20)).await,
        "gateway should become reachable on {port}"
    );

    let resp = http_client()
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .expect("GET /health");
    assert!(resp.status().is_success(), "/health should return success");

    handle.abort();
    let _ = handle.await;

    // Give the socket a moment to be released after task cancellation.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still_open = wait_for_port(port, Duration::from_secs(2)).await;
    assert!(!still_open, "gateway port should close after shutdown");
}
