//! Stateless DingTalk Open API helpers for message send/recall.
//!
//! Each function takes an explicit `&reqwest::Client` and credentials so it can
//! be called from any context without coupling to a particular channel instance.

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use tracing::warn;

use crate::http::warn_on_error;

/// Send a text message to a DingTalk webhook target (URL or `access_token`).
///
/// `channel` is either a full webhook URL (`https://oapi.dingtalk.com/...`) or
/// a logical identifier; when not a URL, `access_token` is required to derive
/// the canonical webhook URL. When `webhook_secret` is set, the request URL
/// is signed using DingTalk's HMAC-SHA256 scheme.
pub async fn send_dingtalk_text_message(
    http_client: &reqwest::Client,
    channel: &str,
    webhook_secret: Option<&str>,
    access_token: Option<&str>,
    message: &str,
) -> anyhow::Result<()> {
    let channel = channel.trim();
    let Some(target) = resolve_webhook_target(channel, access_token) else {
        anyhow::bail!("dingtalk webhook target is not configured");
    };
    let signed_url = sign_webhook_url(&target, webhook_secret)?;
    let body = json!({
        "msgtype": "text",
        "text": { "content": message },
    });
    let response = http_client
        .post(&signed_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    warn_on_error(response, "Dingtalk send error").await;
    Ok(())
}

fn resolve_webhook_target(channel: &str, access_token: Option<&str>) -> Option<String> {
    if channel.starts_with("https://") || channel.starts_with("http://") {
        return Some(channel.to_owned());
    }
    let token = access_token.map(str::trim).filter(|v| !v.is_empty())?;
    Some(format!(
        "https://oapi.dingtalk.com/robot/send?access_token={token}"
    ))
}

fn sign_webhook_url(url: &str, webhook_secret: Option<&str>) -> anyhow::Result<String> {
    let Some(secret) = webhook_secret.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(url.to_owned());
    };
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();
    let sign_content = format!("{timestamp}\n{secret}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
    mac.update(sign_content.as_bytes());
    let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let sign_encoded: String = url::form_urlencoded::byte_serialize(sign.as_bytes()).collect();
    let separator = if url.contains('?') { '&' } else { '?' };
    Ok(format!(
        "{url}{separator}timestamp={timestamp}&sign={sign_encoded}"
    ))
}

/// Fetch an access token using client credentials (appKey/appSecret).
pub async fn fetch_access_token(
    client: &reqwest::Client,
    openapi_host: &str,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<String> {
    let url = format!(
        "{}/v1.0/oauth2/accessToken",
        openapi_host.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "appKey": client_id,
        "appSecret": client_secret,
    });
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let resp_bytes = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "DingTalk accessToken error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&resp_bytes)
        );
    }
    let payload: serde_json::Value = serde_json::from_slice(&resp_bytes)?;
    payload
        .get("accessToken")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("DingTalk accessToken response missing accessToken field"))
}

/// Send a text message to a single user (DM) and return the `processQueryKey`.
pub async fn send_dm_message_returning_id(
    client: &reqwest::Client,
    openapi_host: &str,
    access_token: &str,
    robot_code: &str,
    user_id: &str,
    text: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "{}/v1.0/robot/oToMessages/batchSend",
        openapi_host.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "robotCode": robot_code,
        "userIds": [user_id],
        "msgKey": "sampleText",
        "msgParam": serde_json::json!({"content": text}).to_string(),
    });
    let response = client
        .post(&url)
        .header("x-acs-dingtalk-access-token", access_token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let resp_bytes = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        warn!(
            "DingTalk DM send error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&resp_bytes)
        );
        return Ok(None);
    }
    let payload: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap_or_default();
    Ok(payload
        .get("processQueryKey")
        .and_then(|v| v.as_str())
        .map(str::to_owned))
}

/// Send a text message to a group conversation and return the `processQueryKey`.
pub async fn send_group_message_returning_id(
    client: &reqwest::Client,
    openapi_host: &str,
    access_token: &str,
    robot_code: &str,
    conversation_id: &str,
    text: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "{}/v1.0/robot/groupMessages/send",
        openapi_host.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "robotCode": robot_code,
        "openConversationId": conversation_id,
        "msgKey": "sampleText",
        "msgParam": serde_json::json!({"content": text}).to_string(),
    });
    let response = client
        .post(&url)
        .header("x-acs-dingtalk-access-token", access_token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let resp_bytes = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        warn!(
            "DingTalk group send error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&resp_bytes)
        );
        return Ok(None);
    }
    let payload: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap_or_default();
    Ok(payload
        .get("processQueryKey")
        .and_then(|v| v.as_str())
        .map(str::to_owned))
}

/// Recall a DM message by its `processQueryKey`.
pub async fn recall_dm_message(
    client: &reqwest::Client,
    openapi_host: &str,
    access_token: &str,
    robot_code: &str,
    process_query_key: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/v1.0/robot/oToMessages/batchRecall",
        openapi_host.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "robotCode": robot_code,
        "processQueryKeys": [process_query_key],
    });
    let response = client
        .post(&url)
        .header("x-acs-dingtalk-access-token", access_token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "DingTalk DM recall error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Recall a group message by its `processQueryKey`.
pub async fn recall_group_message(
    client: &reqwest::Client,
    openapi_host: &str,
    access_token: &str,
    robot_code: &str,
    conversation_id: &str,
    process_query_key: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/v1.0/robot/groupMessages/recall",
        openapi_host.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "robotCode": robot_code,
        "openConversationId": conversation_id,
        "processQueryKeys": [process_query_key],
    });
    let response = client
        .post(&url)
        .header("x-acs-dingtalk-access-token", access_token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "DingTalk group recall error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}
