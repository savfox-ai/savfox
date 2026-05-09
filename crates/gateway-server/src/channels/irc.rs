use salvo::prelude::*;
use serde_json::json;
use tracing::info;

use super::runtime;

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(body) = super::parse_json_body(req, res, "irc").await else {
        return;
    };

    let message = body.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let channel = body
        .get("channel")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_owned();
    let prompt = message
        .strip_prefix("!savfox ")
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    let dedupe_key = body
        .get("message_id")
        .or_else(|| body.get("id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("irc:{id}"))
        .or_else(|| {
            if channel.is_empty() || message.is_empty() {
                None
            } else {
                Some(format!(
                    "irc:{}:{}:{}",
                    channel,
                    body.get("nick").and_then(|v| v.as_str()).unwrap_or(""),
                    message
                ))
            }
        });

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if !channel.is_empty() && !prompt.is_empty() {
        let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res)
        else {
            return;
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                gateway_channel,
                session_store,
                "irc",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("IRC webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
