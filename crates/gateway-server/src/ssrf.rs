//! SSRF protection helpers for outbound HTTP access.
//!
//! This module provides:
//! - private/metadata IP and hostname blocking
//! - allowlist/blocklist hostname policy
//! - DNS resolution with pinned IP validation
//! - guarded fetch with redirect re-validation

use std::collections::HashSet;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::LOCATION;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

const DEFAULT_MAX_REDIRECTS: usize = 5;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;

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
}

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
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
        }
    }
}

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
            reason: "no address records".to_string(),
        });
    }
    Ok(out)
}

pub async fn validate_outbound_url(url: &str, cfg: &SsrfConfig) -> Result<(), SsrfError> {
    let parsed = Url::parse(url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(SsrfError::UnsupportedScheme(other.to_string())),
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
    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));
    let client = build_guarded_client(cfg)?;

    let mut current = Url::parse(url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    for _hop in 0..=cfg.max_redirects {
        match current.scheme() {
            "http" | "https" => {}
            other => return Err(SsrfError::UnsupportedScheme(other.to_string())),
        }

        let host = current.host_str().ok_or(SsrfError::MissingHost)?;
        let port = current
            .port_or_known_default()
            .ok_or(SsrfError::MissingHost)?;
        if let Err(err) = resolve_pinned_hostname(host, port, cfg).await {
            warn!(url = %current, error = %err, "ssrf blocked redirect hop");
            return Err(err);
        }

        let response = tokio::time::timeout(timeout, client.get(current.clone()).send())
            .await
            .map_err(|_| SsrfError::Timeout(cfg.timeout_ms))?
            .map_err(|e| SsrfError::Http(e.to_string()))?;

        if response.status().is_redirection() {
            let Some(location) = response.headers().get(LOCATION) else {
                return Err(SsrfError::Http(
                    "redirect without location header".to_string(),
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
            .map(|v| v.to_string());
        let body = tokio::time::timeout(timeout, response.bytes())
            .await
            .map_err(|_| SsrfError::Timeout(cfg.timeout_ms))?
            .map_err(|e| SsrfError::Http(e.to_string()))?
            .to_vec();
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
    use super::*;

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
            allowlist: vec!["*.example.com".to_string()],
            blocklist: vec!["api.example.com".to_string()],
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
}
