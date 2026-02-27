use std::collections::HashMap;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct WebFetchHandler;

/// Maximum response body size: 5 MB.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// HTTP request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum redirects to follow while re-validating each hop for SSRF safety.
const MAX_REDIRECTS: usize = 5;
/// Default maximum content length returned to the model.
const DEFAULT_MAX_LENGTH: usize = 50_000;
/// Default cache TTL: 15 minutes.
const DEFAULT_CACHE_TTL_SECS: u64 = 15 * 60;
/// Maximum cache entries before eviction.
const MAX_CACHE_ENTRIES: usize = 100;

#[derive(Deserialize)]
struct WebFetchArgs {
    /// URL to fetch.
    url: String,
    /// How to extract content: "markdown" (default), "text", or "raw".
    #[serde(default = "defaults::extract_mode")]
    extract_mode: String,
    /// Maximum character length of the returned content.
    #[serde(default = "defaults::max_length")]
    max_length: usize,
    /// Cache TTL in seconds (0 = no cache, default = 900).
    #[serde(default = "defaults::cache_ttl_secs")]
    cache_ttl_secs: u64,
    /// Custom timeout in seconds (default = 30).
    #[serde(default = "defaults::timeout_secs")]
    timeout_secs: u64,
}

mod defaults {
    pub fn extract_mode() -> String {
        "markdown".to_string()
    }

    pub fn max_length() -> usize {
        super::DEFAULT_MAX_LENGTH
    }

    pub fn cache_ttl_secs() -> u64 {
        super::DEFAULT_CACHE_TTL_SECS
    }

    pub fn timeout_secs() -> u64 {
        30
    }
}

/// In-memory cache entry with TTL.
struct CacheEntry {
    content: String,
    inserted_at: Instant,
    ttl: Duration,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() > self.ttl
    }
}

/// Simple TTL-based cache for web fetch results.
static FETCH_CACHE: std::sync::LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Normalize a cache key: trim + lowercase.
fn normalize_cache_key(url: &str) -> String {
    url.trim().to_lowercase()
}

/// Read from cache if entry exists and is not expired.
fn read_cache(key: &str) -> Option<String> {
    let cache = FETCH_CACHE.lock().ok()?;
    let entry = cache.get(key)?;
    if entry.is_expired() {
        None
    } else {
        Some(entry.content.clone())
    }
}

/// Write to cache, evicting the oldest entry if full.
fn write_cache(key: String, content: String, ttl: Duration) {
    let Ok(mut cache) = FETCH_CACHE.lock() else {
        return;
    };

    // Evict expired entries first.
    cache.retain(|_, entry| !entry.is_expired());

    // If still full, remove the oldest entry.
    if cache.len() >= MAX_CACHE_ENTRIES {
        let oldest_key = cache
            .iter()
            .min_by_key(|(_, entry)| entry.inserted_at)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest_key {
            cache.remove(&key);
        }
    }

    cache.insert(
        key,
        CacheEntry {
            content,
            inserted_at: Instant::now(),
            ttl,
        },
    );
}

#[async_trait]
impl ToolHandler for WebFetchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "web_fetch handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: WebFetchArgs = parse_arguments(&arguments)?;

        // Parse and validate the URL.
        let parsed_url: url::Url = args.url.parse().map_err(|err: url::ParseError| {
            FunctionCallError::RespondToModel(format!("invalid URL: {err}"))
        })?;
        validate_target_url(&parsed_url).await?;

        // Check cache first.
        let cache_key = normalize_cache_key(parsed_url.as_str());
        if args.cache_ttl_secs > 0 {
            if let Some(cached) = read_cache(&cache_key) {
                let output = truncate_output(&cached, args.max_length);
                let wrapped =
                    crate::external_content::wrap_external_content(parsed_url.as_str(), &output);
                return Ok(ToolOutput::Function {
                    content: format!("[cached]\n{wrapped}"),
                    content_items: None,
                    success: Some(true),
                });
            }
        }

        // Build the HTTP client with custom timeout.
        let timeout = Duration::from_secs(args.timeout_secs.max(1).min(300));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        let mut current_url = parsed_url.clone();
        let response = {
            let mut response = None;
            for _hop in 0..=MAX_REDIRECTS {
                validate_target_url(&current_url).await?;
                let candidate = client
                    .get(current_url.as_str())
                    .send()
                    .await
                    .map_err(|err| {
                        FunctionCallError::RespondToModel(format!("HTTP request failed: {err}"))
                    })?;

                if candidate.status().is_redirection() {
                    let location = candidate
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| {
                            FunctionCallError::RespondToModel(
                                "HTTP redirect missing Location header".to_string(),
                            )
                        })?;
                    current_url = current_url.join(location).map_err(|err| {
                        FunctionCallError::RespondToModel(format!("invalid redirect target: {err}"))
                    })?;
                    continue;
                }

                response = Some(candidate);
                break;
            }
            response.ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "redirect limit exceeded (max {MAX_REDIRECTS})"
                ))
            })?
        };

        let status = response.status();
        if !status.is_success() {
            return Err(FunctionCallError::RespondToModel(format!(
                "HTTP {status} for {}",
                args.url
            )));
        }

        // Check content-length if available.
        if let Some(content_length) = response.content_length() {
            if content_length as usize > MAX_BODY_BYTES {
                return Err(FunctionCallError::RespondToModel(format!(
                    "response body too large: {} bytes (limit: {} bytes)",
                    content_length, MAX_BODY_BYTES
                )));
            }
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        // Read the body with size limit.
        let body_bytes = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read response body: {err}"))
        })?;

        if body_bytes.len() > MAX_BODY_BYTES {
            return Err(FunctionCallError::RespondToModel(format!(
                "response body too large: {} bytes (limit: {} bytes)",
                body_bytes.len(),
                MAX_BODY_BYTES
            )));
        }

        let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

        // Convert based on extract mode.
        let extracted = match args.extract_mode.as_str() {
            "raw" => body_text,
            "text" => {
                if content_type.contains("html") {
                    let md = html_to_markdown(&body_text);
                    markdown_to_text(&md)
                } else {
                    body_text
                }
            }
            "markdown" | _ => {
                if content_type.contains("html") {
                    html_to_markdown(&body_text)
                } else {
                    body_text
                }
            }
        };

        // Write to cache.
        if args.cache_ttl_secs > 0 {
            write_cache(
                cache_key,
                extracted.clone(),
                Duration::from_secs(args.cache_ttl_secs),
            );
        }

        // Truncate to max_length.
        let output = truncate_output(&extracted, args.max_length);
        let wrapped = crate::external_content::wrap_external_content(parsed_url.as_str(), &output);

        Ok(ToolOutput::Function {
            content: wrapped,
            content_items: None,
            success: Some(true),
        })
    }
}

/// Truncate content to max_length with indicator.
fn truncate_output(content: &str, max_length: usize) -> String {
    let char_count = content.chars().count();
    if char_count > max_length {
        let truncated = content.chars().take(max_length).collect::<String>();
        format!(
            "{truncated}\n\n[Content truncated at {} characters]",
            max_length
        )
    } else {
        content.to_owned()
    }
}

// ─── HTML-to-Markdown conversion ────────────────────────────────────────────

/// Convert HTML to Markdown, preserving structure (headings, links, lists, code).
/// Falls back to plain text extraction on malformed HTML.
fn html_to_markdown(html: &str) -> String {
    // First try the html2text crate for basic conversion.
    let basic = html2text::from_read(html.as_bytes(), 80).unwrap_or_default();
    if basic.is_empty() {
        // Fallback: strip tags and decode entities.
        return strip_tags_and_normalize(html);
    }

    // Enhance: the html2text output is already reasonable Markdown.
    // Normalize excessive whitespace.
    normalize_whitespace(&basic)
}

/// Strip HTML tags and decode common entities.
fn strip_tags_and_normalize(html: &str) -> String {
    let stripped = strip_tags(html);
    let decoded = decode_html_entities(&stripped);
    normalize_whitespace(&decoded)
}

/// Remove all HTML tags from a string.
fn strip_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    result
}

/// Decode common HTML entities.
fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
}

/// Normalize whitespace: collapse multiple blank lines, trim trailing spaces.
fn normalize_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut consecutive_newlines = 0u32;

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                result.push('\n');
            }
        } else {
            if consecutive_newlines > 0 && !result.is_empty() {
                // Already pushed newlines above.
            }
            consecutive_newlines = 0;
            result.push_str(trimmed);
            result.push('\n');
        }
    }

    result.trim().to_owned()
}

// ─── Markdown-to-text conversion ────────────────────────────────────────────

/// Strip Markdown formatting to plain text.
fn markdown_to_text(markdown: &str) -> String {
    let mut text = markdown.to_owned();

    // Remove images: ![alt](url) → alt
    while let Some(start) = text.find("![") {
        if let Some(mid) = text[start..].find("](") {
            if let Some(end) = text[start + mid..].find(')') {
                let alt = &text[start + 2..start + mid].to_owned();
                text = format!(
                    "{}{}{}",
                    &text[..start],
                    alt,
                    &text[start + mid + end + 1..]
                );
                continue;
            }
        }
        break;
    }

    // Remove links: [label](url) → label
    while let Some(start) = text.find('[') {
        if let Some(mid) = text[start..].find("](") {
            if let Some(end) = text[start + mid..].find(')') {
                let label = &text[start + 1..start + mid].to_owned();
                text = format!(
                    "{}{}{}",
                    &text[..start],
                    label,
                    &text[start + mid + end + 1..]
                );
                continue;
            }
        }
        break;
    }

    // Remove heading markers.
    text = text
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("# ") {
                trimmed.strip_prefix("# ").unwrap_or(trimmed)
            } else if trimmed.starts_with("## ") {
                trimmed.strip_prefix("## ").unwrap_or(trimmed)
            } else if trimmed.starts_with("### ") {
                trimmed.strip_prefix("### ").unwrap_or(trimmed)
            } else if trimmed.starts_with("#### ") {
                trimmed.strip_prefix("#### ").unwrap_or(trimmed)
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Remove inline code backticks.
    text = text.replace('`', "");

    // Remove bold/italic markers.
    text = text.replace("**", "").replace("__", "");
    text = text.replace('*', "").replace('_', " ");

    normalize_whitespace(&text)
}

// ─── SSRF protection ────────────────────────────────────────────────────────

/// Check if a hostname resolves to a private or loopback IP.
async fn validate_target_url(url: &url::Url) -> Result<(), FunctionCallError> {
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(FunctionCallError::RespondToModel(format!(
                "unsupported URL scheme: {scheme}"
            )));
        }
    }

    let host = url
        .host_str()
        .ok_or_else(|| FunctionCallError::RespondToModel("URL missing host".to_string()))?;
    if is_blocked_hostname(host) {
        return Err(FunctionCallError::RespondToModel(
            "fetching private/metadata hosts is not allowed".to_string(),
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| FunctionCallError::RespondToModel("URL missing port".to_string()))?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(FunctionCallError::RespondToModel(
                "fetching private/loopback addresses is not allowed".to_string(),
            ));
        }
        return Ok(());
    }

    let lookup = tokio::net::lookup_host((host, port)).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to resolve host '{host}': {err}"))
    })?;
    let mut unique = HashSet::new();
    let mut has_records = false;
    for addr in lookup {
        has_records = true;
        let ip = addr.ip();
        if !unique.insert(ip) {
            continue;
        }
        if is_private_ip(ip) {
            return Err(FunctionCallError::RespondToModel(format!(
                "fetching private address {ip} is not allowed"
            )));
        }
    }
    if !has_records {
        return Err(FunctionCallError::RespondToModel(format!(
            "failed to resolve host '{host}'"
        )));
    }
    Ok(())
}

fn is_private_host(host: &str) -> bool {
    if is_blocked_hostname(host) {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    false
}

fn is_blocked_hostname(host: &str) -> bool {
    let normalized = host.trim().trim_matches('.').to_ascii_lowercase();
    normalized == "localhost"
        || normalized == "metadata.google.internal"
        || normalized == "169.254.169.254"
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64.0.0/10
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_is_private() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("::1"));
        assert!(is_private_host("metadata.google.internal"));
    }

    #[test]
    fn private_ips_detected() {
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }

    #[tokio::test]
    async fn validate_target_url_blocks_private_literal() {
        let url: url::Url = "http://127.0.0.1".parse().unwrap();
        let err = validate_target_url(&url)
            .await
            .expect_err("must block localhost");
        let text = format!("{err}");
        assert!(text.contains("private") || text.contains("loopback"));
    }

    #[tokio::test]
    async fn validate_target_url_allows_public_literal() {
        let url: url::Url = "https://8.8.8.8".parse().unwrap();
        validate_target_url(&url)
            .await
            .expect("public ip should be allowed");
    }

    #[test]
    fn html_to_markdown_basic() {
        let html = "<h1>Title</h1><p>Hello <b>world</b></p>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("world"));
    }

    #[test]
    fn strip_tags_works() {
        assert_eq!(strip_tags("<p>hello</p>"), "hello");
        assert_eq!(strip_tags("<a href='x'>link</a>"), "link");
    }

    #[test]
    fn decode_entities_works() {
        assert_eq!(decode_html_entities("&amp; &lt; &gt;"), "& < >");
    }

    #[test]
    fn markdown_to_text_strips_formatting() {
        let md = "# Title\n**bold** and [link](http://x.com)";
        let text = markdown_to_text(md);
        assert!(text.contains("Title"));
        assert!(text.contains("bold"));
        assert!(text.contains("link"));
        assert!(!text.contains("http://x.com"));
    }

    #[test]
    fn cache_round_trip() {
        let key = normalize_cache_key("  HTTP://Example.COM  ");
        assert_eq!(key, "http://example.com");

        write_cache(
            "test_key".to_string(),
            "test_value".to_string(),
            Duration::from_secs(60),
        );
        assert_eq!(read_cache("test_key"), Some("test_value".to_string()));
    }
}
