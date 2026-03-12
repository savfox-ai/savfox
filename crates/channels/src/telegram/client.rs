use std::path::PathBuf;

use serde_json::Value;
use tracing::warn;

use super::config::resolve_telegram_outbound_token;

// ---------------------------------------------------------------------------
// HTML escaping
// ---------------------------------------------------------------------------

/// Escape text for Telegram HTML parse mode.
///
/// Replaces `&`, `<`, and `>` with their HTML entity equivalents.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Log preview helper
// ---------------------------------------------------------------------------

/// Truncate a string for log preview purposes.
pub fn truncate_log_preview(text: &str, max_chars: usize) -> String {
    let mut preview: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

// ---------------------------------------------------------------------------
// Webhook verification
// ---------------------------------------------------------------------------

/// Verify Telegram webhook secret token equality.
pub fn verify_webhook_secret(expected_secret: &str, received_secret: &str) -> bool {
    expected_secret == received_secret
}

// ---------------------------------------------------------------------------
// Bot token resolution
// ---------------------------------------------------------------------------

/// Resolve the Telegram bot token by chaining: saved channel config -> `TELEGRAM_BOT_TOKEN` env
/// var.
///
/// This calls [`resolve_telegram_outbound_token`] for the saved-config lookup and
/// falls back to the `TELEGRAM_BOT_TOKEN` environment variable.
pub async fn resolve_bot_token(savfox_home: &PathBuf) -> Option<String> {
    if let Ok(Some(t)) = resolve_telegram_outbound_token(savfox_home).await {
        return Some(t);
    }
    std::env::var("TELEGRAM_BOT_TOKEN").ok()
}

// ---------------------------------------------------------------------------
// Sending messages
// ---------------------------------------------------------------------------

/// Send a Telegram message and return the resulting `message_id`.
///
/// `parse_mode` is typically `Some("HTML")` or `None`.  When HTML parse mode
/// is active the text is automatically escaped via [`escape_html`].
pub async fn send_message(
    client: &reqwest::Client,
    bot_token: &str,
    chat_id: &str,
    text: &str,
    parse_mode: Option<&str>,
    reply_to_message_id: Option<&str>,
) -> anyhow::Result<Option<i64>> {
    let text_preview = truncate_log_preview(text, 180);
    println!(
        "[telegram] Sending message: chat_id={chat_id}, text_len={}, parse_mode={:?}, reply_to={:?}, text_preview={text_preview}",
        text.len(),
        parse_mode,
        reply_to_message_id,
    );

    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");

    let rendered_text = if parse_mode.is_some_and(|mode| mode.eq_ignore_ascii_case("HTML")) {
        escape_html(text)
    } else {
        text.to_string()
    };

    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "text": rendered_text,
    });

    if let Some(mode) = parse_mode {
        body["parse_mode"] = serde_json::json!(mode);
    }
    if let Some(message_id) = reply_to_message_id.and_then(|v| v.trim().parse::<i64>().ok()) {
        body["reply_to_message_id"] = serde_json::json!(message_id);
    }

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    let body_str = String::from_utf8_lossy(&body);
    let response_preview = truncate_log_preview(body_str.as_ref(), 220);

    if !status.is_success() {
        println!("[telegram] Send FAILED: HTTP {status}: {response_preview}");
        warn!("Telegram API error: HTTP {status}: {body_str}");
        return Err(anyhow::anyhow!(
            "telegram API returned HTTP {status}: {body_str}"
        ));
    }

    println!(
        "[telegram] Message sent successfully to chat_id={chat_id}: HTTP {status}, response={response_preview}"
    );

    let message_id = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("result")?.get("message_id")?.as_i64());
    Ok(message_id)
}

// ---------------------------------------------------------------------------
// Editing messages
// ---------------------------------------------------------------------------

/// Edit an existing Telegram message.
///
/// When `parse_mode` is `Some("HTML")` (or not provided – defaults to `"HTML"`),
/// the text is automatically escaped via [`escape_html`].
///
/// The "message is not modified" error from Telegram (HTTP 400) is silently
/// ignored since it simply means the text has not changed.
pub async fn edit_message(
    client: &reqwest::Client,
    bot_token: &str,
    chat_id: &str,
    message_id: i64,
    text: &str,
    parse_mode: Option<&str>,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{bot_token}/editMessageText");

    let rendered_text = if parse_mode.is_some_and(|mode| mode.eq_ignore_ascii_case("HTML")) {
        escape_html(text)
    } else {
        text.to_string()
    };

    let body = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": rendered_text,
        "parse_mode": parse_mode.unwrap_or("HTML"),
    });

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let resp_body = response.bytes().await.unwrap_or_default();
        let resp_str = String::from_utf8_lossy(&resp_body);
        // Telegram returns 400 "message is not modified" if text unchanged – not a real error.
        if resp_str.contains("message is not modified") {
            return Ok(());
        }
        println!(
            "[telegram] Edit FAILED: message_id={message_id}, HTTP {status}: {}",
            truncate_log_preview(&resp_str, 200)
        );
        return Err(anyhow::anyhow!(
            "telegram editMessageText returned HTTP {status}: {resp_str}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Deleting messages
// ---------------------------------------------------------------------------

/// Delete a Telegram message.
pub async fn delete_message(
    client: &reqwest::Client,
    bot_token: &str,
    chat_id: &str,
    message_id: i64,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{bot_token}/deleteMessage");
    let body = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
    });
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let resp_body = response.bytes().await.unwrap_or_default();
        warn!(
            "Telegram delete error: HTTP {status}: {}",
            String::from_utf8_lossy(&resp_body)
        );
    }
    Ok(())
}
