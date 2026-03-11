use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use form_urlencoded::Serializer;
use matrix_bot_sdk::client::{MatrixAuth, MatrixClient};
use reqwest::Method;
use salvo::http::StatusCode;
use salvo::prelude::*;
use savfox_channels::matrix::{
    MatrixChannelConfig as MatrixPlatformConfig, MatrixCommandEvent, parse_inbound_payload_for_user,
};
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::{obtain_channel_and_store, parse_json_body, render_error, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

#[derive(Debug, Clone, Default)]
pub(crate) struct MatrixRuntimeState {
    pub mode: Option<String>,
    pub homeserver: Option<String>,
    pub user_id: Option<String>,
    pub access_token: Option<String>,
    pub connected: bool,
    pub room_count: Option<u32>,
    pub appservice_url: Option<String>,
    pub sender_localpart: Option<String>,
    pub user_prefix: Option<String>,
    pub server_name: Option<String>,
    pub config_id: Option<String>,
    pub registration: Option<Value>,
    pub last_error: Option<String>,
}

fn matrix_runtime_state_store() -> &'static Mutex<HashMap<String, MatrixRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, MatrixRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn matrix_appservice_store() -> &'static Mutex<HashMap<String, MatrixAppserviceChannel>> {
    static STORE: OnceLock<Mutex<HashMap<String, MatrixAppserviceChannel>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[allow(clippy::print_stdout)]
fn debug_matrix_appservice(message: impl AsRef<str>) {
    println!("[matrix][appservice] {}", message.as_ref());
}

fn appservice_token_state(token: Option<&str>) -> &'static str {
    if token.is_some_and(|value| !value.trim().is_empty()) {
        "present"
    } else {
        "missing"
    }
}

fn matrix_event_count(body: &Value) -> usize {
    body.get("events")
        .and_then(Value::as_array)
        .map(|events| events.len())
        .unwrap_or(0)
}

fn matrix_event_preview(body: &Value) -> String {
    let Some(events) = body.get("events").and_then(Value::as_array) else {
        return "none".to_string();
    };

    let mut parts = Vec::new();
    for event in events.iter().take(6) {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let sender = event
            .get("sender")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let room_id = event
            .get("room_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let mut part = event_type.to_string();
        if let Some(sender) = sender {
            part.push_str(&format!(" sender={sender}"));
        }
        if let Some(room_id) = room_id {
            part.push_str(&format!(" room={room_id}"));
        }
        parts.push(part);
    }

    if events.len() > 6 {
        parts.push(format!("... +{} more", events.len() - 6));
    }

    parts.join(" | ")
}

pub(crate) fn matrix_runtime_state_snapshot() -> HashMap<String, MatrixRuntimeState> {
    matrix_runtime_state_store()
        .lock()
        .map(|store| store.clone())
        .unwrap_or_default()
}

pub(crate) fn matrix_runtime_state_for(config_id: &str) -> Option<MatrixRuntimeState> {
    matrix_runtime_state_store()
        .lock()
        .ok()
        .and_then(|store| store.get(config_id).cloned())
}

fn set_matrix_runtime_state(config_id: &str, state: MatrixRuntimeState) {
    if let Ok(mut store) = matrix_runtime_state_store().lock() {
        store.insert(config_id.to_string(), state);
    }
}

fn update_matrix_runtime_state(config_id: &str, apply: impl FnOnce(&mut MatrixRuntimeState)) {
    if let Ok(mut store) = matrix_runtime_state_store().lock() {
        apply(store.entry(config_id.to_string()).or_default());
    }
}

pub(crate) fn remove_matrix_runtime_state(config_id: &str) {
    if let Ok(mut store) = matrix_runtime_state_store().lock() {
        store.remove(config_id);
    }
}

pub(crate) fn matrix_appservice_channel_for(config_id: &str) -> Option<MatrixAppserviceChannel> {
    matrix_appservice_store()
        .lock()
        .ok()
        .and_then(|store| store.get(config_id).cloned())
}

fn set_matrix_appservice_channel(config_id: &str, channel: MatrixAppserviceChannel) {
    if let Ok(mut store) = matrix_appservice_store().lock() {
        store.insert(config_id.to_string(), channel);
    }
}

pub(crate) fn remove_matrix_appservice_channel(config_id: &str) {
    if let Ok(mut store) = matrix_appservice_store().lock() {
        store.remove(config_id);
    }
    remove_matrix_runtime_state(config_id);
}

struct MatrixSyncTask {
    config_id: String,
    homeserver: String,
    user_id: String,
    client: MatrixClient,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
}

pub(crate) struct MatrixChannel {
    config_id: String,
    homeserver: String,
    user_id: Option<String>,
    access_token: Option<String>,
    password: Option<String>,
    device_name: Option<String>,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    client: Mutex<Option<MatrixClient>>,
    sync_task: Mutex<Option<JoinHandle<()>>>,
}

impl MatrixChannel {
    pub(crate) fn new(
        config: MatrixPlatformConfig,
        gateway_channel: Arc<GatewayChannel>,
        session_store: Arc<SessionStore>,
    ) -> Self {
        let MatrixPlatformConfig {
            id: config_id,
            mode: _,
            homeserver,
            access_token,
            password,
            device_name,
            user_id,
            rooms: _,
            server_name: _,
            public_url: _,
            appservice_id: _,
            appservice_token: _,
            homeserver_token: _,
            sender_localpart: _,
            user_prefix: _,
            alias_prefix: _,
        } = config;

        Self {
            config_id,
            homeserver,
            user_id,
            access_token,
            password,
            device_name,
            gateway_channel,
            session_store,
            client: Mutex::new(None),
            sync_task: Mutex::new(None),
        }
    }
}

impl Drop for MatrixChannel {
    fn drop(&mut self) {
        if let Ok(mut task) = self.sync_task.lock()
            && let Some(handle) = task.take()
        {
            handle.abort();
        }
        if let Ok(mut client) = self.client.lock()
            && let Some(client) = client.take()
        {
            client.stop();
        }
        remove_matrix_runtime_state(&self.config_id);
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        if self
            .sync_task
            .lock()
            .map(|task| task.is_some())
            .unwrap_or(false)
        {
            return Ok(());
        }

        let resolved = GatewayChannel::resolve_matrix_client(
            &self.homeserver,
            self.access_token.as_deref(),
            self.user_id.as_deref(),
            self.password.as_deref(),
            self.device_name.as_deref(),
        )
        .await
        .with_context(|| format!("failed to authenticate Matrix channel '{}'", self.config_id))?;

        let initial_payload = matrix_sync_request(&resolved.client, None, 0)
            .await
            .with_context(|| {
                format!(
                    "failed to perform initial Matrix sync for channel '{}'",
                    self.config_id
                )
            })?;
        let initial_batch =
            parse_inbound_payload_for_user(&initial_payload, Some(&resolved.user_id));
        let next_batch = initial_payload
            .get("next_batch")
            .and_then(Value::as_str)
            .map(ToString::to_string);

        handle_matrix_invites(
            &MatrixSyncTask {
                config_id: self.config_id.clone(),
                homeserver: self.homeserver.clone(),
                user_id: resolved.user_id.clone(),
                client: resolved.client.clone(),
                gateway_channel: Arc::clone(&self.gateway_channel),
                session_store: Arc::clone(&self.session_store),
            },
            initial_batch.rooms_to_auto_join,
        )
        .await;

        let room_count = resolved
            .client
            .get_joined_rooms()
            .await
            .ok()
            .map(|rooms| rooms.len() as u32);
        set_matrix_runtime_state(
            &self.config_id,
            MatrixRuntimeState {
                mode: Some("user".to_string()),
                homeserver: Some(self.homeserver.clone()),
                user_id: Some(resolved.user_id.clone()),
                access_token: Some(resolved.access_token.clone()),
                connected: true,
                room_count,
                appservice_url: None,
                sender_localpart: None,
                user_prefix: None,
                server_name: None,
                config_id: Some(self.config_id.clone()),
                registration: None,
                last_error: None,
            },
        );

        if let Ok(mut client) = self.client.lock() {
            *client = Some(resolved.client.clone());
        }

        let task = MatrixSyncTask {
            config_id: self.config_id.clone(),
            homeserver: self.homeserver.clone(),
            user_id: resolved.user_id,
            client: resolved.client,
            gateway_channel: Arc::clone(&self.gateway_channel),
            session_store: Arc::clone(&self.session_store),
        };
        let handle = tokio::spawn(run_matrix_sync_task(task, next_batch));
        if let Ok(mut sync_task) = self.sync_task.lock() {
            *sync_task = Some(handle);
        }

        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let client = self
            .client
            .lock()
            .ok()
            .and_then(|client| client.clone())
            .context("Matrix client is not started")?;
        client
            .send_text(channel, message)
            .await
            .with_context(|| format!("failed to send Matrix message to room {channel}"))?;
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let client = self
            .client
            .lock()
            .ok()
            .and_then(|client| client.clone())
            .context("Matrix client is not started")?;
        client
            .send_html_text(
                channel,
                &render_matrix_rich_text(&msg),
                &render_matrix_rich_html(&msg),
            )
            .await
            .with_context(|| format!("failed to send Matrix rich message to room {channel}"))?;
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        let parsed = parse_inbound_payload_for_user(&payload, None);
        Ok(parsed
            .commands
            .first()
            .map(|command| ChannelAction::StartThread {
                channel: command.room_id.clone(),
                prompt: command.prompt.clone(),
            })
            .unwrap_or(ChannelAction::Ignore))
    }
}

#[derive(Clone)]
pub(crate) struct MatrixAppserviceChannel {
    inner: Arc<MatrixAppserviceInner>,
}

struct MatrixAppserviceInner {
    config_id: String,
    homeserver: String,
    server_name: String,
    public_url: String,
    appservice_id: String,
    appservice_token: String,
    homeserver_token: String,
    sender_localpart: String,
    user_prefix: String,
    alias_prefix: String,
    bot_user_id: String,
    client: MatrixClient,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
}

impl MatrixAppserviceChannel {
    pub(crate) fn new(
        config: MatrixPlatformConfig,
        gateway_channel: Arc<GatewayChannel>,
        session_store: Arc<SessionStore>,
    ) -> anyhow::Result<Self> {
        let config_id = config.id.clone();
        let config_homeserver = config.homeserver.clone();
        let server_name = config
            .server_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires serverName")?
            .to_string();
        let public_url = config
            .public_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires publicUrl")?
            .to_string();
        let appservice_id = config
            .appservice_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires appserviceId")?
            .to_string();
        let appservice_token = config
            .appservice_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires appserviceToken")?
            .to_string();
        let homeserver_token = config
            .homeserver_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires homeserverToken")?
            .to_string();
        let sender_localpart = config
            .sender_localpart
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires senderLocalpart")?
            .to_string();
        let homeserver_url = url::Url::parse(&config.homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {}", config.homeserver))?;
        let bot_user_id = format!("@{}:{}", sender_localpart, server_name);
        let auth = MatrixAuth::new(appservice_token.clone()).with_user_id(bot_user_id.clone());
        let client = MatrixClient::new(homeserver_url, auth);

        debug_matrix_appservice(format!(
            "config='{}' constructed homeserver='{}' public_url='{}' bot_user_id='{}' user_prefix='{}' alias_prefix='{}' tokens(as={}, hs={})",
            config_id,
            config_homeserver,
            public_url,
            bot_user_id,
            config.user_prefix,
            config.alias_prefix,
            appservice_token_state(Some(&appservice_token)),
            appservice_token_state(Some(&homeserver_token)),
        ));

        Ok(Self {
            inner: Arc::new(MatrixAppserviceInner {
                config_id: config.id,
                homeserver: config.homeserver,
                server_name,
                public_url,
                appservice_id,
                appservice_token,
                homeserver_token,
                sender_localpart,
                user_prefix: config.user_prefix,
                alias_prefix: config.alias_prefix,
                bot_user_id,
                client,
                gateway_channel,
                session_store,
            }),
        })
    }

    pub(crate) fn config_id(&self) -> &str {
        &self.inner.config_id
    }

    pub(crate) fn homeserver_token(&self) -> &str {
        &self.inner.homeserver_token
    }

    pub(crate) fn appservice_url(&self) -> String {
        self.inner.public_url.trim_end_matches('/').to_string()
    }

    pub(crate) fn user_prefix(&self) -> &str {
        &self.inner.user_prefix
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.inner.server_name
    }

    /// Generate the Matrix user ID for a given agent slug.
    pub(crate) fn agent_user_id(&self, agent_slug: &str) -> String {
        format!(
            "@{}{}:{}",
            self.inner.user_prefix, agent_slug, self.inner.server_name
        )
    }

    pub(crate) fn registration_preview(&self) -> Value {
        let escaped_server_name = regex::escape(&self.inner.server_name);
        let escaped_user_prefix = regex::escape(&self.inner.user_prefix);
        let escaped_alias_prefix = regex::escape(&self.inner.alias_prefix);
        let escaped_bot = regex::escape(&self.inner.sender_localpart);

        json!({
            "id": self.inner.appservice_id,
            "url": self.appservice_url(),
            "as_token": self.inner.appservice_token,
            "hs_token": self.inner.homeserver_token,
            "sender_localpart": self.inner.sender_localpart,
            "rate_limited": false,
            "namespaces": {
                "users": [
                    {
                        "exclusive": true,
                        "regex": format!("@{}:{}$", escaped_bot, escaped_server_name),
                    },
                    {
                        "exclusive": true,
                        "regex": format!("@{}.*:{}$", escaped_user_prefix, escaped_server_name),
                    }
                ],
                "aliases": [
                    {
                        "exclusive": true,
                        "regex": format!("#{}.*:{}$", escaped_alias_prefix, escaped_server_name),
                    }
                ],
                "rooms": []
            }
        })
    }

    fn matches_user_id(&self, user_id: &str) -> bool {
        if user_id.eq_ignore_ascii_case(&self.inner.bot_user_id) {
            return true;
        }
        matrix_localpart(user_id)
            .map(|localpart| localpart.starts_with(&self.inner.user_prefix))
            .unwrap_or(false)
            && user_id.ends_with(&format!(":{}", self.inner.server_name))
    }

    fn matches_room_alias(&self, room_alias: &str) -> bool {
        matrix_alias_localpart(room_alias)
            .map(|localpart| localpart.starts_with(&self.inner.alias_prefix))
            .unwrap_or(false)
            && room_alias.ends_with(&format!(":{}", self.inner.server_name))
    }

    fn should_ignore_sender(&self, sender: &str) -> bool {
        sender.eq_ignore_ascii_case(&self.inner.bot_user_id)
            || (matrix_localpart(sender)
                .map(|localpart| localpart.starts_with(&self.inner.user_prefix))
                .unwrap_or(false)
                && sender.ends_with(&format!(":{}", self.inner.server_name)))
    }

    async fn refresh_room_count(&self) {
        if let Ok(joined_rooms) = self.inner.client.get_joined_rooms().await {
            update_matrix_runtime_state(&self.inner.config_id, |state| {
                state.room_count = Some(joined_rooms.len() as u32);
            });
        }
    }

    pub(crate) async fn join_room(&self, room_id: &str) -> anyhow::Result<()> {
        self.inner
            .client
            .join_room(room_id)
            .await
            .with_context(|| format!("failed to auto-join Matrix room {room_id}"))?;
        self.refresh_room_count().await;
        Ok(())
    }

    async fn handle_transaction(&self, txn_id: &str, body: &Value) -> anyhow::Result<()> {
        debug_matrix_appservice(format!(
            "config='{}' txn='{}' received event_count={} preview=[{}]",
            self.inner.config_id,
            txn_id,
            matrix_event_count(body),
            matrix_event_preview(body),
        ));
        let parsed = parse_inbound_payload_for_user(body, Some(&self.inner.bot_user_id));
        update_matrix_runtime_state(&self.inner.config_id, |state| {
            state.connected = true;
            state.last_error = None;
        });
        debug_matrix_appservice(format!(
            "config='{}' txn='{}' parsed commands={} invites={}",
            self.inner.config_id,
            txn_id,
            parsed.commands.len(),
            parsed.rooms_to_auto_join.len(),
        ));

        let mut joined_rooms = 0_u32;
        let mut skipped_invites = 0_u32;

        for (room_id, invited_user_id) in parsed.rooms_to_auto_join {
            if let Some(invited_user_id) = invited_user_id
                .as_deref()
                .and_then(|value| non_empty_trimmed(Some(value)))
                && !self.matches_user_id(invited_user_id)
            {
                skipped_invites = skipped_invites.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' skipping invite room='{}' invited_user='{}' reason='invite_not_for_bot'",
                    self.inner.config_id, txn_id, room_id, invited_user_id,
                ));
                continue;
            }
            self.join_room(&room_id).await?;
            joined_rooms = joined_rooms.saturating_add(1);
            debug_matrix_appservice(format!(
                "config='{}' txn='{}' joined room='{}'",
                self.inner.config_id, txn_id, room_id
            ));
        }

        let mut ignored_senders = 0_u32;
        let mut ignored_duplicates = 0_u32;
        let mut dispatched_commands = 0_u32;
        for command in parsed.commands {
            if self.should_ignore_sender(&command.sender) {
                ignored_senders = ignored_senders.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' ignoring command room='{}' sender='{}' reason='sender_in_appservice_namespace'",
                    self.inner.config_id, txn_id, command.room_id, command.sender,
                ));
                continue;
            }
            if runtime::should_drop_duplicate(command.dedupe_key.clone()).await {
                ignored_duplicates = ignored_duplicates.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' ignoring command room='{}' sender='{}' reason='duplicate'",
                    self.inner.config_id, txn_id, command.room_id, command.sender,
                ));
                continue;
            }

            let gateway_channel = Arc::clone(&self.inner.gateway_channel);
            let session_store = Arc::clone(&self.inner.session_store);
            let config_id = self.inner.config_id.clone();
            debug_matrix_appservice(format!(
                "config='{}' txn='{}' dispatching command room='{}' sender='{}'",
                self.inner.config_id, txn_id, command.room_id, command.sender,
            ));
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "matrix",
                    command.room_id.clone(),
                    command.prompt,
                    Some(command.sender.clone()),
                    Some(runtime::StartThreadMeta {
                        peer_id: Some(command.sender),
                        group_id: Some(command.room_id),
                        chat_type: Some("group".to_string()),
                        saved_channel_config_id: Some(config_id),
                        ..runtime::StartThreadMeta::default()
                    }),
                )
                .await;
            });
            dispatched_commands = dispatched_commands.saturating_add(1);
        }

        debug_matrix_appservice(format!(
            "config='{}' txn='{}' complete joined_rooms={} skipped_invites={} dispatched={} ignored_sender={} ignored_duplicate={}",
            self.inner.config_id,
            txn_id,
            joined_rooms,
            skipped_invites,
            dispatched_commands,
            ignored_senders,
            ignored_duplicates,
        ));
        info!(
            config_id = %self.inner.config_id,
            txn_id,
            "Matrix appservice transaction handled"
        );
        Ok(())
    }

    async fn start_inner(&self) -> anyhow::Result<()> {
        debug_matrix_appservice(format!(
            "config='{}' starting bot_user_id='{}' public_url='{}'",
            self.inner.config_id,
            self.inner.bot_user_id,
            self.appservice_url(),
        ));
        set_matrix_appservice_channel(&self.inner.config_id, self.clone());
        let room_count = self
            .inner
            .client
            .get_joined_rooms()
            .await
            .ok()
            .map(|rooms| rooms.len() as u32);
        set_matrix_runtime_state(
            &self.inner.config_id,
            MatrixRuntimeState {
                mode: Some("appservice".to_string()),
                homeserver: Some(self.inner.homeserver.clone()),
                user_id: Some(self.inner.bot_user_id.clone()),
                access_token: None,
                connected: true,
                room_count,
                appservice_url: Some(self.appservice_url()),
                sender_localpart: Some(self.inner.sender_localpart.clone()),
                user_prefix: Some(self.inner.user_prefix.clone()),
                server_name: Some(self.inner.server_name.clone()),
                config_id: Some(self.inner.config_id.clone()),
                registration: Some(self.registration_preview()),
                last_error: None,
            },
        );
        debug_matrix_appservice(format!(
            "config='{}' started connected=true room_count={} registration_url='{}'",
            self.inner.config_id,
            room_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            self.appservice_url(),
        ));
        Ok(())
    }
}

#[async_trait]
impl Channel for MatrixAppserviceChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        self.start_inner().await
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        self.inner
            .client
            .send_text(channel, message)
            .await
            .with_context(|| format!("failed to send Matrix appservice message to room {channel}"))
            .map(|_| ())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        self.inner
            .client
            .send_html_text(
                channel,
                &render_matrix_rich_text(&msg),
                &render_matrix_rich_html(&msg),
            )
            .await
            .with_context(|| {
                format!("failed to send Matrix appservice rich message to room {channel}")
            })
            .map(|_| ())
    }

    async fn handle_webhook(&self, _payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(ChannelAction::Ignore)
    }
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn matrix_localpart(user_id: &str) -> Option<&str> {
    let trimmed = user_id.trim();
    let without_at = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let localpart = without_at.split(':').next()?.trim();
    if localpart.is_empty() {
        None
    } else {
        Some(localpart)
    }
}

fn matrix_alias_localpart(alias: &str) -> Option<&str> {
    let trimmed = alias.trim();
    let without_hash = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let localpart = without_hash.split(':').next()?.trim();
    if localpart.is_empty() {
        None
    } else {
        Some(localpart)
    }
}

fn render_matrix_rich_text(msg: &RichMessage) -> String {
    let mut text = msg.text.clone();
    for block in &msg.code_blocks {
        text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
    }
    text
}

fn render_matrix_rich_html(msg: &RichMessage) -> String {
    let mut html = msg.text.replace('\n', "<br/>");
    for block in &msg.code_blocks {
        html.push_str(&format!(
            "<br/><pre><code class=\"language-{}\">{}</code></pre>",
            block.language, block.content
        ));
    }
    html
}

fn build_sync_endpoint(since: Option<&str>, timeout_ms: u64) -> String {
    let mut query = Serializer::new(String::new());
    query.append_pair("timeout", &timeout_ms.to_string());
    if let Some(since) = non_empty_trimmed(since) {
        query.append_pair("since", since);
    }
    format!("/_matrix/client/v3/sync?{}", query.finish())
}

async fn matrix_sync_request(
    client: &MatrixClient,
    since: Option<&str>,
    timeout_ms: u64,
) -> anyhow::Result<Value> {
    let endpoint = build_sync_endpoint(since, timeout_ms);
    client
        .raw_json(Method::GET, &endpoint, None)
        .await
        .with_context(|| format!("Matrix sync request failed: {endpoint}"))
}

async fn handle_matrix_invites(task: &MatrixSyncTask, invites: Vec<(String, Option<String>)>) {
    for (room_id, invited_user_id) in invites {
        if let Some(invited_user_id) = invited_user_id
            .as_deref()
            .and_then(|value| non_empty_trimmed(Some(value)))
            && !invited_user_id.eq_ignore_ascii_case(&task.user_id)
        {
            continue;
        }

        match task.client.join_room(&room_id).await {
            Ok(_) => {
                if let Ok(joined_rooms) = task.client.get_joined_rooms().await {
                    update_matrix_runtime_state(&task.config_id, |state| {
                        state.room_count = Some(joined_rooms.len() as u32);
                    });
                }
            }
            Err(err) => {
                update_matrix_runtime_state(&task.config_id, |state| {
                    state.last_error = Some(err.to_string());
                });
                warn!(
                    config_id = %task.config_id,
                    room_id,
                    error = %err,
                    "Matrix invite auto-join failed"
                );
            }
        }
    }
}

async fn dispatch_matrix_commands(task: &MatrixSyncTask, commands: Vec<MatrixCommandEvent>) {
    for command in commands {
        if command.sender.eq_ignore_ascii_case(&task.user_id) {
            continue;
        }
        if runtime::should_drop_duplicate(command.dedupe_key.clone()).await {
            continue;
        }

        let gateway_channel = Arc::clone(&task.gateway_channel);
        let session_store = Arc::clone(&task.session_store);
        let config_id = task.config_id.clone();
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "matrix",
                command.room_id.clone(),
                command.prompt,
                Some(command.sender.clone()),
                Some(runtime::StartThreadMeta {
                    peer_id: Some(command.sender),
                    group_id: Some(command.room_id),
                    chat_type: Some("group".to_string()),
                    saved_channel_config_id: Some(config_id),
                    ..runtime::StartThreadMeta::default()
                }),
            )
            .await;
        });
    }
}

async fn run_matrix_sync_task(task: MatrixSyncTask, mut since: Option<String>) {
    let mut backoff = Duration::from_secs(1);

    loop {
        match matrix_sync_request(&task.client, since.as_deref(), 30_000).await {
            Ok(payload) => {
                runtime::record_channel_probe("matrix", "ok").await;
                if let Some(next_batch) = payload
                    .get("next_batch")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                {
                    since = Some(next_batch);
                }

                let parsed = parse_inbound_payload_for_user(&payload, Some(&task.user_id));
                update_matrix_runtime_state(&task.config_id, |state| {
                    state.connected = true;
                    state.last_error = None;
                });

                handle_matrix_invites(&task, parsed.rooms_to_auto_join).await;
                dispatch_matrix_commands(&task, parsed.commands).await;
                backoff = Duration::from_secs(1);
            }
            Err(err) => {
                runtime::record_channel_probe("matrix", "error").await;
                update_matrix_runtime_state(&task.config_id, |state| {
                    state.connected = false;
                    state.last_error = Some(err.to_string());
                });
                warn!(
                    config_id = %task.config_id,
                    homeserver = %task.homeserver,
                    error = %err,
                    "Matrix sync loop failed"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn extract_first_room_id(payload: &Value) -> Option<&str> {
    if let Some(room_id) = payload
        .get("room_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|room_id| !room_id.is_empty())
    {
        return Some(room_id);
    }

    for section in ["join", "invite", "leave"] {
        if let Some(rooms) = payload
            .get("rooms")
            .and_then(|rooms| rooms.get(section))
            .and_then(Value::as_object)
            && let Some((room_id, _)) = rooms.iter().next()
        {
            let room_id = room_id.trim();
            if !room_id.is_empty() {
                return Some(room_id);
            }
        }
    }

    payload
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|event| {
            event
                .get("room_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|room_id| !room_id.is_empty())
        })
}

fn appservice_access_token(req: &Request) -> Option<String> {
    if let Some(auth_header) = req.header::<String>("authorization") {
        return auth_header.strip_prefix("Bearer ").map(ToOwned::to_owned);
    }
    req.query::<String>("access_token")
}

// ---------------------------------------------------------------------------
// /_matrix routes — resolve appservice channel by hs_token
// ---------------------------------------------------------------------------

fn resolve_appservice_by_token(
    req: &Request,
    res: &mut Response,
) -> Option<MatrixAppserviceChannel> {
    let token = appservice_access_token(req);
    let provided = token.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let Some(provided) = provided else {
        debug_matrix_appservice("direct route auth failed reason='missing_access_token'");
        render_error(
            res,
            StatusCode::UNAUTHORIZED,
            "auth_failed",
            "missing access token",
        );
        return None;
    };
    let store = match matrix_appservice_store().lock() {
        Ok(store) => store,
        Err(err) => {
            debug_matrix_appservice(format!(
                "direct route auth failed reason='appservice_store_lock_failed' error='{}'",
                err
            ));
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "m_unknown",
                "matrix appservice store unavailable",
            );
            return None;
        }
    };
    let channel = store
        .values()
        .find(|ch| ch.homeserver_token() == provided)
        .cloned();
    if let Some(channel) = channel.as_ref() {
        debug_matrix_appservice(format!(
            "direct route auth ok config='{}'",
            channel.config_id()
        ));
    } else {
        debug_matrix_appservice("direct route auth failed reason='homeserver_token_mismatch'");
        render_error(
            res,
            StatusCode::UNAUTHORIZED,
            "auth_failed",
            "authentication failed",
        );
    }
    channel
}

#[handler]
async fn appservice_user_query(req: &mut Request, res: &mut Response) {
    let Some(channel) = resolve_appservice_by_token(req, res) else {
        return;
    };
    let user_id = req.param::<String>("user_id").unwrap_or_default();
    debug_matrix_appservice(format!(
        "direct user query config='{}' user_id='{}'",
        channel.config_id(),
        user_id,
    ));
    if channel.matches_user_id(&user_id) {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({})));
        debug_matrix_appservice(format!(
            "direct user query matched config='{}' user_id='{}'",
            channel.config_id(),
            user_id,
        ));
    } else {
        debug_matrix_appservice(format!(
            "direct user query missed config='{}' user_id='{}'",
            channel.config_id(),
            user_id,
        ));
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "user_does_not_exist",
            "user not created",
        );
    }
}

#[handler]
async fn appservice_room_query(req: &mut Request, res: &mut Response) {
    let Some(channel) = resolve_appservice_by_token(req, res) else {
        return;
    };
    let room_alias = req.param::<String>("room_alias").unwrap_or_default();
    debug_matrix_appservice(format!(
        "direct room query config='{}' alias='{}'",
        channel.config_id(),
        room_alias,
    ));
    if channel.matches_room_alias(&room_alias) {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({})));
        debug_matrix_appservice(format!(
            "direct room query matched config='{}' alias='{}'",
            channel.config_id(),
            room_alias,
        ));
    } else {
        debug_matrix_appservice(format!(
            "direct room query missed config='{}' alias='{}'",
            channel.config_id(),
            room_alias,
        ));
        render_error(
            res,
            StatusCode::NOT_FOUND,
            "room_alias_does_not_exist",
            "room alias not created",
        );
    }
}

#[handler]
async fn appservice_transaction(req: &mut Request, res: &mut Response) {
    let txn_id = req.param::<String>("txn_id").unwrap_or_default();
    debug_matrix_appservice(format!(
        "direct transaction route hit txn='{}' token={}",
        txn_id,
        appservice_token_state(appservice_access_token(req).as_deref()),
    ));
    let Some(channel) = resolve_appservice_by_token(req, res) else {
        return;
    };
    let body = match req.parse_json::<Value>().await {
        Ok(body) if body.get("events").and_then(Value::as_array).is_some() => body,
        Ok(_) => {
            debug_matrix_appservice(format!(
                "direct txn='{}' config='{}' rejected reason='missing_events_array'",
                txn_id,
                channel.config_id(),
            ));
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "bad_request",
                "invalid JSON: expected events",
            );
            return;
        }
        Err(err) => {
            debug_matrix_appservice(format!(
                "direct txn='{}' config='{}' rejected reason='invalid_json' error='{}'",
                txn_id,
                channel.config_id(),
                err,
            ));
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("invalid JSON: {err}"),
            );
            return;
        }
    };

    if let Err(err) = channel.handle_transaction(&txn_id, &body).await {
        update_matrix_runtime_state(channel.config_id(), |state| {
            state.connected = false;
            state.last_error = Some(err.to_string());
        });
        warn!(
            config_id = %channel.config_id(),
            txn_id,
            error = %err,
            "Matrix appservice direct transaction failed"
        );
        render_error(
            res,
            StatusCode::INTERNAL_SERVER_ERROR,
            "m_unknown",
            "transaction failed",
        );
        return;
    }

    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
    debug_matrix_appservice(format!(
        "direct txn='{}' config='{}' responded 200",
        txn_id,
        channel.config_id(),
    ));
}

#[handler]
async fn appservice_ping(req: &mut Request, res: &mut Response) {
    debug_matrix_appservice(format!(
        "direct ping received token={}",
        appservice_token_state(appservice_access_token(req).as_deref()),
    ));
    if resolve_appservice_by_token(req, res).is_none() {
        return;
    }
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
    debug_matrix_appservice("direct ping ok");
}

/// Routes at `/_matrix/app/v1/...` that resolve the appservice channel by hs_token.
pub(crate) fn matrix_appservice_router() -> Router {
    Router::with_path("_matrix/app/v1")
        .push(Router::with_path("users/{user_id}").get(appservice_user_query))
        .push(Router::with_path("rooms/{room_alias}").get(appservice_room_query))
        .push(
            Router::with_path("transactions/{txn_id}")
                .put(appservice_transaction)
                .post(appservice_transaction),
        )
        .push(Router::with_path("ping").post(appservice_ping))
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(body) = parse_json_body(req, res, "matrix").await else {
        return;
    };
    println!(
        "[matrix][webhook] request received event_count={} preview=[{}]",
        matrix_event_count(&body),
        matrix_event_preview(&body),
    );

    let gateway_channel = depot.obtain::<Arc<GatewayChannel>>().ok().cloned();
    let self_user_id = if let Some(channel) = gateway_channel.as_ref() {
        match channel
            .resolve_matrix_user_id_for_room(extract_first_room_id(&body))
            .await
        {
            Ok(user_id) => user_id,
            Err(err) => {
                warn!(error = %err, "failed to resolve Matrix webhook user id");
                None
            }
        }
    } else {
        None
    };

    let parsed = parse_inbound_payload_for_user(&body, self_user_id.as_deref());
    let dedupe_key = parsed
        .commands
        .first()
        .and_then(|command| command.dedupe_key.clone());

    if !parsed.rooms_to_auto_join.is_empty() {
        match depot.obtain::<Arc<GatewayChannel>>() {
            Ok(channel) => {
                let channel = channel.clone();
                for (room_id, invited_user_id) in &parsed.rooms_to_auto_join {
                    if let Err(err) = channel
                        .auto_join_matrix_invited_room(room_id, invited_user_id.as_deref())
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
            Err(_) => warn!("Matrix invite received but gateway channel state is unavailable"),
        }
    }

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let Some(command) = parsed.commands.into_iter().next() {
        let Some((gateway_channel, session_store)) = obtain_channel_and_store(depot, res) else {
            return;
        };
        let saved_channel_config_id = gateway_channel
            .resolve_matrix_outbound_config(&command.room_id)
            .await
            .ok()
            .flatten()
            .map(|config| config.id);
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "matrix",
                command.room_id.clone(),
                command.prompt,
                Some(command.sender.clone()),
                Some(runtime::StartThreadMeta {
                    peer_id: Some(command.sender),
                    group_id: Some(command.room_id),
                    chat_type: Some("group".to_string()),
                    saved_channel_config_id,
                    ..runtime::StartThreadMeta::default()
                }),
            )
            .await;
        });
    }

    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}

#[cfg(test)]
mod tests {
    use super::*;

    use salvo::conn::SocketAddr;
    use salvo::http::Method;
    use salvo::http::uri::{Scheme, Uri};
    use salvo::Service;

    #[handler]
    async fn spa_fallback() -> &'static str {
        "spa"
    }

    async fn call_service(service: &Service, method: Method, path: &str) -> Response {
        let mut req = Request::new();
        *req.method_mut() = method;
        req.set_uri(path.parse::<Uri>().expect("valid test URI"));
        service
            .hyper_handler(
                SocketAddr::Unknown,
                SocketAddr::Unknown,
                Scheme::HTTP,
                None,
                None,
            )
            .handle(req)
            .await
    }

    #[tokio::test]
    async fn direct_matrix_transactions_accept_put_without_falling_back_to_spa() {
        let router = Router::new()
            .push(matrix_appservice_router())
            .push(Router::with_path("{**rest}").get(spa_fallback));
        let service = Service::new(router);

        let res = call_service(
            &service,
            Method::PUT,
            "/_matrix/app/v1/transactions/-6RtNsrCwDuvkf81xU9OHz3Dhw9kchuObKxGvW93kv8",
        )
        .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn direct_matrix_transactions_accept_post_for_homeserver_compatibility() {
        let router = Router::new()
            .push(matrix_appservice_router())
            .push(Router::with_path("{**rest}").get(spa_fallback));
        let service = Service::new(router);

        let res = call_service(
            &service,
            Method::POST,
            "/_matrix/app/v1/transactions/-6RtNsrCwDuvkf81xU9OHz3Dhw9kchuObKxGvW93kv8",
        )
        .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn config_scoped_appservice_transactions_accept_post_for_compatibility() {
        let router = Router::new().push(appservice_router());
        let service = Service::new(router);

        let res = call_service(
            &service,
            Method::POST,
            "/appservices/matrix/savfox/_matrix/app/v1/transactions/test-txn",
        )
        .await;

        assert_eq!(res.status_code.unwrap(), StatusCode::UNAUTHORIZED);
    }

}
