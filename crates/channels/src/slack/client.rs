//! Stateless Slack Web API client helpers.
//!
//! Every function takes a shared `&reqwest::Client` so callers can
//! reuse connection pools without tying the helpers to any particular
//! server struct.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;
use tracing::warn;

use super::config::resolve_slack_outbound_token;

// ---------------------------------------------------------------------------
// Messaging
// ---------------------------------------------------------------------------

/// Post a message (with optional Block Kit blocks) to a Slack channel.
pub async fn send_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    text: &str,
    blocks: Option<Value>,
    thread_ts: Option<&str>,
) -> Result<()> {
    let url = "https://slack.com/api/chat.postMessage";
    let mut body = serde_json::json!({
        "channel": channel,
        "text": text,
    });

    if let Some(blocks) = blocks {
        body["blocks"] = blocks;
    }
    if let Some(thread_ts) = thread_ts {
        let trimmed = thread_ts.trim();
        if !trimmed.is_empty() {
            body["thread_ts"] = serde_json::json!(trimmed);
        }
    }

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body);
        warn!("Slack API error: HTTP {status}: {body_str}");
    }

    Ok(())
}

/// Post a message and return the resulting message timestamp (`ts`).
pub async fn send_message_returning_id(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<Option<String>> {
    let url = "https://slack.com/api/chat.postMessage";
    let mut body = serde_json::json!({
        "channel": channel,
        "text": text,
    });
    if let Some(thread_ts) = thread_ts {
        let trimmed = thread_ts.trim();
        if !trimmed.is_empty() {
            body["thread_ts"] = serde_json::json!(trimmed);
        }
    }

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
    if resp_body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = resp_body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        warn!("Slack API error: {err}");
        return Ok(None);
    }
    // Slack returns channel + ts in the response.
    let ts = resp_body
        .get("ts")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    Ok(ts)
}

/// Edit a Slack message identified by its timestamp.
pub async fn edit_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    ts: &str,
    text: &str,
) -> Result<()> {
    let url = "https://slack.com/api/chat.update";
    let body = serde_json::json!({
        "channel": channel,
        "ts": ts,
        "text": text,
    });
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
    if resp_body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = resp_body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        warn!("Slack chat.update error: {err}");
    }
    Ok(())
}

/// Delete a Slack message identified by its timestamp.
pub async fn delete_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    ts: &str,
) -> Result<()> {
    let url = "https://slack.com/api/chat.delete";
    let body = serde_json::json!({
        "channel": channel,
        "ts": ts,
    });
    let _ = client
        .post(url)
        .header("Authorization", format!("Bearer {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Signature / timestamp verification
// ---------------------------------------------------------------------------

/// Verify a Slack request signature using HMAC-SHA256.
///
/// `expected_sig` is the value of the `X-Slack-Signature` header.
#[must_use] 
pub fn verify_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &[u8],
    expected_sig: &str,
) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let sig_basestring = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));

    let mut mac = match Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(sig_basestring.as_bytes());
    let result = mac.finalize();
    let computed = format!("v0={}", hex::encode(result.into_bytes()));

    // Constant-time comparison
    computed == expected_sig
}

/// Check whether a Slack timestamp is within the allowed replay window.
///
/// Returns `false` if `ts` cannot be parsed or lies in the future.
#[must_use] 
pub fn is_timestamp_fresh(ts: &str, max_age_seconds: u64) -> bool {
    let now_epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let Ok(parsed) = ts.parse::<u64>() else {
        return false;
    };
    if parsed > now_epoch_secs {
        return false;
    }
    now_epoch_secs.saturating_sub(parsed) <= max_age_seconds
}

// ---------------------------------------------------------------------------
// Token resolution
// ---------------------------------------------------------------------------

/// Resolve a Slack bot token by first checking saved channel configs under
/// `savfox_home`, then falling back to the `SLACK_BOT_TOKEN` environment
/// variable.
pub async fn resolve_bot_token(savfox_home: &PathBuf) -> Option<String> {
    if let Ok(Some(t)) = resolve_slack_outbound_token(savfox_home).await {
        return Some(t);
    }
    std::env::var("SLACK_BOT_TOKEN").ok()
}
