//! Stateless Discord Bot API client functions.
//!
//! Each function takes a shared `&reqwest::Client` and a bot token, making them
//! easy to call from any context without carrying around channel state.

use std::path::PathBuf;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tracing::{debug, warn};

use super::config::resolve_discord_outbound_token;

const BASE_URL: &str = "https://discord.com/api/v10";

/// Send a message to a Discord channel via the Bot API.
pub async fn send_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    content: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    send_message_returning_id(client, bot_token, channel_id, content, reply_to).await?;
    Ok(())
}

/// Send a message to a Discord channel and return the resulting message ID.
pub async fn send_message_returning_id(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    content: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<Option<String>> {
    debug!(
        target: "savfox::channels::discord",
        channel_id,
        content_len = content.len(),
        ?reply_to,
        "send_message"
    );
    let url = format!("{BASE_URL}/channels/{channel_id}/messages");
    let mut body = serde_json::json!({ "content": content });
    if let Some(message_id) = reply_to {
        let trimmed = message_id.trim();
        if !trimmed.is_empty() {
            body["message_reference"] = serde_json::json!({
                "message_id": trimmed,
            });
        }
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body);
        warn!(
            target: "savfox::channels::discord",
            %status,
            body = %body_str,
            "send_message failed"
        );
        return Ok(None);
    }

    let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
    let msg_id = resp_body
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    debug!(target: "savfox::channels::discord", ?msg_id, "send_message ok");
    Ok(msg_id)
}

/// Edit an existing Discord message.
pub async fn edit_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    debug!(
        target: "savfox::channels::discord",
        channel_id,
        message_id,
        content_len = content.len(),
        "edit_message"
    );
    let url = format!("{BASE_URL}/channels/{channel_id}/messages/{message_id}");
    let body = serde_json::json!({ "content": content });
    let response = client
        .patch(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            target: "savfox::channels::discord",
            %status,
            body = %String::from_utf8_lossy(&body),
            "edit_message failed"
        );
    } else {
        debug!(target: "savfox::channels::discord", "edit_message ok");
    }
    Ok(())
}

/// Delete a Discord message.
pub async fn delete_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    debug!(
        target: "savfox::channels::discord",
        channel_id,
        message_id,
        "delete_message"
    );
    let url = format!("{BASE_URL}/channels/{channel_id}/messages/{message_id}");
    let response = client
        .delete(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            target: "savfox::channels::discord",
            %status,
            body = %String::from_utf8_lossy(&body),
            "delete_message failed"
        );
    } else {
        debug!(target: "savfox::channels::discord", "delete_message ok");
    }
    Ok(())
}

/// Send a rich embed message to a Discord channel.
pub async fn send_embed(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    title: &str,
    description: &str,
    color: u32,
) -> anyhow::Result<()> {
    debug!(
        target: "savfox::channels::discord",
        channel_id,
        title,
        desc_len = description.len(),
        "send_embed"
    );
    let url = format!("{BASE_URL}/channels/{channel_id}/messages");
    let body = serde_json::json!({
        "embeds": [{
            "title": title,
            "description": description,
            "color": color,
        }]
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            target: "savfox::channels::discord",
            %status,
            body = %String::from_utf8_lossy(&body),
            "send_embed failed"
        );
    }

    Ok(())
}

/// Verify a Discord interaction signature using Ed25519.
///
/// `public_key_hex` is the hex-encoded application public key.
/// `timestamp` and `body` form the signed message.
/// `signature_hex` is the hex-encoded Ed25519 signature from the request header.
#[must_use]
pub fn verify_signature(
    public_key_hex: &str,
    timestamp: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    debug!(
        target: "savfox::channels::discord",
        pubkey_len = public_key_hex.len(),
        sig_len = signature_hex.len(),
        body_len = body.len(),
        timestamp,
        "verify_signature"
    );
    let pub_key_bytes = match hex::decode(public_key_hex) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let pub_key_array: [u8; 32] = match pub_key_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&pub_key_array) {
        Ok(key) => key,
        Err(_) => return false,
    };

    let sig_bytes = match hex::decode(signature_hex) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let sig_array: [u8; 64] = match sig_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(&sig_array);

    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    verifying_key.verify(&message, &sig).is_ok()
}

/// Resolve a Discord bot token by first checking the saved channel configuration
/// under `savfox_home`, then falling back to the `DISCORD_BOT_TOKEN` environment
/// variable. Returns `None` if neither source provides a token.
pub async fn resolve_bot_token(savfox_home: &PathBuf) -> Option<String> {
    // Try saved channel config first.
    if let Ok(Some(token)) = resolve_discord_outbound_token(savfox_home).await {
        debug!(
            target: "savfox::channels::discord",
            "resolve_bot_token: found token from channel config"
        );
        return Some(token);
    }

    // Fall back to environment variable.
    let env_token = std::env::var("DISCORD_BOT_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());
    debug!(
        target: "savfox::channels::discord",
        env_fallback = env_token.is_some(),
        "resolve_bot_token: env fallback"
    );
    env_token
}
