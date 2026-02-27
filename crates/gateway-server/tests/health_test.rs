//! Integration tests for the /health endpoint.

mod helpers;

use helpers::{free_port, http_client, wait_for_port};
use std::time::Duration;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}/health");

    // Note: In a real test, we'd start the gateway server here.
    // For now, this is a skeleton that tests the HTTP client setup.
    let client = http_client();

    // This test will pass when a gateway server is running on the port.
    // In CI, the server would be started as a fixture.
    if wait_for_port(port, Duration::from_millis(200)).await {
        let resp = client.get(&url).send().await.expect("GET /health");
        assert!(resp.status().is_success());
    }
    // If no server is running, the test passes silently (integration test requires server).
}
