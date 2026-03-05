use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::config::{GatewayConfig, MsTeamsChannelConfig};
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Bot Framework OAuth2 token endpoint.
const BOT_FRAMEWORK_TOKEN_URL: &str =
    "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";

/// Bot Framework activity API base URL.
const BOT_FRAMEWORK_API: &str = "https://smba.trafficmanager.net/teams";

/// Cached OAuth2 access token with expiry.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    /// Epoch seconds at which the token expires (with a safety margin).
    expires_at: u64,
}

/// MS Teams bot bridge using Bot Framework REST API v3.
pub(crate) struct MsTeamsChannel {
    config: MsTeamsChannelConfig,
    http_client: reqwest::Client,
    /// Cached Bot Framework OAuth2 bearer token.
    token_cache: Arc<RwLock<Option<CachedToken>>>,
}

impl MsTeamsChannel {
    #[must_use]
    pub(crate) fn new(config: MsTeamsChannelConfig, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Acquire a valid Bot Framework access token, refreshing if expired.
    async fn get_access_token(&self) -> anyhow::Result<String> {
        // Fast path: check if a cached token is still valid.
        {
            let cache = self.token_cache.read().await;
            if let Some(ref cached) = *cache {
                let now = now_epoch_secs();
                if now < cached.expires_at {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        // Slow path: fetch a new token.
        let token_url = if let Some(ref tenant_id) = self.config.tenant_id {
            format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token")
        } else {
            BOT_FRAMEWORK_TOKEN_URL.to_owned()
        };

        let form_body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", &self.config.app_id)
            .append_pair("client_secret", &self.config.app_password)
            .append_pair("scope", "https://api.botframework.com/.default")
            .finish();

        let response = self
            .http_client
            .post(&token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let err_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Bot Framework token request failed: HTTP {status}: {err_body}");
        }

        let token_resp: Value = response.json().await?;
        let access_token = token_resp
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing access_token in token response"))?
            .to_owned();
        let expires_in = token_resp
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);

        // Cache with a 5-minute safety margin.
        let expires_at = now_epoch_secs() + expires_in.saturating_sub(300);
        let cached = CachedToken {
            access_token: access_token.clone(),
            expires_at,
        };

        {
            let mut cache = self.token_cache.write().await;
            *cache = Some(cached);
        }

        Ok(access_token)
    }

    /// Send an activity reply via Bot Framework REST API v3.
    async fn send_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;
        let url = format!(
            "{}/v3/conversations/{}/activities",
            service_url.trim_end_matches('/'),
            conversation_id,
        );

        let body = json!({
            "type": "message",
            "text": text,
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Bot Framework API error: HTTP {status}: {resp_body}");
        }

        Ok(())
    }

    /// Send an Adaptive Card activity via Bot Framework REST API v3.
    async fn send_adaptive_card(
        &self,
        service_url: &str,
        conversation_id: &str,
        card: Value,
        summary: &str,
    ) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;
        let url = format!(
            "{}/v3/conversations/{}/activities",
            service_url.trim_end_matches('/'),
            conversation_id,
        );

        let body = json!({
            "type": "message",
            "summary": summary,
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": card,
            }],
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Bot Framework API error (card): HTTP {status}: {resp_body}");
        }

        Ok(())
    }

    /// Parse an incoming Bot Framework activity into a `BridgeAction`.
    fn parse_activity(payload: &Value) -> anyhow::Result<BridgeAction> {
        let activity_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match activity_type {
            "message" => {
                let raw_text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");

                // Strip @mention tags. Teams wraps mentions as <at>BotName</at>.
                let prompt = strip_at_mentions(raw_text).trim().to_owned();

                let conversation_id = payload
                    .get("conversation")
                    .and_then(|c| c.get("id"))
                    .and_then(|id| id.as_str())
                    .unwrap_or("unknown")
                    .to_owned();

                if prompt.is_empty() {
                    Ok(BridgeAction::Ignore)
                } else {
                    Ok(BridgeAction::StartThread {
                        channel: conversation_id,
                        prompt,
                    })
                }
            }
            "invoke" => {
                // Handle invoke activities (e.g., Adaptive Card action submits).
                let invoke_value = payload.get("value").unwrap_or(&Value::Null);

                let action_type = invoke_value
                    .get("action")
                    .and_then(|a| a.as_str())
                    .unwrap_or("");

                if let Some(thread_id) = action_type.strip_prefix("approve:") {
                    Ok(BridgeAction::Approve {
                        thread_id: thread_id.to_owned(),
                        decision: true,
                    })
                } else if let Some(thread_id) = action_type.strip_prefix("deny:") {
                    Ok(BridgeAction::Approve {
                        thread_id: thread_id.to_owned(),
                        decision: false,
                    })
                } else {
                    Ok(BridgeAction::Ignore)
                }
            }
            _ => Ok(BridgeAction::Ignore),
        }
    }
}

/// Strip Teams `<at>...</at>` mention tags from the message text.
fn strip_at_mentions(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find("<at>") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find("</at>") {
            remaining = &remaining[start + end + 5..];
        } else {
            // Malformed tag; keep the rest as-is.
            remaining = &remaining[start..];
            break;
        }
    }
    result.push_str(remaining);
    result
}

/// Build an Adaptive Card JSON from a `RichMessage`.
fn build_adaptive_card(msg: &RichMessage) -> Value {
    let mut body_items: Vec<Value> = Vec::new();

    // Title block.
    if let Some(ref title) = msg.title {
        body_items.push(json!({
            "type": "TextBlock",
            "text": title,
            "size": "Medium",
            "weight": "Bolder",
            "wrap": true,
        }));
    }

    // Main text block.
    body_items.push(json!({
        "type": "TextBlock",
        "text": msg.text,
        "wrap": true,
    }));

    // Code blocks rendered as monospace text blocks.
    for block in &msg.code_blocks {
        body_items.push(json!({
            "type": "TextBlock",
            "text": format!("```{}\n{}\n```", block.language, block.content),
            "fontType": "Monospace",
            "wrap": true,
        }));
    }

    json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.4",
        "body": body_items,
    })
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

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[async_trait]
impl Channel for MsTeamsChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("MS Teams bridge initialized (Bot Framework webhook mode)");
        // Pre-warm the OAuth2 token cache.
        match self.get_access_token().await {
            Ok(_) => info!("MS Teams: Bot Framework token acquired"),
            Err(err) => warn!("MS Teams: failed to pre-acquire token (will retry on send): {err}"),
        }
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        // Channel format: "serviceUrl|conversationId" or just conversationId.
        let (service_url, conversation_id) = parse_channel(channel);

        self.send_activity(service_url, conversation_id, message)
            .await?;

        info!(conversation_id = %conversation_id, "MS Teams message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let (service_url, conversation_id) = parse_channel(channel);
        let card = build_adaptive_card(&msg);
        let summary = msg.title.as_deref().unwrap_or("Savfox");

        self.send_adaptive_card(service_url, conversation_id, card, summary)
            .await?;

        info!(conversation_id = %conversation_id, "MS Teams rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_activity(&payload)
    }
}

/// Parse a channel string into (service_url, conversation_id).
///
/// If the channel contains `|`, the part before is the service URL and the part after
/// is the conversation ID. Otherwise, the default Bot Framework API URL is used.
fn parse_channel(channel: &str) -> (&str, &str) {
    if let Some((service_url, conversation_id)) = channel.split_once('|') {
        (service_url, conversation_id)
    } else {
        (BOT_FRAMEWORK_API, channel)
    }
}

/// `POST /webhooks/msteams` -- Handle MS Teams Bot Framework webhook activities.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("MS Teams webhook: failed to read body: {err}");
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
            warn!("MS Teams webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse MS Teams payload: {err}"),
            );
            return;
        }
    };

    // HMAC-SHA256 signature verification.
    // The Bot Framework sends an Authorization header with a Bearer JWT token.
    // For webhook-based bots, we verify using the app password as HMAC secret
    // if the `Authorization` header is present.
    let app_password = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| cfg.bridges.msteams.as_ref().map(|b| b.app_password.clone()))
        .or_else(|| std::env::var("MSTEAMS_APP_PASSWORD").ok());
    if let Some(secret) = app_password {
        let auth_header = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if auth_header.is_empty() {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_authorization",
                "missing Authorization header",
            );
            return;
        }

        // Verify HMAC-SHA256 if a custom signature header is present.
        let hmac_signature = req
            .headers()
            .get("x-ms-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !hmac_signature.is_empty()
            && !crate::bridge::verify_webhook_hmac(&secret, hmac_signature, raw_body.as_ref())
        {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "MS Teams request signature verification failed",
            );
            return;
        }
    }

    let action = match MsTeamsChannel::parse_activity(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("MS Teams webhook: failed to parse activity: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse MS Teams activity: {err}"),
            );
            return;
        }
    };

    match action {
        BridgeAction::StartThread { channel, prompt } => {
            info!(conversation_id = %channel, "MS Teams: starting thread with prompt: {prompt}");

            let activity_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| format!("msteams:{id}"));
            if runtime::should_drop_duplicate(activity_id).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("MS Teams webhook: missing gateway bridge state: {err:?}");
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
                    warn!("MS Teams webhook: missing session store state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store state unavailable",
                    );
                    return;
                }
            };

            // Encode service URL into channel so outbound replies can route back.
            let service_url = body
                .get("serviceUrl")
                .and_then(|v| v.as_str())
                .unwrap_or(BOT_FRAMEWORK_API);
            let outbound_channel = format!("{service_url}|{channel}");

            // Respond 200 OK immediately; process asynchronously.
            res.status_code(StatusCode::OK);

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    bridge,
                    session_store,
                    "msteams",
                    outbound_channel,
                    prompt,
                    None,
                )
                .await;
            });
        }
        BridgeAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "MS Teams: approval response");
            res.status_code(StatusCode::OK);
        }
        BridgeAction::Ignore | BridgeAction::SendToThread { .. } => {
            res.status_code(StatusCode::OK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_at_mentions_removes_single() {
        let input = "<at>Savfox</at> hello world";
        assert_eq!(strip_at_mentions(input), " hello world");
    }

    #[test]
    fn strip_at_mentions_removes_multiple() {
        let input = "<at>Savfox</at> <at>User</at> do something";
        assert_eq!(strip_at_mentions(input), "  do something");
    }

    #[test]
    fn strip_at_mentions_no_tags() {
        let input = "plain message";
        assert_eq!(strip_at_mentions(input), "plain message");
    }

    #[test]
    fn parse_activity_message_with_mention() {
        let payload = json!({
            "type": "message",
            "text": "<at>Savfox</at> build a website",
            "conversation": {"id": "conv-123"},
        });
        let action = MsTeamsChannel::parse_activity(&payload).unwrap();
        match action {
            BridgeAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "conv-123");
                assert_eq!(prompt, "build a website");
            }
            _ => panic!("expected StartThread"),
        }
    }

    #[test]
    fn parse_activity_empty_text_is_ignored() {
        let payload = json!({
            "type": "message",
            "text": "<at>Savfox</at>",
            "conversation": {"id": "conv-456"},
        });
        let action = MsTeamsChannel::parse_activity(&payload).unwrap();
        assert!(matches!(action, BridgeAction::Ignore));
    }

    #[test]
    fn parse_activity_invoke_approve() {
        let payload = json!({
            "type": "invoke",
            "value": {"action": "approve:thread-1"},
        });
        let action = MsTeamsChannel::parse_activity(&payload).unwrap();
        match action {
            BridgeAction::Approve {
                thread_id,
                decision,
            } => {
                assert_eq!(thread_id, "thread-1");
                assert!(decision);
            }
            _ => panic!("expected Approve"),
        }
    }

    #[test]
    fn parse_activity_invoke_deny() {
        let payload = json!({
            "type": "invoke",
            "value": {"action": "deny:thread-2"},
        });
        let action = MsTeamsChannel::parse_activity(&payload).unwrap();
        match action {
            BridgeAction::Approve {
                thread_id,
                decision,
            } => {
                assert_eq!(thread_id, "thread-2");
                assert!(!decision);
            }
            _ => panic!("expected Approve (deny)"),
        }
    }

    #[test]
    fn parse_activity_unknown_type_ignored() {
        let payload = json!({
            "type": "conversationUpdate",
            "membersAdded": [],
        });
        let action = MsTeamsChannel::parse_activity(&payload).unwrap();
        assert!(matches!(action, BridgeAction::Ignore));
    }

    #[test]
    fn parse_channel_with_service_url() {
        let (service, conv) = parse_channel("https://smba.trafficmanager.net/teams|conv-123");
        assert_eq!(service, "https://smba.trafficmanager.net/teams");
        assert_eq!(conv, "conv-123");
    }

    #[test]
    fn parse_channel_without_service_url() {
        let (service, conv) = parse_channel("conv-456");
        assert_eq!(service, BOT_FRAMEWORK_API);
        assert_eq!(conv, "conv-456");
    }

    #[test]
    fn adaptive_card_has_required_fields() {
        let msg = RichMessage {
            text: "Hello".to_owned(),
            code_blocks: vec![],
            title: Some("Test".to_owned()),
            color: None,
        };
        let card = build_adaptive_card(&msg);
        assert_eq!(card["type"], "AdaptiveCard");
        assert_eq!(card["version"], "1.4");
        assert!(card["body"].is_array());
        assert_eq!(card["body"].as_array().unwrap().len(), 2); // title + text
    }
}
