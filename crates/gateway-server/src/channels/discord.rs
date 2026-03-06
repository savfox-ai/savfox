use std::sync::Arc;

use salvo::prelude::*;
use savfox_channel_discord::{
    DiscordStartMeta, build_command_prompt, parse_interaction_with_resolver, parse_start_meta,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::runtime;
use crate::auto_reply::CommandRegistry;
use crate::bridge::{GatewayChannel, verify_discord_signature};
use crate::config::GatewayConfig;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

fn build_registry_prompt(command_name: &str, data: &Value) -> Option<String> {
    let registry = CommandRegistry::new();
    let canonical = registry
        .resolve_command_name(command_name)
        .or_else(|| registry.resolve_command_name(&format!("/{command_name}")))?;
    build_command_prompt(&canonical, data)
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Text::Json(
        json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        })
        .to_string(),
    ));
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

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("Discord webhook: failed to read body: {err}");
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
        Ok(v) => v,
        Err(err) => {
            warn!("Discord webhook: failed to parse body: {err}");
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
            cfg.bridges
                .discord
                .as_ref()
                .and_then(|b| b.application_public_key.clone())
        })
        .or_else(|| std::env::var("DISCORD_APPLICATION_PUBLIC_KEY").ok());
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
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing Discord signature headers",
            );
            return;
        }
        if !verify_discord_signature(&public_key, signature, timestamp, raw_body.as_ref()) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Discord request signature verification failed",
            );
            return;
        }
    }

    if body.get("type").and_then(|t| t.as_u64()) == Some(1) {
        res.render(Text::Json(json!({"type": 1}).to_string()));
        return;
    }

    let action = match parse_interaction_with_resolver(&body, build_registry_prompt) {
        Ok(action) => action,
        Err(err) => {
            warn!("Discord webhook: failed to parse interaction: {err}");
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
        ChannelAction::StartThread { channel, prompt } => {
            info!(channel = %channel, "Discord: starting thread with prompt: {prompt}");
            let interaction_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| format!("discord:{id}"));
            if runtime::should_drop_duplicate(interaction_id).await {
                res.render(Text::Json(json!({"type": 1}).to_string()));
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayChannel>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Discord webhook: missing gateway bridge state: {err:?}");
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
                Err(err) => {
                    warn!("Discord webhook: missing session store state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store state unavailable",
                    );
                    return;
                }
            };

            res.render(Text::Json(json!({"type": 5}).to_string()));

            let start_meta = to_runtime_start_meta(parse_start_meta(&body));
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
                    bridge,
                    session_store,
                    "discord",
                    channel,
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
            info!(thread_id = %thread_id, decision = %decision, "Discord: approval response");
            res.render(Text::Json(json!({"type": 6}).to_string()));
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {
            res.render(Text::Json(json!({"type": 1}).to_string()));
        }
    }
}
