use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::channel::GatewayChannel;

// ── StreamEvent (unchanged) ──────────────────────────────────────────

/// Events sent from the `on_delta` callback to the streaming writer task.
pub(crate) enum StreamEvent {
    /// A text delta from the AI model.
    Delta(String),
    /// The AI model finished. Carries an optional footer to append.
    Complete { footer: Option<String> },
}

// ── StreamSink trait ─────────────────────────────────────────────────

/// Platform-agnostic interface for sending, editing, and deleting messages.
#[async_trait]
pub(crate) trait StreamSink: Send + Sync + 'static {
    /// Send the initial message and return its ID (platform-specific string).
    async fn send_initial(&self, text: &str) -> Option<String>;
    /// Edit the message identified by `message_id` with new content.
    async fn edit_message(&self, message_id: &str, text: &str);
    /// Delete the message identified by `message_id`.
    async fn delete_message(&self, message_id: &str);
    /// Maximum text length for a single message on this platform.
    fn max_length(&self) -> usize;
    /// Minimum interval between edits to avoid rate-limiting.
    fn throttle_interval(&self) -> Duration;
    /// Platform name for logging.
    fn platform_tag(&self) -> &'static str;
}

// ── ChannelStreamWriter ──────────────────────────────────────────────

/// Minimum accumulated characters before the first flush.
const MIN_FIRST_CHARS: usize = 20;

/// Generic stream writer that works with any platform implementing `StreamSink`.
pub(crate) struct ChannelStreamWriter {
    sink: Box<dyn StreamSink>,
    /// If an initial "⏳" status message was sent, its ID is stored here
    /// so it can be deleted when the first real delta arrives.
    status_msg_id: Option<String>,
}

impl ChannelStreamWriter {
    pub fn new(sink: Box<dyn StreamSink>, status_msg_id: Option<String>) -> Self {
        Self {
            sink,
            status_msg_id,
        }
    }

    /// Run the streaming loop.  Returns `true` if at least one message was
    /// sent (meaning the caller should skip its own `send_with_retry`).
    pub async fn run(self, mut rx: mpsc::UnboundedReceiver<StreamEvent>) -> bool {
        let mut buffer = String::new();
        let mut message_id: Option<String> = None;
        let mut last_flush = Instant::now();
        let mut flushed_len: usize = 0;
        let mut completed_footer: Option<String> = None;
        let mut status_deleted = self.status_msg_id.is_none();

        let tag = self.sink.platform_tag();
        let throttle = self.sink.throttle_interval();
        let max_len = self.sink.max_length();

        debug!(platform = tag, "Stream writer started");

        loop {
            let event = tokio::time::timeout(throttle, rx.recv()).await;

            match event {
                Ok(Some(StreamEvent::Delta(delta))) => {
                    buffer.push_str(&delta);

                    // Delete the status message on first delta.
                    if !status_deleted {
                        if let Some(ref sid) = self.status_msg_id {
                            self.sink.delete_message(sid).await;
                        }
                        status_deleted = true;
                    }

                    let should_flush = last_flush.elapsed() >= throttle
                        && buffer.len() > flushed_len
                        && (message_id.is_some() || buffer.len() >= MIN_FIRST_CHARS);
                    if should_flush {
                        self.flush(tag, max_len, &buffer, &mut message_id, &mut flushed_len)
                            .await;
                        last_flush = Instant::now();
                    }
                }
                Ok(Some(StreamEvent::Complete { footer })) => {
                    completed_footer = footer;
                    while let Ok(msg) = rx.try_recv() {
                        if let StreamEvent::Delta(d) = msg {
                            buffer.push_str(&d);
                        }
                    }
                    break;
                }
                Ok(None) => break,
                Err(_timeout) => {
                    if buffer.len() > flushed_len
                        && (message_id.is_some() || buffer.len() >= MIN_FIRST_CHARS)
                    {
                        // Delete status if we haven't yet (first content arriving via timeout
                        // path).
                        if !status_deleted {
                            if let Some(ref sid) = self.status_msg_id {
                                self.sink.delete_message(sid).await;
                            }
                            status_deleted = true;
                        }
                        self.flush(tag, max_len, &buffer, &mut message_id, &mut flushed_len)
                            .await;
                        last_flush = Instant::now();
                    }
                }
            }
        }

        // Delete status if never deleted (no deltas received at all).
        if !status_deleted
            && let Some(ref sid) = self.status_msg_id {
                self.sink.delete_message(sid).await;
            }

        // Final flush with footer.
        if let Some(footer) = &completed_footer {
            buffer.push_str(footer);
        }
        if !buffer.is_empty() {
            self.flush(tag, max_len, &buffer, &mut message_id, &mut flushed_len)
                .await;
        }

        let sent = message_id.is_some();
        debug!(
            platform = tag,
            streamed = sent,
            total_len = buffer.len(),
            "Stream writer finished"
        );

        // If the final text exceeded the platform limit, delete the partial
        // streaming message and return false so the caller falls back to
        // the chunked send_with_retry path.
        if buffer.len() > max_len {
            if let Some(ref mid) = message_id {
                self.sink.delete_message(mid).await;
            }
            return false;
        }

        sent
    }

    async fn flush(
        &self,
        tag: &str,
        max_len: usize,
        text: &str,
        message_id: &mut Option<String>,
        flushed_len: &mut usize,
    ) {
        let display_text = if text.len() > max_len {
            &text[..text.floor_char_boundary(max_len)]
        } else {
            text
        };

        if let Some(ref mid) = *message_id {
            self.sink.edit_message(mid, display_text).await;
        } else {
            match self.sink.send_initial(display_text).await {
                Some(mid) => {
                    debug!(platform = tag, id = %mid, "Initial message sent");
                    *message_id = Some(mid);
                }
                None => {
                    debug!(platform = tag, "Initial send returned no id");
                }
            }
        }
        *flushed_len = text.len();
    }
}

// ── Platform Sinks ───────────────────────────────────────────────────
//
// Each sink holds only a `reqwest::Client` and the credentials needed to
// call the stateless helpers in `savfox_channels::{platform}::client`.
// This decouples the streaming layer from `GatewayChannel`.

/// Telegram streaming sink.
pub(crate) struct TelegramSink {
    client: reqwest::Client,
    bot_token: String,
    chat_id: String,
    reply_to: Option<String>,
}

impl TelegramSink {
    pub fn new(
        client: reqwest::Client,
        bot_token: String,
        chat_id: String,
        reply_to: Option<String>,
    ) -> Self {
        Self {
            client,
            bot_token,
            chat_id,
            reply_to,
        }
    }
}

#[async_trait]
impl StreamSink for TelegramSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        match savfox_channels::telegram::client::send_message(
            &self.client,
            &self.bot_token,
            &self.chat_id,
            text,
            Some("HTML"),
            self.reply_to.as_deref(),
        )
        .await
        {
            Ok(Some(mid)) => Some(mid.to_string()),
            Ok(None) => None,
            Err(err) => {
                warn!("[telegram-stream] send failed: {err}");
                None
            }
        }
    }

    async fn edit_message(&self, message_id: &str, text: &str) {
        if let Ok(mid) = message_id.parse::<i64>() {
            let _ = savfox_channels::telegram::client::edit_message(
                &self.client,
                &self.bot_token,
                &self.chat_id,
                mid,
                text,
                Some("HTML"),
            )
            .await;
        }
    }

    async fn delete_message(&self, message_id: &str) {
        if let Ok(mid) = message_id.parse::<i64>() {
            let _ = savfox_channels::telegram::client::delete_message(
                &self.client,
                &self.bot_token,
                &self.chat_id,
                mid,
            )
            .await;
        }
    }

    fn max_length(&self) -> usize {
        4000
    }

    fn throttle_interval(&self) -> Duration {
        Duration::from_millis(1500)
    }

    fn platform_tag(&self) -> &'static str {
        "telegram"
    }
}

/// Discord streaming sink.
pub(crate) struct DiscordSink {
    client: reqwest::Client,
    bot_token: String,
    channel_id: String,
    reply_to: Option<String>,
}

impl DiscordSink {
    pub fn new(
        client: reqwest::Client,
        bot_token: String,
        channel_id: String,
        reply_to: Option<String>,
    ) -> Self {
        Self {
            client,
            bot_token,
            channel_id,
            reply_to,
        }
    }
}

#[async_trait]
impl StreamSink for DiscordSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        match savfox_channels::discord::client::send_message_returning_id(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            text,
            self.reply_to.as_deref(),
        )
        .await
        {
            Ok(mid) => mid,
            Err(err) => {
                warn!("[discord-stream] send failed: {err}");
                None
            }
        }
    }

    async fn edit_message(&self, message_id: &str, text: &str) {
        let _ = savfox_channels::discord::client::edit_message(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            message_id,
            text,
        )
        .await;
    }

    async fn delete_message(&self, message_id: &str) {
        let _ = savfox_channels::discord::client::delete_message(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            message_id,
        )
        .await;
    }

    fn max_length(&self) -> usize {
        2000
    }

    fn throttle_interval(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn platform_tag(&self) -> &'static str {
        "discord"
    }
}

/// Slack streaming sink.
pub(crate) struct SlackSink {
    client: reqwest::Client,
    bot_token: String,
    channel_id: String,
    thread_ts: Option<String>,
}

impl SlackSink {
    pub fn new(
        client: reqwest::Client,
        bot_token: String,
        channel_id: String,
        thread_ts: Option<String>,
    ) -> Self {
        Self {
            client,
            bot_token,
            channel_id,
            thread_ts,
        }
    }
}

#[async_trait]
impl StreamSink for SlackSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        match savfox_channels::slack::client::send_message_returning_id(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            text,
            self.thread_ts.as_deref(),
        )
        .await
        {
            Ok(ts) => ts,
            Err(err) => {
                warn!("[slack-stream] send failed: {err}");
                None
            }
        }
    }

    async fn edit_message(&self, ts: &str, text: &str) {
        let _ = savfox_channels::slack::client::edit_message(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            ts,
            text,
        )
        .await;
    }

    async fn delete_message(&self, ts: &str) {
        let _ = savfox_channels::slack::client::delete_message(
            &self.client,
            &self.bot_token,
            &self.channel_id,
            ts,
        )
        .await;
    }

    fn max_length(&self) -> usize {
        40000
    }

    fn throttle_interval(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn platform_tag(&self) -> &'static str {
        "slack"
    }
}

/// Mattermost streaming sink.
pub(crate) struct MattermostSink {
    client: reqwest::Client,
    server_url: String,
    access_token: String,
    channel_id: String,
    root_id: Option<String>,
}

impl MattermostSink {
    pub fn new(
        client: reqwest::Client,
        server_url: String,
        access_token: String,
        channel_id: String,
        root_id: Option<String>,
    ) -> Self {
        Self {
            client,
            server_url,
            access_token,
            channel_id,
            root_id,
        }
    }
}

#[async_trait]
impl StreamSink for MattermostSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        match savfox_channels::mattermost::client::send_message_returning_id(
            &self.client,
            &self.server_url,
            &self.access_token,
            &self.channel_id,
            text,
            self.root_id.as_deref(),
        )
        .await
        {
            Ok(pid) => pid,
            Err(err) => {
                warn!("[mattermost-stream] send failed: {err}");
                None
            }
        }
    }

    async fn edit_message(&self, post_id: &str, text: &str) {
        let _ = savfox_channels::mattermost::client::edit_message(
            &self.client,
            &self.server_url,
            &self.access_token,
            post_id,
            text,
        )
        .await;
    }

    async fn delete_message(&self, post_id: &str) {
        let _ = savfox_channels::mattermost::client::delete_message(
            &self.client,
            &self.server_url,
            &self.access_token,
            post_id,
        )
        .await;
    }

    fn max_length(&self) -> usize {
        16383
    }

    fn throttle_interval(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn platform_tag(&self) -> &'static str {
        "mattermost"
    }
}

/// Feishu/Lark streaming sink.
pub(crate) struct FeishuSink {
    client: reqwest::Client,
    base_url: String,
    tenant_token: String,
    receive_id: String,
    receive_id_type: String,
}

impl FeishuSink {
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        tenant_token: String,
        receive_id: String,
        receive_id_type: String,
    ) -> Self {
        Self {
            client,
            base_url,
            tenant_token,
            receive_id,
            receive_id_type,
        }
    }
}

#[async_trait]
impl StreamSink for FeishuSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        match savfox_channels::feishu::client::send_message_returning_id(
            &self.client,
            &self.base_url,
            &self.tenant_token,
            &self.receive_id,
            &self.receive_id_type,
            text,
        )
        .await
        {
            Ok(mid) => mid,
            Err(err) => {
                warn!("[feishu-stream] send failed: {err}");
                None
            }
        }
    }

    async fn edit_message(&self, message_id: &str, text: &str) {
        let _ = savfox_channels::feishu::client::edit_message(
            &self.client,
            &self.base_url,
            &self.tenant_token,
            message_id,
            text,
        )
        .await;
    }

    async fn delete_message(&self, message_id: &str) {
        let _ = savfox_channels::feishu::client::delete_message(
            &self.client,
            &self.base_url,
            &self.tenant_token,
            message_id,
        )
        .await;
    }

    fn max_length(&self) -> usize {
        // Feishu text messages have no strict limit, but keep a reasonable cap.
        30000
    }

    fn throttle_interval(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn platform_tag(&self) -> &'static str {
        "feishu"
    }
}

/// DingTalk streaming sink.
///
/// DingTalk does not support editing text messages after sending, so
/// `edit_message` is implemented as "recall old message + send new message".
pub(crate) struct DingtalkSink {
    client: reqwest::Client,
    openapi_host: String,
    access_token: String,
    robot_code: String,
    target: DingtalkTarget,
    /// Track the current processQueryKey so we can recall-and-resend.
    current_key: tokio::sync::Mutex<Option<String>>,
}

pub(crate) enum DingtalkTarget {
    Dm { user_id: String },
    Group { conversation_id: String },
}

impl DingtalkSink {
    pub fn new(
        client: reqwest::Client,
        openapi_host: String,
        access_token: String,
        robot_code: String,
        target: DingtalkTarget,
    ) -> Self {
        Self {
            client,
            openapi_host,
            access_token,
            robot_code,
            target,
            current_key: tokio::sync::Mutex::new(None),
        }
    }

    async fn send_text(&self, text: &str) -> Option<String> {
        let result = match &self.target {
            DingtalkTarget::Dm { user_id } => {
                savfox_channels::dingtalk::client::send_dm_message_returning_id(
                    &self.client,
                    &self.openapi_host,
                    &self.access_token,
                    &self.robot_code,
                    user_id,
                    text,
                )
                .await
            }
            DingtalkTarget::Group { conversation_id } => {
                savfox_channels::dingtalk::client::send_group_message_returning_id(
                    &self.client,
                    &self.openapi_host,
                    &self.access_token,
                    &self.robot_code,
                    conversation_id,
                    text,
                )
                .await
            }
        };
        match result {
            Ok(key) => key,
            Err(err) => {
                warn!("[dingtalk-stream] send failed: {err}");
                None
            }
        }
    }

    async fn recall(&self, process_query_key: &str) {
        let result = match &self.target {
            DingtalkTarget::Dm { .. } => {
                savfox_channels::dingtalk::client::recall_dm_message(
                    &self.client,
                    &self.openapi_host,
                    &self.access_token,
                    &self.robot_code,
                    process_query_key,
                )
                .await
            }
            DingtalkTarget::Group { conversation_id } => {
                savfox_channels::dingtalk::client::recall_group_message(
                    &self.client,
                    &self.openapi_host,
                    &self.access_token,
                    &self.robot_code,
                    conversation_id,
                    process_query_key,
                )
                .await
            }
        };
        if let Err(err) = result {
            warn!("[dingtalk-stream] recall failed: {err}");
        }
    }
}

#[async_trait]
impl StreamSink for DingtalkSink {
    async fn send_initial(&self, text: &str) -> Option<String> {
        let key = self.send_text(text).await?;
        *self.current_key.lock().await = Some(key.clone());
        Some(key)
    }

    async fn edit_message(&self, _message_id: &str, text: &str) {
        // DingTalk doesn't support editing text messages.
        // Recall the current message and send a replacement.
        let mut current = self.current_key.lock().await;
        if let Some(old_key) = current.take() {
            drop(current);
            self.recall(&old_key).await;
        } else {
            drop(current);
        }
        if let Some(new_key) = self.send_text(text).await {
            *self.current_key.lock().await = Some(new_key);
        }
    }

    async fn delete_message(&self, message_id: &str) {
        self.recall(message_id).await;
        // Clear current_key if it matches the deleted message.
        let mut current = self.current_key.lock().await;
        if current.as_deref() == Some(message_id) {
            *current = None;
        }
    }

    fn max_length(&self) -> usize {
        20000
    }

    fn throttle_interval(&self) -> Duration {
        // Longer throttle to avoid rate limits: each "edit" = recall + send = 2 API calls.
        Duration::from_millis(5000)
    }

    fn platform_tag(&self) -> &'static str {
        "dingtalk"
    }
}

// ── Sink factory ─────────────────────────────────────────────────────

/// Extra context for sink creation that some platforms need beyond the
/// basic `channel_id` and `reply_target`.
pub(crate) struct StreamSinkContext<'a> {
    pub peer_id: Option<&'a str>,
    pub thread_id: Option<&'a str>,
    pub chat_type: Option<&'a str>,
}

/// Create a `StreamSink` for the given platform, resolving tokens as needed.
/// Returns `None` if the platform doesn't support editing or tokens are missing.
pub(crate) async fn create_stream_sink(
    platform: &str,
    channel_id: &str,
    reply_target: Option<&str>,
    gw: &Arc<GatewayChannel>,
    ctx: Option<&StreamSinkContext<'_>>,
) -> Option<Box<dyn StreamSink>> {
    let client = gw.http_client().clone();

    match platform {
        "telegram" => {
            let token = gw.resolve_telegram_bot_token().await?;
            Some(Box::new(TelegramSink::new(
                client,
                token,
                channel_id.to_owned(),
                reply_target.map(ToOwned::to_owned),
            )))
        }
        "discord" => {
            let token = gw.resolve_discord_bot_token().await?;
            Some(Box::new(DiscordSink::new(
                client,
                token,
                channel_id.to_owned(),
                reply_target.map(ToOwned::to_owned),
            )))
        }
        "slack" => {
            let token = gw.resolve_slack_bot_token().await?;
            Some(Box::new(SlackSink::new(
                client,
                token,
                channel_id.to_owned(),
                reply_target.map(ToOwned::to_owned),
            )))
        }
        "mattermost" => {
            let (server, token) = gw.resolve_mattermost_config().await?;
            Some(Box::new(MattermostSink::new(
                client,
                server,
                token,
                channel_id.to_owned(),
                reply_target.map(ToOwned::to_owned),
            )))
        }
        "feishu" | "lark" => {
            // Feishu requires tenant token which may need runtime fetching.
            // Try to resolve from saved configs.
            let config =
                crate::channels::feishu::resolve_feishu_outbound_config(&gw.config().savfox_home)
                    .await
                    .ok()?;
            let cfg = config?;

            // Try direct tenant token first, then fetch via app_id/app_secret.
            let tenant_token = if let Some(t) = cfg
                .app_access_token
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                t.to_owned()
            } else if let (Some(app_id), Some(app_secret)) = (
                cfg.app_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty()),
                cfg.app_secret
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty()),
            ) {
                crate::channels::feishu::fetch_feishu_tenant_access_token(
                    &client,
                    &cfg.base_url,
                    app_id,
                    app_secret,
                )
                .await
                .ok()?
            } else {
                std::env::var("FEISHU_TENANT_ACCESS_TOKEN")
                    .or_else(|_| std::env::var("FEISHU_APP_ACCESS_TOKEN"))
                    .ok()?
            };

            let receive_id_type = savfox_channels::feishu::client::infer_receive_id_type(
                channel_id,
                &cfg.receive_id_type,
            );
            Some(Box::new(FeishuSink::new(
                client,
                cfg.base_url.clone(),
                tenant_token,
                channel_id.to_owned(),
                receive_id_type,
            )))
        }
        "dingtalk" => {
            // DingTalk uses the Open API for send/recall. Requires client_id/client_secret
            // from the channel config to obtain an access token.
            let config =
                savfox_channels::dingtalk::load_dingtalk_channel_config(&gw.config().savfox_home)
                    .await
                    .ok()?;
            let cfg = config?;
            let client_id = cfg
                .client_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())?;
            let client_secret = cfg
                .client_secret
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())?;

            let access_token = savfox_channels::dingtalk::client::fetch_access_token(
                &client,
                &cfg.openapi_host,
                client_id,
                client_secret,
            )
            .await
            .ok()?;

            // Determine target: DM (need user_id) or group (need conversation_id).
            let chat_type = ctx.and_then(|c| c.chat_type);
            let target = if let Some("dm" | "private" | "direct" | "single") = chat_type {
                let user_id = ctx
                    .and_then(|c| c.peer_id)
                    .filter(|v| !v.trim().is_empty())?;
                DingtalkTarget::Dm {
                    user_id: user_id.to_owned(),
                }
            } else {
                // Group chat or unknown — use conversation_id from thread_id.
                let conversation_id = ctx
                    .and_then(|c| c.thread_id)
                    .filter(|v| !v.trim().is_empty())?;
                DingtalkTarget::Group {
                    conversation_id: conversation_id.to_owned(),
                }
            };

            Some(Box::new(DingtalkSink::new(
                client,
                cfg.openapi_host.clone(),
                access_token,
                client_id.to_owned(),
                target,
            )))
        }
        _ => None,
    }
}

/// Send an initial "⏳" status message on the given sink.
/// Returns the message ID that should be passed to `ChannelStreamWriter::new`.
pub(crate) async fn send_status_message(sink: &dyn StreamSink) -> Option<String> {
    sink.send_initial("\u{23f3}").await
}
