//! Stateless Mattermost REST API helpers.
//!
//! Each function takes an explicit `&reqwest::Client` and credentials so it can
//! be called from any context without coupling to a particular channel
//! instance.

use std::path::PathBuf;

use tracing::warn;

use super::config::resolve_mattermost_outbound_config;

/// Send a message to a Mattermost channel, optionally as a threaded reply.
pub async fn send_message(
    client: &reqwest::Client,
    server_url: &str,
    access_token: &str,
    channel_id: &str,
    text: &str,
    root_id: Option<&str>,
) -> anyhow::Result<()> {
    let url = format!("{server_url}/api/v4/posts");
    let mut body = serde_json::json!({
        "channel_id": channel_id,
        "message": text,
    });
    if let Some(root) = root_id.filter(|s| !s.is_empty()) {
        body["root_id"] = serde_json::json!(root);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Mattermost API error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Send a message and return the resulting post ID (if available).
pub async fn send_message_returning_id(
    client: &reqwest::Client,
    server_url: &str,
    access_token: &str,
    channel_id: &str,
    text: &str,
    root_id: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let url = format!("{server_url}/api/v4/posts");
    let mut body = serde_json::json!({
        "channel_id": channel_id,
        "message": text,
    });
    if let Some(root) = root_id.filter(|s| !s.is_empty()) {
        body["root_id"] = serde_json::json!(root);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Mattermost API error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
        return Ok(None);
    }
    let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
    Ok(resp_body
        .get("id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string))
}

/// Edit an existing Mattermost post.
pub async fn edit_message(
    client: &reqwest::Client,
    server_url: &str,
    access_token: &str,
    post_id: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("{server_url}/api/v4/posts/{post_id}");
    let body = serde_json::json!({ "id": post_id, "message": text });
    let response = client
        .put(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Mattermost edit error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Delete a Mattermost post.
pub async fn delete_message(
    client: &reqwest::Client,
    server_url: &str,
    access_token: &str,
    post_id: &str,
) -> anyhow::Result<()> {
    let url = format!("{server_url}/api/v4/posts/{post_id}");
    let response = client
        .delete(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Mattermost delete error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Resolve the Mattermost server URL and access token.
///
/// Resolution order:
/// 1. Saved channel configs under `savfox_home`.
/// 2. Environment variables `MATTERMOST_URL` and `MATTERMOST_TOKEN`.
///
/// Returns `None` if neither source provides valid credentials.
pub async fn resolve_outbound_credentials(savfox_home: &PathBuf) -> Option<(String, String)> {
    // Try saved channel configs first.
    if let Ok(Some(pair)) = resolve_mattermost_outbound_config(savfox_home).await {
        return Some(pair);
    }

    // Fall back to environment variables.
    let url = std::env::var("MATTERMOST_URL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let token = std::env::var("MATTERMOST_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    match (url, token) {
        (Some(u), Some(t)) => Some((u, t)),
        _ => None,
    }
}
