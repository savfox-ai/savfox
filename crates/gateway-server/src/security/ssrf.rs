//! SSRF protection helpers for outbound HTTP access.
//!
//! This module provides:
//! - private/metadata IP and hostname blocking
//! - allowlist/blocklist hostname policy
//! - DNS resolution with pinned IP validation
//! - guarded fetch with redirect re-validation

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use reqwest::header::LOCATION;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

const DEFAULT_MAX_REDIRECTS: usize = 5;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
/// Hard bound for generic outbound downloads; callers with smaller policies
/// (browser text or media stores) should use `guarded_fetch_with_limit`.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsrfConfig {
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub blocklist: Vec<String>,
    #[serde(default = "default_max_redirects")]
    pub max_redirects: usize,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_max_redirects() -> usize {
    DEFAULT_MAX_REDIRECTS
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

impl Default for SsrfConfig {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            blocklist: Vec::new(),
            max_redirects: default_max_redirects(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

impl SsrfConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let allowlist = std::env::var("SAVFOX_SSRF_ALLOWLIST")
            .ok()
            .map(|v| split_csv(&v))
            .unwrap_or_default();
        let blocklist = std::env::var("SAVFOX_SSRF_BLOCKLIST")
            .ok()
            .map(|v| split_csv(&v))
            .unwrap_or_default();
        let max_redirects = std::env::var("SAVFOX_SSRF_MAX_REDIRECTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_REDIRECTS);
        let timeout_ms = std::env::var("SAVFOX_SSRF_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        Self {
            allowlist,
            blocklist,
            max_redirects,
            timeout_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuardedFetchResponse {
    pub final_url: String,
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum SsrfError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("missing host in URL")]
    MissingHost,
    #[error("blocked host: {0}")]
    BlockedHost(String),
    #[error("blocked ip: {0}")]
    BlockedIp(String),
    #[error("dns resolve failed for {host}: {reason}")]
    DnsResolve { host: String, reason: String },
    #[error("redirect limit exceeded ({0})")]
    RedirectLimitExceeded(usize),
    #[error("request timed out after {0}ms")]
    Timeout(u64),
    #[error("http error: {0}")]
    Http(String),
    #[error("response body exceeds {limit} bytes")]
    BodyTooLarge { limit: usize },
}

#[must_use]
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
            {
                return true;
            }
            // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
            // addresses must be evaluated against the IPv4 ruleset, otherwise
            // e.g. `::ffff:169.254.169.254` or `::ffff:127.0.0.1` would slip
            // through the IPv6-only checks below and reach internal/metadata
            // endpoints (SSRF).
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            if let Some(v4) = v6.to_ipv4() {
                return is_private_ip(IpAddr::V4(v4));
            }
            false
        }
    }
}

#[must_use]
pub fn is_metadata_hostname(host: &str) -> bool {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    host == "metadata.google.internal" || host == "169.254.169.254"
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

fn host_matches_pattern(host: &str, pattern: &str) -> bool {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    let pattern = pattern.trim().trim_matches('.').to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    host == pattern
}

fn host_blocked_by_policy(host: &str, cfg: &SsrfConfig) -> bool {
    let host = host.to_ascii_lowercase();
    if is_metadata_hostname(&host) {
        return true;
    }
    if cfg
        .blocklist
        .iter()
        .any(|pattern| host_matches_pattern(&host, pattern))
    {
        return true;
    }
    if cfg.allowlist.is_empty() {
        return false;
    }
    !cfg.allowlist
        .iter()
        .any(|pattern| host_matches_pattern(&host, pattern))
}

fn validate_ip(ip: IpAddr) -> Result<(), SsrfError> {
    if is_private_ip(ip) {
        warn!(ip = %ip, "ssrf blocked private/link-local/loopback ip");
        return Err(SsrfError::BlockedIp(ip.to_string()));
    }
    if ip.to_string() == "169.254.169.254" {
        warn!(ip = %ip, "ssrf blocked metadata ip");
        return Err(SsrfError::BlockedIp(ip.to_string()));
    }
    Ok(())
}

pub fn build_guarded_client(cfg: &SsrfConfig) -> Result<reqwest::Client, SsrfError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(cfg.timeout_ms.max(1)))
        .build()
        .map_err(|e| SsrfError::Http(e.to_string()))
}

/// Build a `reqwest::Client` with the IP address resolved for `url`'s host
/// **pinned** into the resolver, so the underlying TCP connection is forced
/// to use the IP we already validated rather than re-resolving DNS at
/// request time.
///
/// This is the defence against DNS rebinding (S10): an attacker controls
/// `attacker.com` so that the validation lookup returns a public IP (passes
/// the SSRF guard), and the subsequent connection lookup returns
/// `127.0.0.1`. By feeding the validated `(host, IP)` pair directly into
/// `Client::builder().resolve(...)` we close the window — reqwest skips
/// system DNS for that host entirely.
///
/// Callers that previously did `build_guarded_client(cfg)` followed by
/// `client.post(url).send()` should switch to
/// `build_pinned_client(url, cfg)` so the validation and the connection
/// agree on the destination IP.
pub async fn build_pinned_client(
    url: &str,
    cfg: &SsrfConfig,
) -> Result<reqwest::Client, SsrfError> {
    let parsed = Url::parse(url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(SsrfError::UnsupportedScheme(other.to_owned())),
    }
    let host = parsed.host_str().ok_or(SsrfError::MissingHost)?.to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or(SsrfError::MissingHost)?;
    let ips = resolve_pinned_hostname(&host, port, cfg).await?;

    pinned_client_for_addresses(&host, port, &ips, cfg)
}

fn pinned_client_for_addresses(
    host: &str,
    port: u16,
    ips: &[IpAddr],
    cfg: &SsrfConfig,
) -> Result<reqwest::Client, SsrfError> {
    let addresses = ips
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .collect::<Vec<_>>();
    reqwest::Client::builder()
        // An environment proxy would resolve the destination itself and could
        // bypass the validated addresses pinned into this client.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(cfg.timeout_ms.max(1)))
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|e| SsrfError::Http(e.to_string()))
}

pub async fn resolve_pinned_hostname(
    host: &str,
    port: u16,
    cfg: &SsrfConfig,
) -> Result<Vec<IpAddr>, SsrfError> {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(SsrfError::MissingHost);
    }
    if host_blocked_by_policy(&host, cfg) {
        warn!(host = %host, "ssrf blocked host by policy");
        return Err(SsrfError::BlockedHost(host));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        validate_ip(ip)?;
        return Ok(vec![ip]);
    }

    let lookup = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|err| SsrfError::DnsResolve {
            host: host.clone(),
            reason: err.to_string(),
        })?;
    let mut unique = HashSet::new();
    let mut out = Vec::new();
    for addr in lookup {
        let ip = addr.ip();
        if unique.insert(ip) {
            validate_ip(ip)?;
            out.push(ip);
        }
    }
    if out.is_empty() {
        return Err(SsrfError::DnsResolve {
            host,
            reason: "no address records".to_owned(),
        });
    }
    Ok(out)
}

pub async fn validate_outbound_url(url: &str, cfg: &SsrfConfig) -> Result<(), SsrfError> {
    let parsed = Url::parse(url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(SsrfError::UnsupportedScheme(other.to_owned())),
    }
    let host = parsed.host_str().ok_or(SsrfError::MissingHost)?;
    let port = parsed
        .port_or_known_default()
        .ok_or(SsrfError::MissingHost)?;
    if let Err(err) = resolve_pinned_hostname(host, port, cfg).await {
        warn!(url = %parsed, error = %err, "ssrf blocked outbound url");
        return Err(err);
    }
    Ok(())
}

pub async fn guarded_fetch(url: &str, cfg: &SsrfConfig) -> Result<GuardedFetchResponse, SsrfError> {
    guarded_fetch_with_limit(url, cfg, DEFAULT_MAX_RESPONSE_BYTES).await
}

/// Read the response incrementally and reject oversized bodies before copying
/// them into the retained buffer. Content-Length is only an early rejection;
/// chunked responses and decompressed bytes are bounded by the same counter.
pub async fn read_limited_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, SsrfError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(SsrfError::BodyTooLarge { limit: max_bytes });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| SsrfError::Http(e.to_string()))?
    {
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(SsrfError::BodyTooLarge { limit: max_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub async fn guarded_fetch_with_limit(
    url: &str,
    cfg: &SsrfConfig,
    max_bytes: usize,
) -> Result<GuardedFetchResponse, SsrfError> {
    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));

    let mut current = Url::parse(url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    for _hop in 0..=cfg.max_redirects {
        match current.scheme() {
            "http" | "https" => {}
            other => return Err(SsrfError::UnsupportedScheme(other.to_owned())),
        }

        // Build a fresh client per hop with the resolved IP pinned into the
        // resolver — closes the DNS rebinding window between validation and
        // the actual connection. `build_pinned_client` re-validates the host
        // so we don't need a separate `resolve_pinned_hostname` call here.
        let client = match build_pinned_client(current.as_str(), cfg).await {
            Ok(c) => c,
            Err(err) => {
                warn!(url = %current, error = %err, "ssrf blocked redirect hop");
                return Err(err);
            }
        };

        let response = tokio::time::timeout(timeout, client.get(current.clone()).send())
            .await
            .map_err(|_| SsrfError::Timeout(cfg.timeout_ms))?
            .map_err(|e| SsrfError::Http(e.to_string()))?;

        if response.status().is_redirection() {
            let Some(location) = response.headers().get(LOCATION) else {
                return Err(SsrfError::Http(
                    "redirect without location header".to_owned(),
                ));
            };
            let location = location
                .to_str()
                .map_err(|e| SsrfError::Http(e.to_string()))?;
            current = current
                .join(location)
                .map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
            continue;
        }

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_owned());
        let body = tokio::time::timeout(timeout, read_limited_body(response, max_bytes))
            .await
            .map_err(|_| SsrfError::Timeout(cfg.timeout_ms))??;
        return Ok(GuardedFetchResponse {
            final_url: current.to_string(),
            status,
            content_type,
            body,
        });
    }
    Err(SsrfError::RedirectLimitExceeded(cfg.max_redirects))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    async fn response_server(
        response: &'static [u8],
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.expect("request");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response).await.expect("response");
            String::from_utf8_lossy(&request).into_owned()
        });
        (address, task)
    }

    #[tokio::test]
    async fn limited_body_rejects_advertised_and_chunked_oversize_responses() {
        for raw in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabcdefghi".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n5\r\nefghi\r\n0\r\n\r\n".as_slice(),
        ] {
            let (address, server) = response_server(raw).await;
            let response = reqwest::Client::builder().no_proxy().build().expect("client")
                .get(format!("http://{address}/")).send().await.expect("response");
            let error = read_limited_body(response, 8).await.expect_err("oversize response");
            assert!(matches!(error, SsrfError::BodyTooLarge { limit: 8 }));
            server.await.expect("server");
        }
    }

    #[tokio::test]
    async fn limited_body_accepts_exact_limit_without_content_length() {
        let (address, server) = response_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n"
        ).await;
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("response");
        assert_eq!(
            read_limited_body(response, 8).await.expect("bounded body"),
            b"abcdefgh"
        );
        server.await.expect("server");
    }

    #[tokio::test]
    async fn pinned_client_uses_supplied_addresses_for_post_without_second_dns_lookup() {
        let (address, server) =
            response_server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        // This internal constructor receives the addresses already validated by
        // build_pinned_client. The deliberately unresolvable name demonstrates
        // that the connection uses those addresses instead of resolving again.
        let client = pinned_client_for_addresses(
            "does-not-exist.invalid",
            address.port(),
            &[address.ip()],
            &SsrfConfig::default(),
        )
        .expect("pinned client");
        let response = client
            .post(format!("http://does-not-exist.invalid:{}/", address.port()))
            .header("x-pinning-test", "custom-header")
            .send()
            .await
            .expect("pinned request");
        assert_eq!(read_limited_body(response, 2).await.expect("body"), b"ok");
        let request = server.await.expect("server");
        assert!(request.starts_with("POST / HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-pinning-test: custom-header")
        );
    }

    #[test]
    fn private_ipv4_ranges_are_blocked() {
        for raw in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.10",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            let ip: IpAddr = raw.parse().expect("ip parse");
            assert!(is_private_ip(ip), "{raw} should be private");
        }
    }

    #[test]
    fn private_ipv6_ranges_are_blocked() {
        for raw in ["::1", "fe80::1", "fc00::1", "fd12:3456::1"] {
            let ip: IpAddr = raw.parse().expect("ip parse");
            assert!(is_private_ip(ip), "{raw} should be private");
        }
    }

    #[test]
    fn public_ips_are_allowed() {
        for raw in ["8.8.8.8", "1.1.1.1", "2001:4860:4860::8888"] {
            let ip: IpAddr = raw.parse().expect("ip parse");
            assert!(!is_private_ip(ip), "{raw} should be public");
        }
    }

    #[test]
    fn wildcard_allowlist_matches_subdomains() {
        assert!(host_matches_pattern("api.example.com", "*.example.com"));
        assert!(host_matches_pattern("example.com", "*.example.com"));
        assert!(!host_matches_pattern("example.org", "*.example.com"));
    }

    #[test]
    fn blocklist_overrides_allowlist() {
        let cfg = SsrfConfig {
            allowlist: vec!["*.example.com".to_owned()],
            blocklist: vec!["api.example.com".to_owned()],
            ..SsrfConfig::default()
        };
        assert!(host_blocked_by_policy("api.example.com", &cfg));
        assert!(!host_blocked_by_policy("docs.example.com", &cfg));
    }

    #[tokio::test]
    async fn metadata_host_is_blocked() {
        let cfg = SsrfConfig::default();
        let err = resolve_pinned_hostname("metadata.google.internal", 80, &cfg)
            .await
            .expect_err("metadata host should be blocked");
        matches!(err, SsrfError::BlockedHost(_));
    }

    #[tokio::test]
    async fn ip_literal_is_validated_without_dns() {
        let cfg = SsrfConfig::default();
        let addrs = resolve_pinned_hostname("8.8.8.8", 443, &cfg)
            .await
            .expect("public ip should pass");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "8.8.8.8");
    }

    #[tokio::test]
    async fn guarded_fetch_blocks_private_target() {
        let cfg = SsrfConfig::default();
        let err = guarded_fetch("http://127.0.0.1/test", &cfg)
            .await
            .expect_err("localhost should be blocked");
        match err {
            SsrfError::BlockedIp(_) | SsrfError::BlockedHost(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_pinned_client_blocks_private_destination() {
        // The DNS-rebinding defence runs at client-build time: even before
        // the first request, asking for a private destination must fail.
        let cfg = SsrfConfig::default();
        let err = build_pinned_client("http://127.0.0.1/test", &cfg)
            .await
            .expect_err("localhost must be rejected at build time");
        match err {
            SsrfError::BlockedIp(_) | SsrfError::BlockedHost(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_pinned_client_rejects_non_http_schemes() {
        let cfg = SsrfConfig::default();
        let err = build_pinned_client("file:///etc/passwd", &cfg)
            .await
            .expect_err("file:// must be rejected");
        assert!(matches!(err, SsrfError::UnsupportedScheme(_)));
    }

    #[tokio::test]
    async fn build_pinned_client_rejects_blocklisted_host() {
        let cfg = SsrfConfig {
            blocklist: vec!["evil.example".to_owned()],
            ..SsrfConfig::default()
        };
        let err = build_pinned_client("https://evil.example/", &cfg)
            .await
            .expect_err("blocklisted host must fail");
        assert!(matches!(err, SsrfError::BlockedHost(_)));
    }
}
