//! Stateless Feishu / Lark Bot API helpers.
//!
//! Each function takes an explicit `&reqwest::Client` and credentials so it can
//! be called from any context without coupling to a particular channel
//! instance.

use tracing::warn;

/// Send a text message via the Feishu/Lark Bot API.
pub async fn send_message(
    client: &reqwest::Client,
    base_url: &str,
    tenant_token: &str,
    receive_id: &str,
    receive_id_type: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
        base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "receive_id": receive_id,
        "msg_type": "text",
        "content": serde_json::json!({"text": text}).to_string(),
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {tenant_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "Feishu API error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }

    if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) {
        let code = payload.get("code").and_then(serde_json::Value::as_i64);
        if code.is_some_and(|value| value != 0) {
            let msg = payload
                .get("msg")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("Feishu API returned code {}: {}", code.unwrap_or(-1), msg);
        }
    }
    Ok(())
}

/// Send a text message and return the resulting `message_id` (if available).
pub async fn send_message_returning_id(
    client: &reqwest::Client,
    base_url: &str,
    tenant_token: &str,
    receive_id: &str,
    receive_id_type: &str,
    text: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
        base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "receive_id": receive_id,
        "msg_type": "text",
        "content": serde_json::json!({"text": text}).to_string(),
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {tenant_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let status = response.status();
    let resp_bytes = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        warn!(
            "Feishu API error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&resp_bytes)
        );
        return Ok(None);
    }
    let payload: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap_or_default();
    let mid = payload
        .get("data")
        .and_then(|d| d.get("message_id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Ok(mid)
}

/// Edit a Feishu message by its `message_id`.
pub async fn edit_message(
    client: &reqwest::Client,
    base_url: &str,
    tenant_token: &str,
    message_id: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/open-apis/im/v1/messages/{message_id}",
        base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "msg_type": "text",
        "content": serde_json::json!({"text": text}).to_string(),
    });
    let response = client
        .patch(&url)
        .header("Authorization", format!("Bearer {tenant_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Feishu edit error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Delete a Feishu message by its `message_id`.
pub async fn delete_message(
    client: &reqwest::Client,
    base_url: &str,
    tenant_token: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/open-apis/im/v1/messages/{message_id}",
        base_url.trim_end_matches('/')
    );
    let response = client
        .delete(&url)
        .header("Authorization", format!("Bearer {tenant_token}"))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Feishu delete error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Infer the Feishu `receive_id_type` from the channel ID prefix and a
/// configured default.
///
/// - IDs starting with `oc_` map to `"chat_id"`.
/// - IDs starting with `ou_` map to `"open_id"`.
/// - Otherwise the configured value is used, falling back to `"chat_id"`.
#[must_use]
pub fn infer_receive_id_type(channel_id: &str, configured: &str) -> String {
    let normalized = configured.trim();
    if channel_id.starts_with("oc_") {
        "chat_id".to_owned()
    } else if channel_id.starts_with("ou_") {
        "open_id".to_owned()
    } else if normalized.is_empty() {
        "chat_id".to_owned()
    } else {
        normalized.to_owned()
    }
}
