use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Matrix chat bridge using the Client-Server API with appservice or webhook mode.
pub(crate) struct MatrixChannel {
    homeserver_url: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MatrixChannel {
    #[must_use]
    pub(crate) fn new(
        homeserver_url: String,
        access_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            homeserver_url,
            access_token,
            http_client,
        }
    }
}

#[allow(clippy::print_stdout)]
fn debug_matrix_inbound_message(room_id: &str, sender: &str, text: &str) {
    println!("[matrix][inbound] room={room_id} sender={sender} text={text}");
}

fn parse_invite_event(event: &Value) -> Option<(String, Option<String>)> {
    if event.get("type").and_then(|t| t.as_str()) != Some("m.room.member") {
        return None;
    }

    let membership = event
        .get("content")
        .and_then(|c| c.get("membership"))
        .and_then(|m| m.as_str())?;
    if !membership.eq_ignore_ascii_case("invite") {
        return None;
    }

    let room_id = event.get("room_id").and_then(|r| r.as_str())?.trim();
    if room_id.is_empty() {
        return None;
    }

    let invited_user_id = event
        .get("state_key")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    Some((room_id.to_owned(), invited_user_id))
}

fn first_non_empty_config_string(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
    })
}

pub(crate) async fn log_configured_matrix_startup(savfox_home: &PathBuf) -> anyhow::Result<()> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;
    for config in all_configs {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("matrix") {
            continue;
        }

        let homeserver = config
            .config
            .as_object()
            .and_then(|raw| {
                first_non_empty_config_string(raw, &["homeserver", "homeserver_url", "server_url"])
            })
            .unwrap_or_else(|| "https://matrix.org".to_string());

        info!(
            channel_id = %config.id,
            homeserver = %homeserver,
            "Matrix bridge starting with homeserver URL: {homeserver}"
        );
    }
    Ok(())
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Json(json!({
        "error": {
            "code": code,
            "message": message.into(),
        }
    })));
}

#[async_trait]
impl Channel for MatrixChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        println!(
            "Matrix bridge starting with homeserver URL: {}",
            self.homeserver_url
        );
        info!(homeserver = %self.homeserver_url, "Matrix bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );
        let body = json!({
            "msgtype": "m.text",
            "body": message,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }

        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );

        let html = text.replace('\n', "<br/>");
        let body = json!({
            "msgtype": "m.text",
            "body": text,
            "format": "org.matrix.custom.html",
            "formatted_body": html,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix rich send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        // Matrix appservice transaction format:
        // { "events": [{ "type": "m.room.message", "content": {...}, "room_id": "...", "sender":
        // "..." }] }
        let events = payload
            .get("events")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        for event in &events {
            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if event_type != "m.room.message" {
                continue;
            }

            let content = event.get("content").unwrap_or(&Value::Null);
            let msgtype = content
                .get("msgtype")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if msgtype != "m.text" {
                continue;
            }

            let body = content.get("body").and_then(|b| b.as_str()).unwrap_or("");
            let room_id = event
                .get("room_id")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_owned();
            let sender = event
                .get("sender")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_owned();

            debug_matrix_inbound_message(&room_id, &sender, body);

            if let Some(prompt) = body.strip_prefix("!savfox ") {
                let prompt = prompt.trim().to_owned();
                if !prompt.is_empty() {
                    return Ok(BridgeAction::StartThread {
                        channel: room_id,
                        prompt,
                    });
                }
            }
        }

        Ok(BridgeAction::Ignore)
    }
}

/// Salvo handler for Matrix appservice webhook.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<Value>().await {
        Ok(body) => body,
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("invalid JSON: {err}"),
            );
            return;
        }
    };

    let mut action = BridgeAction::Ignore;
    let mut dedupe_key: Option<String> = None;
    let mut rooms_to_auto_join: Vec<(String, Option<String>)> = Vec::new();
    if let Some(events) = body.get("events").and_then(|e| e.as_array()) {
        for event in events {
            if let Some((room_id, invited_user_id)) = parse_invite_event(event) {
                if !rooms_to_auto_join
                    .iter()
                    .any(|(existing_room_id, _)| existing_room_id.eq_ignore_ascii_case(&room_id))
                {
                    rooms_to_auto_join.push((room_id, invited_user_id));
                }
            }

            if event.get("type").and_then(|t| t.as_str()) != Some("m.room.message") {
                continue;
            }
            let content = event.get("content").unwrap_or(&Value::Null);
            if content.get("msgtype").and_then(|m| m.as_str()) != Some("m.text") {
                continue;
            }
            let text = content.get("body").and_then(|b| b.as_str()).unwrap_or("");
            let room_id = event.get("room_id").and_then(|r| r.as_str()).unwrap_or("");
            let sender = event.get("sender").and_then(|s| s.as_str()).unwrap_or("");
            debug_matrix_inbound_message(room_id, sender, text);
            if let Some(prompt) = text.strip_prefix("!savfox ").map(str::trim)
                && !prompt.is_empty()
            {
                let room_id = room_id.to_owned();
                if room_id.is_empty() {
                    break;
                }
                dedupe_key = event
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("matrix:{id}"));
                action = BridgeAction::StartThread {
                    channel: room_id,
                    prompt: prompt.to_owned(),
                };
                break;
            }
        }
    }

    if !rooms_to_auto_join.is_empty() {
        match depot.obtain::<Arc<GatewayBridge>>() {
            Ok(bridge) => {
                let bridge = bridge.clone();
                for (room_id, invited_user_id) in rooms_to_auto_join {
                    if let Err(err) = bridge
                        .auto_join_matrix_invited_room(&room_id, invited_user_id.as_deref())
                        .await
                    {
                        warn!(
                            room_id,
                            invited_user_id = invited_user_id.as_deref().unwrap_or(""),
                            error = %err,
                            "Matrix invite auto-join failed"
                        );
                    }
                }
            }
            Err(_) => {
                warn!("Matrix invite received but gateway bridge state is unavailable");
            }
        }
    }

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let BridgeAction::StartThread { channel, prompt } = action {
        let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
            Ok(bridge) => bridge.clone(),
            Err(_) => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "gateway bridge state unavailable",
                );
                return;
            }
        };
        let session_store = match depot.obtain::<Arc<SessionStore>>() {
            Ok(store) => store.clone(),
            Err(_) => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "session store state unavailable",
                );
                return;
            }
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                bridge,
                session_store,
                "matrix",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Matrix webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
