//! Stateless Discord Bot API client functions.
//!
//! Each function takes a shared `&reqwest::Client` and a bot token, making them
//! easy to call from any context without carrying around channel state.

use std::path::PathBuf;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tracing::warn;

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
    println!(
        "[discord:client] send_message channel={channel_id}, content_len={}, reply_to={:?}",
        content.len(),
        reply_to
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
        println!("[discord:client] send_message FAILED: HTTP {status}: {body_str}");
        warn!("Discord API error: HTTP {status}: {body_str}");
        return Ok(None);
    }

    let resp_body: serde_json::Value = response.json().await.unwrap_or_default();
    let msg_id = resp_body
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    println!("[discord:client] send_message OK, msg_id={msg_id:?}");
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
    println!(
        "[discord:client] edit_message channel={channel_id}, msg={message_id}, content_len={}",
        content.len()
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
        println!(
            "[discord:client] edit_message FAILED: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
        warn!(
            "Discord edit error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    } else {
        println!("[discord:client] edit_message OK");
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
    println!("[discord:client] delete_message channel={channel_id}, msg={message_id}");
    let url = format!("{BASE_URL}/channels/{channel_id}/messages/{message_id}");
    let response = client
        .delete(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        println!(
            "[discord:client] delete_message FAILED: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
        warn!(
            "Discord delete error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    } else {
        println!("[discord:client] delete_message OK");
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
    println!(
        "[discord:client] send_embed channel={channel_id}, title={title}, desc_len={}",
        description.len()
    );
    let url = format!("{BASE_URL}/channels/{channel_id}/messages");
    let body = serde_json::json!({
        "embeds": [{
            "title": title,
            "description": description,
            "color": color,
        }]
    });

    let _response = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

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
    println!(
        "[discord:client] verify_signature: pubkey_len={}, sig_len={}, body_len={}, ts={timestamp}",
        public_key_hex.len(),
        signature_hex.len(),
        body.len()
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
        println!("[discord:client] resolve_bot_token: found token from channel config");
        return Some(token);
    }

    // Fall back to environment variable.
    let env_token = std::env::var("DISCORD_BOT_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());
    println!(
        "[discord:client] resolve_bot_token: env fallback={}",
        env_token.is_some()
    );
    env_token
}
