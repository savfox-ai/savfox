use std::sync::Arc;

use salvo::prelude::*;
use savfox_channels::slack::{
    SlackStartMeta, parse_event_with_resolver, parse_payload_bytes,
    parse_slash_command_with_resolver, parse_start_meta,
};
use serde_json::json;
use tracing::{info, warn};

use super::{ensure_inbound_channel_enabled, obtain_channel_and_store, render_error, runtime};
use crate::auto_reply::CommandRegistry;
use crate::channel::is_slack_timestamp_fresh;
use crate::protocol::ChannelAction;

fn resolve_registry_command_name(command: &str) -> Option<String> {
    let registry = CommandRegistry::new();
    registry.resolve_command_name(command)
}

fn to_runtime_start_meta(meta: SlackStartMeta) -> runtime::StartThreadMeta {
    runtime::StartThreadMeta {
        peer_id: meta.peer_id,
        group_id: meta.channel_id.filter(|channel| !channel.starts_with('D')),
        thread_id: meta.thread_id.clone(),
        parent_thread_id: meta.thread_id,
        reply_target: meta.reply_target,
        team_id: meta.team_id,
        account_id: meta.account_id,
        parent_sender_id: meta.parent_sender_id,
        slack_groups: meta.slack_groups,
        chat_type: meta.chat_type,
        ..runtime::StartThreadMeta::default()
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "slack").await {
        return;
    }

    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("Slack webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };
    let body = match parse_payload_bytes(raw_body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            warn!("Slack webhook: failed to parse payload: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse payload: {err}"),
            );
            return;
        }
    };

    let signing_secret = {
        let savfox_home = depot
            .obtain::<Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::slack::resolve_slack_signing_secret(&savfox_home)
                .await
                .ok()
                .flatten()
                .or_else(|| std::env::var("SLACK_SIGNING_SECRET").ok())
        } else {
            std::env::var("SLACK_SIGNING_SECRET").ok()
        }
    };
    if let Some(secret) = signing_secret {
        let signature = req
            .headers()
            .get("x-slack-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let timestamp = req
            .headers()
            .get("x-slack-request-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if signature.is_empty() || timestamp.is_empty() {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing Slack signature headers",
            );
            return;
        }
        let now_epoch_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !is_slack_timestamp_fresh(timestamp, 300, now_epoch_secs) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "stale_signature",
                "Slack request timestamp is outside the allowed replay window",
            );
            return;
        }
        if !crate::channel::verify_slack_signature(&secret, timestamp, signature, raw_body.as_ref())
        {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Slack request signature verification failed",
            );
            return;
        }
    }

    if body.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
        res.render(Text::Json(json!({"challenge": challenge}).to_string()));
        return;
    }

    let action = if body.get("command").is_some() {
        parse_slash_command_with_resolver(&body, resolve_registry_command_name)
    } else {
        parse_event_with_resolver(&body, resolve_registry_command_name)
    };

    match action {
        Ok(ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        }) => {
            info!(channel = %channel_id, "Slack: starting thread with prompt: {prompt}");
            let dedupe_key = body
                .get("event")
                .and_then(|e| e.get("event_ts"))
                .and_then(|v| v.as_str())
                .map(|id| format!("slack:{id}"))
                .or_else(|| {
                    body.get("trigger_id")
                        .and_then(|v| v.as_str())
                        .map(|id| format!("slack:{id}"))
                });
            if runtime::should_drop_duplicate(dedupe_key).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let Some((gateway_channel, session_store)) = obtain_channel_and_store(depot, res)
            else {
                return;
            };

            res.render(Text::Json(
                json!({
                    "response_type": "in_channel",
                    "text": "Starting Savfox agent..."
                })
                .to_string(),
            ));

            let start_meta = to_runtime_start_meta(parse_start_meta(&body));
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "slack",
                    channel_id,
                    prompt,
                    None,
                    Some(start_meta),
                )
                .await;
            });
        }
        Ok(ChannelAction::Approve {
            thread_id,
            decision,
        }) => {
            info!(thread_id = %thread_id, decision = %decision, "Slack: approval response");
            res.status_code(StatusCode::OK);
        }
        Ok(ChannelAction::Ignore | ChannelAction::SendToThread { .. }) => {
            res.status_code(StatusCode::OK);
        }
        Err(err) => {
            warn!("Slack webhook parse error: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Slack action: {err}"),
            );
        }
    }
}
