//! Stateless DingTalk Open API helpers for message send/recall.
//!
//! Each function takes an explicit `&reqwest::Client` and credentials so it can
//! be called from any context without coupling to a particular channel instance.

use tracing::warn;

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
