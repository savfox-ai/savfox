//! Shared traits, helpers, and base abstractions for channel implementations.
//!
//! This module provides common building blocks that all (or most) channel
//! implementations share, reducing duplication and establishing consistent
//! patterns across the codebase.
//!
//! # Overview
//!
//! - [`ChannelConfig`] – trait for typed channel configuration structs.
//! - [`StreamingChannel`] – subtrait for channels with persistent connections.
//! - [`WebhookChannel`] – subtrait for channels that receive webhook callbacks.
//! - [`non_empty`] – shared config field lookup helper.
//! - [`resolve_outbound_token`] – generic token resolution from saved configs.
//! - [`send_http_json`] – generic HTTP POST + response checking.

use std::path::PathBuf;

use anyhow::Context;
use serde_json::{Map, Value};
use tracing::warn;

// ---------------------------------------------------------------------------
// Configuration trait
// ---------------------------------------------------------------------------

/// Common trait for typed channel configuration structs.
///
/// Each channel (Discord, Slack, Telegram, etc.) has a concrete config type
/// that can be constructed from the generic
/// [`savfox_core::config::channel_store::ChannelConfig`].  This trait captures
/// the shared interface so that helpers like [`resolve_outbound_token`] can
/// work generically.
pub trait ChannelConfig: Send + Sync {
    /// The platform identifier used to match `ChannelConfig.kind`, e.g.
    /// `"discord"`, `"slack"`, `"telegram"`.
    fn platform() -> &'static str;

    /// Try to construct this typed config from a generic channel config entry.
    ///
    /// Returns `None` if the entry's `kind` does not match [`Self::platform()`]
    /// or if the raw JSON cannot be interpreted.
    fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self>
    where
        Self: Sized;

    /// Whether this config carries enough authentication info to send outbound
    /// messages (e.g. has a non-empty bot token).
    fn has_outbound_auth(&self) -> bool;

    /// Return the primary outbound authentication token, if present.
    fn outbound_token(&self) -> Option<&str>;
}

// ---------------------------------------------------------------------------
// Streaming channel subtrait
// ---------------------------------------------------------------------------

/// Subtrait for channels that maintain persistent connections.
///
/// Channels like Discord (gateway WebSocket), Slack (Socket Mode), or IRC keep
/// long-lived connections open.  This trait provides lifecycle hooks for those
/// connections.
#[async_trait::async_trait]
pub trait StreamingChannel: Send + Sync {
    /// Open the persistent connection to the platform.
    async fn connect(&mut self) -> anyhow::Result<()>;

    /// Gracefully close the connection.
    async fn disconnect(&mut self) -> anyhow::Result<()>;

    /// Whether the connection is currently established.
    fn is_connected(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Webhook channel subtrait
// ---------------------------------------------------------------------------

/// Subtrait for channels that receive inbound messages via HTTP webhooks.
///
/// Channels like Telegram, LINE, and Google Chat receive events as HTTP POST
/// requests.  This trait captures the common webhook verification pattern.
pub trait WebhookChannel: Send + Sync {
    /// The URL path segment this channel listens on, e.g. `"/telegram"`.
    fn webhook_path(&self) -> &str;

    /// Verify the authenticity of an incoming webhook payload.
    ///
    /// `payload` is the raw request body and `signature` is the
    /// platform-specific signature header value.  Returns `true` if the
    /// signature is valid.
    fn verify_signature(&self, payload: &[u8], signature: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Shared config helpers
// ---------------------------------------------------------------------------

/// Look up a non-empty string value from a JSON object, trying multiple key
/// aliases in order.
///
/// This helper is duplicated across every channel config module today; this
/// single copy can be re-used by all of them.
pub fn non_empty(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    })
}

/// Resolve the outbound authentication token for a channel type `C` from saved
/// channel configurations.
///
/// Iterates over all enabled channel configs, parses each one into the typed
/// config `C`, and returns the first valid outbound token.
pub async fn resolve_outbound_token<C: ChannelConfig>(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let token = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(C::from_channel_config)
        .filter(C::has_outbound_auth)
        .find_map(|cfg| cfg.outbound_token().map(ToString::to_string));
    Ok(token)
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Send a JSON body via HTTP POST with optional auth header, and check the
/// response status.
///
/// This captures the common pattern used by Discord, Slack, Telegram, LINE,
/// and most other channels:
///
/// 1. POST JSON to `url`.
/// 2. Attach an `Authorization` (or other) header if `auth_header` is provided.
/// 3. Check whether the response indicates success; if not, bail with the
///    status code and response body.
///
/// For more nuanced error handling (e.g. treating some errors as warnings),
/// use the lower-level [`crate::http::check_response`] or
/// [`crate::http::warn_on_error`] utilities directly.
pub async fn send_http_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    auth_header: Option<(&str, &str)>,
    context: &str,
) -> anyhow::Result<()> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string());

    if let Some((name, value)) = auth_header {
        request = request.header(name, value);
    }

    let response = request.send().await?;
    check_response_status(response, context).await
}

/// Check an HTTP response, returning an `Err` with status and body on failure.
///
/// This is functionally equivalent to [`crate::http::check_response`] but lives
/// here for discoverability alongside [`send_http_json`].  Channel code that
/// already uses `crate::http::check_response` does not need to switch.
pub async fn check_response_status(
    response: reqwest::Response,
    context: &str,
) -> anyhow::Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("{context}: HTTP {status}: {body}")
}

/// Send an HTTP POST with JSON and return the response body on success,
/// logging a warning (but not failing) on error.
///
/// Useful for "fire-and-forget" operations like deleting or editing a message
/// where the caller typically wants to continue even if the API call fails.
pub async fn send_http_json_lenient(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    auth_header: Option<(&str, &str)>,
    context: &str,
) -> Option<Value> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string());

    if let Some((name, value)) = auth_header {
        request = request.header(name, value);
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("{context}: request failed: {e}");
            return None;
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let resp_body = response.bytes().await.unwrap_or_default();
        warn!(
            "{context}: HTTP {status}: {}",
            String::from_utf8_lossy(&resp_body)
        );
        return None;
    }

    response.json::<Value>().await.ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn non_empty_finds_first_matching_key() {
        let mut map = Map::new();
        map.insert("bot_token".to_string(), json!("secret-123"));
        map.insert("token".to_string(), json!("fallback"));

        assert_eq!(
            non_empty(&map, &["bot_token", "token"]),
            Some("secret-123".to_string())
        );
    }

    #[test]
    fn non_empty_skips_empty_strings() {
        let mut map = Map::new();
        map.insert("bot_token".to_string(), json!(""));
        map.insert("token".to_string(), json!("fallback"));

        assert_eq!(
            non_empty(&map, &["bot_token", "token"]),
            Some("fallback".to_string())
        );
    }

    #[test]
    fn non_empty_skips_whitespace_only() {
        let mut map = Map::new();
        map.insert("token".to_string(), json!("   "));

        assert_eq!(non_empty(&map, &["token"]), None);
    }

    #[test]
    fn non_empty_returns_none_on_missing_keys() {
        let map = Map::new();
        assert_eq!(non_empty(&map, &["bot_token", "token"]), None);
    }

    #[test]
    fn non_empty_trims_whitespace() {
        let mut map = Map::new();
        map.insert("token".to_string(), json!("  abc  "));

        assert_eq!(non_empty(&map, &["token"]), Some("abc".to_string()));
    }
}
