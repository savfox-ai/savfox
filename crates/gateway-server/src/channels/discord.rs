use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use savfox_channels::discord::{
    DiscordEventSink, DiscordInboundMessage, DiscordStartMeta, build_command_prompt,
    parse_display_name, parse_interaction_with_resolver, parse_message_with_resolver,
    parse_start_meta,
};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use super::{obtain_channel_and_store, render_error, runtime};
use crate::auto_reply::CommandRegistry;
use crate::channel::{GatewayChannel, verify_discord_signature};
use crate::config::GatewayConfig;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// HTML template for the OAuth callback result page.
fn callback_html(title: &str, message: &str, success: bool) -> String {
    let color = if success { "#43b581" } else { "#f04747" };
    let icon = if success { "&#10003;" } else { "&#10007;" };
    format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title}</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
  display: flex; justify-content: center; align-items: center; min-height: 100vh;
  margin: 0; background: #2c2f33; color: #dcddde; }}
.card {{ text-align: center; padding: 48px; border-radius: 12px; background: #36393f;
  box-shadow: 0 4px 24px rgba(0,0,0,0.3); max-width: 480px; }}
.icon {{ font-size: 64px; color: {color}; margin-bottom: 16px; }}
h1 {{ font-size: 24px; margin: 0 0 12px; color: #fff; }}
p {{ font-size: 16px; color: #b9bbbe; margin: 0 0 24px; }}
.hint {{ font-size: 13px; color: #72767d; }}
</style></head>
<body><div class="card">
  <div class="icon">{icon}</div>
  <h1>{title}</h1>
  <p>{message}</p>
  <div class="hint">You can close this window.</div>
</div></body></html>"#
    )
}

/// Handles the Discord OAuth2 callback redirect.
///
/// Discord redirects here after a user authorizes the bot via an OAuth2 invite URL.
/// Query params: `code`, `guild_id` (optional), `permissions` (optional).
#[handler]
pub(crate) async fn oauth_callback_handler(req: &mut Request, res: &mut Response) {
    debug!(target: "discord::oauth", uri = %req.uri(), "callback received");
    let code = req.query::<String>("code");
    let guild_id = req.query::<String>("guild_id");
    let error = req.query::<String>("error");

    if let Some(err) = error {
        let description = req
            .query::<String>("error_description")
            .unwrap_or_else(|| "Authorization was denied.".to_string());
        warn!(target: "discord::oauth", error = %err, description = %description, "OAuth callback error");
        res.render(Text::Html(callback_html(
            "Authorization Failed",
            &description,
            false,
        )));
        return;
    }

    if code.is_none() {
        warn!(target: "discord::oauth", "missing code parameter");
        res.render(Text::Html(callback_html(
            "Missing Authorization Code",
            "No authorization code was received from Discord.",
            false,
        )));
        return;
    }

    let guild_msg = guild_id
        .as_deref()
        .map(|id| format!(" to guild {id}"))
        .unwrap_or_default();

    debug!(target: "discord::oauth", guild_msg = %guild_msg, code = %code.as_deref().unwrap_or("?"), "bot authorized");
    info!(target: "discord::oauth", "bot authorized{guild_msg}");
    res.render(Text::Html(callback_html(
        "Bot Authorized",
        &format!(
            "The Discord bot has been successfully added{guild_msg}. You can return to the gateway dashboard."
        ),
        true,
    )));
}

fn build_registry_prompt(command_name: &str, data: &Value) -> Option<String> {
    let registry = CommandRegistry::new();
    let canonical = registry
        .resolve_command_name(command_name)
        .or_else(|| registry.resolve_command_name(&format!("/{command_name}")))?;
    build_command_prompt(&canonical, data)
}

fn resolve_registry_command_name(command: &str) -> Option<String> {
    let registry = CommandRegistry::new();
    registry.resolve_command_name(command)
}

fn to_runtime_start_meta(meta: DiscordStartMeta) -> runtime::StartThreadMeta {
    runtime::StartThreadMeta {
        peer_id: meta.peer_id,
        group_id: meta.guild_id.clone(),
        guild_id: meta.guild_id,
        role_ids: meta.role_ids,
        parent_thread_id: meta.parent_thread_id,
        reply_target: meta.reply_target,
        parent_sender_id: meta.parent_sender_id,
        chat_type: meta.chat_type,
        ..runtime::StartThreadMeta::default()
    }
}

async fn process_inbound_message(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    config_id: String,
    event: DiscordInboundMessage,
) {
    let action = match parse_message_with_resolver(
        &event.payload,
        &event.bot_user_id,
        resolve_registry_command_name,
    ) {
        Ok(parsed) => parsed,
        Err(err) => {
            warn!("Discord gateway message: failed to parse inbound message: {err}");
            return;
        }
    };

    let dedupe_key = action.dedupe_key.clone();
    match action.action {
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            info!(channel = %channel_id, "Discord gateway: starting thread with prompt: {prompt}");
            if runtime::should_drop_duplicate(dedupe_key).await {
                return;
            }

            let mut start_meta = to_runtime_start_meta(parse_start_meta(&event.payload));
            start_meta.saved_channel_config_id = Some(config_id);
            // Use the user's own message ID as reply_target so the bot's
            // streamed response appears as a reply to the user's message.
            if start_meta.reply_target.is_none() {
                start_meta.reply_target = event
                    .payload
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .map(ToString::to_string);
            }
            let sender_name = parse_display_name(&event.payload);

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "discord",
                    channel_id,
                    prompt,
                    sender_name,
                    Some(start_meta),
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            info!(
                thread_id = %thread_id,
                decision = %decision,
                "Discord gateway: approval response"
            );
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }
}

struct DiscordRuntimeSink {
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    config_id: String,
}

#[async_trait]
impl DiscordEventSink for DiscordRuntimeSink {
    async fn handle_message(&self, event: DiscordInboundMessage) {
        process_inbound_message(
            Arc::clone(&self.channel),
            Arc::clone(&self.session_store),
            self.config_id.clone(),
            event,
        )
        .await;
    }
}

pub(crate) fn discord_sink(
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    config_id: String,
) -> Arc<dyn DiscordEventSink> {
    Arc::new(DiscordRuntimeSink {
        channel,
        session_store,
        config_id,
    })
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    debug!(target: "discord::webhook", method = %req.method(), uri = %req.uri(), "incoming request");
    let raw_body = match req.payload().await {
        Ok(bytes) => {
            debug!(target: "discord::webhook", bytes = bytes.len(), "body received");
            bytes.clone()
        }
        Err(err) => {
            warn!(target: "discord::webhook", error = %err, "failed to read body");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };
    let body = match serde_json::from_slice::<Value>(raw_body.as_ref()) {
        Ok(v) => {
            let interaction_type = v.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
            let interaction_name = v
                .get("data")
                .and_then(|d| d.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("?");
            debug!(target: "discord::webhook", interaction_type = interaction_type, command = interaction_name, "parsed payload");
            v
        }
        Err(err) => {
            warn!(target: "discord::webhook", error = %err, "JSON parse error");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Discord payload: {err}"),
            );
            return;
        }
    };

    let public_key = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.channels
                .discord
                .as_ref()
                .and_then(|b| b.application_public_key.clone())
        })
        .or_else(|| std::env::var("DISCORD_APPLICATION_PUBLIC_KEY").ok());
    debug!(target: "discord::webhook", configured = public_key.is_some(), "public_key status");
    if let Some(public_key) = public_key {
        let signature = req
            .headers()
            .get("x-signature-ed25519")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let timestamp = req
            .headers()
            .get("x-signature-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if signature.is_empty() || timestamp.is_empty() {
            warn!(target: "discord::webhook", sig_empty = signature.is_empty(), ts_empty = timestamp.is_empty(), "REJECT: missing signature headers");
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing Discord signature headers",
            );
            return;
        }
        if !verify_discord_signature(&public_key, signature, timestamp, raw_body.as_ref()) {
            warn!(target: "discord::webhook", "REJECT: signature verification failed");
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Discord request signature verification failed",
            );
            return;
        }
        debug!(target: "discord::webhook", "signature verified OK");
    }

    if body.get("type").and_then(|t| t.as_u64()) == Some(1) {
        debug!(target: "discord::webhook", "PING received, responding with PONG");
        res.render(Text::Json(json!({"type": 1}).to_string()));
        return;
    }

    let action = match parse_interaction_with_resolver(&body, build_registry_prompt) {
        Ok(action) => {
            debug!(target: "discord::webhook", action = ?action, "parsed action");
            action
        }
        Err(err) => {
            warn!(target: "discord::webhook", error = %err, "failed to parse interaction");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Discord interaction: {err}"),
            );
            return;
        }
    };

    match action {
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            debug!(target: "discord::webhook", channel = %channel_id, prompt_len = prompt.len(), "StartThread");
            info!(channel = %channel_id, "Discord: starting thread with prompt: {prompt}");
            let interaction_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| format!("discord:{id}"));
            if runtime::should_drop_duplicate(interaction_id).await {
                debug!(target: "discord::webhook", "dropping duplicate interaction");
                res.render(Text::Json(json!({"type": 1}).to_string()));
                return;
            }

            let Some((gateway_channel, session_store)) = obtain_channel_and_store(depot, res)
            else {
                return;
            };

            res.render(Text::Json(json!({"type": 5}).to_string()));

            let start_meta = to_runtime_start_meta(parse_start_meta(&body));
            debug!(target: "discord::webhook", peer_id = ?start_meta.peer_id, guild_id = ?start_meta.guild_id, "spawning thread pipeline");
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "discord",
                    channel_id,
                    prompt,
                    None,
                    Some(start_meta),
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            debug!(target: "discord::webhook", thread_id = %thread_id, decision = %decision, "Approve");
            info!(thread_id = %thread_id, decision = %decision, "Discord: approval response");
            res.render(Text::Json(json!({"type": 6}).to_string()));
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {
            debug!(target: "discord::webhook", "action ignored (Ignore/SendToThread)");
            res.render(Text::Json(json!({"type": 1}).to_string()));
        }
    }
}
