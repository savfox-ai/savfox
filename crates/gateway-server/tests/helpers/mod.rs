//! Shared test helpers for gateway integration tests.

use std::net::TcpListener;
use std::time::Duration;

/// Find a free TCP port on localhost.
pub(crate) fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to free port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// Wait until a TCP port is accepting connections, with a timeout.
pub(crate) async fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    loop {
        if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .is_ok()
        {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Build an HTTP client with common defaults.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build HTTP client")
}
