use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use salvo::prelude::*;
use savfox_channels::whatsapp::{parse_start_meta, parse_webhook_payload};
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{info, warn};

use super::{ensure_inbound_channel_enabled, obtain_channel_and_store, render_error, runtime};
use crate::protocol::ChannelAction;

fn verify_whatsapp_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    computed == expected
}

fn to_runtime_start_meta(
    meta: savfox_channels::whatsapp::WhatsAppStartMeta,
) -> runtime::StartThreadMeta {
    runtime::StartThreadMeta {
        peer_id: meta.from.clone(),
        thread_id: meta.from.clone(),
        parent_thread_id: meta.from,
        ..runtime::StartThreadMeta::default()
    }
}

/// `GET /webhooks/whatsapp`: WhatsApp webhook verification.
#[handler]
pub(crate) async fn webhook_verification_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    if !ensure_inbound_channel_enabled(depot, res, "whatsapp").await {
        return;
    }

    let verify_token = {
        let savfox_home = depot
            .obtain::<Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::whatsapp::resolve_whatsapp_verify_token(&savfox_home)
                .await
                .ok()
                .flatten()
                .or_else(|| std::env::var("WHATSAPP_VERIFY_TOKEN").ok())
        } else {
            std::env::var("WHATSAPP_VERIFY_TOKEN").ok()
        }
    };

    let mode = req.query::<String>("hub.mode").unwrap_or_default();
    let token = req.query::<String>("hub.verify_token").unwrap_or_default();
    let challenge = req.query::<String>("hub.challenge").unwrap_or_default();

    if mode != "subscribe" {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({"error": "invalid mode"}).to_string()));
        return;
    }

    if let Some(expected) = verify_token {
        if token != expected {
            res.status_code(StatusCode::FORBIDDEN);
            res.render(Text::Json(
                json!({"error": "verification failed"}).to_string(),
            ));
            return;
        }
    }

    res.status_code(StatusCode::OK);
    res.render(Text::Plain(challenge));
}

/// `POST /webhooks/whatsapp`: Handle WhatsApp Cloud API webhook events.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "whatsapp").await {
        return;
    }

    let app_secret = {
        let savfox_home = depot
            .obtain::<Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::whatsapp::resolve_whatsapp_app_secret(&savfox_home)
                .await
                .ok()
                .flatten()
                .or_else(|| std::env::var("WHATSAPP_APP_SECRET").ok())
        } else {
            std::env::var("WHATSAPP_APP_SECRET").ok()
        }
    };

    let body_bytes = match req.payload().await {
        Ok(bytes) => bytes.to_vec(),
        Err(err) => {
            warn!("WhatsApp webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to read body",
            );
            return;
        }
    };

    if let Some(secret) = app_secret {
        let signature = req
            .headers()
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if !verify_whatsapp_signature(&secret, &body_bytes, signature) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "signature verification failed",
            );
            return;
        }
    }

    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(err) => {
            warn!("WhatsApp webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to parse JSON",
            );
            return;
        }
    };

    let action = parse_webhook_payload(&body);

    match action {
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            info!(from = %channel_id, "WhatsApp: starting thread with prompt");

            let wa_meta = parse_start_meta(&body);

            let dedupe_key = wa_meta
                .message_id
                .as_deref()
                .map(|id| format!("whatsapp:{id}"));

            if runtime::should_drop_duplicate(dedupe_key).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let Some((gateway_channel, session_store)) = obtain_channel_and_store(depot, res)
            else {
                return;
            };

            let name = wa_meta
                .display_name
                .clone()
                .or_else(|| wa_meta.from.clone());
            let meta = to_runtime_start_meta(wa_meta);

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "whatsapp",
                    channel_id,
                    prompt,
                    name,
                    Some(meta),
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "WhatsApp: approval response");
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }

    res.status_code(StatusCode::OK);
}
