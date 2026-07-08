use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
    MatrixAutoJoin, MatrixChannelConfig as MatrixPlatformConfig, MatrixCommandEvent,
    MatrixInviteEvent, parse_appservice_message_event_for_user, parse_inbound_payload_for_user,
    parse_invite_event,
};
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::{obtain_channel_and_store, parse_json_body, render_error, runtime};
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

const MATRIX_INVITES_FILE: &str = "matrix-invites.json";

#[derive(Debug, Clone, Default)]
pub(crate) struct MatrixRuntimeState {
    pub mode: Option<String>,
    pub homeserver: Option<String>,
    pub user_id: Option<String>,
    pub access_token: Option<String>,
    pub connected: bool,
    pub room_count: Option<u32>,
    pub pending_invites: Option<u32>,
    pub auto_join: Option<String>,
    pub appservice_url: Option<String>,
    pub sender_localpart: Option<String>,
    pub user_prefix: Option<String>,
    pub server_name: Option<String>,
    pub config_id: Option<String>,
    pub registration: Option<Value>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MatrixPendingInvite {
    pub config_id: String,
    pub room_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_event_id: Option<String>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl MatrixPendingInvite {
    pub(crate) fn from_event(config_id: &str, invite: &MatrixInviteEvent) -> Self {
        let now = chrono::Utc::now();
        Self {
            config_id: config_id.to_owned(),
            room_id: invite.room_id.clone(),
            invited_user_id: invite.invited_user_id.clone(),
            inviter: invite.inviter.clone(),
            room_name: invite.room_name.clone(),
            canonical_alias: invite.canonical_alias.clone(),
            membership_event_id: invite.membership_event_id.clone(),
            received_at: now,
            last_seen_at: now,
            dismissed_at: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MatrixInviteStoreData {
    #[serde(default)]
    invites: Vec<MatrixPendingInvite>,
}

#[derive(Debug, Clone)]
pub(crate) struct MatrixInviteStore {
    path: PathBuf,
}

impl MatrixInviteStore {
    pub(crate) fn for_savfox_home(savfox_home: &Path) -> Self {
        Self {
            path: savfox_home.join("gateway").join(MATRIX_INVITES_FILE),
        }
    }

    #[cfg(test)]
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    async fn load_data(&self) -> anyhow::Result<MatrixInviteStoreData> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(body) => serde_json::from_str(&body).with_context(|| {
                format!(
                    "failed to parse Matrix invite store {}",
                    self.path.display()
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(MatrixInviteStoreData::default())
            }
            Err(err) => Err(err).with_context(|| {
                format!("failed to read Matrix invite store {}", self.path.display())
            }),
        }
    }

    async fn save_data(&self, data: &MatrixInviteStoreData) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create Matrix invite dir {}", parent.display())
            })?;
        }
        let body =
            serde_json::to_string_pretty(data).context("failed to serialize Matrix invites")?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, body)
            .await
            .with_context(|| format!("failed to write Matrix invite store {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &self.path).await.with_context(|| {
            format!(
                "failed to replace Matrix invite store {}",
                self.path.display()
            )
        })?;
        Ok(())
    }

    pub(crate) async fn list(
        &self,
        include_dismissed: bool,
    ) -> anyhow::Result<Vec<MatrixPendingInvite>> {
        let mut invites = self.load_data().await?.invites;
        if !include_dismissed {
            invites.retain(|invite| invite.dismissed_at.is_none());
        }
        invites.sort_by_key(|invite| std::cmp::Reverse(invite.last_seen_at));
        Ok(invites)
    }

    pub(crate) async fn upsert(&self, invite: MatrixPendingInvite) -> anyhow::Result<()> {
        let mut data = self.load_data().await?;
        match data.invites.iter_mut().find(|existing| {
            existing.config_id == invite.config_id
                && existing.room_id.eq_ignore_ascii_case(&invite.room_id)
        }) {
            Some(existing) => {
                let membership_changed = invite.membership_event_id.is_some()
                    && existing.membership_event_id != invite.membership_event_id;
                existing.invited_user_id = invite.invited_user_id;
                existing.inviter = invite.inviter;
                existing.room_name = invite.room_name;
                existing.canonical_alias = invite.canonical_alias;
                existing.membership_event_id = invite.membership_event_id;
                existing.last_seen_at = invite.last_seen_at;
                if membership_changed {
                    existing.received_at = invite.received_at;
                    existing.dismissed_at = None;
                }
            }
            None => data.invites.push(invite),
        }
        self.save_data(&data).await
    }

    pub(crate) async fn remove(&self, config_id: &str, room_id: &str) -> anyhow::Result<bool> {
        let mut data = self.load_data().await?;
        let original_len = data.invites.len();
        data.invites.retain(|invite| {
            invite.config_id != config_id || !invite.room_id.eq_ignore_ascii_case(room_id)
        });
        let removed = data.invites.len() != original_len;
        if removed {
            self.save_data(&data).await?;
        }
        Ok(removed)
    }

    pub(crate) async fn dismiss(&self, config_id: &str, room_id: &str) -> anyhow::Result<bool> {
        let mut data = self.load_data().await?;
        let now = chrono::Utc::now();
        let mut updated = false;
        for invite in &mut data.invites {
            if invite.config_id == config_id && invite.room_id.eq_ignore_ascii_case(room_id) {
                invite.dismissed_at = Some(now);
                updated = true;
            }
        }
        if updated {
            self.save_data(&data).await?;
        }
        Ok(updated)
    }
}

fn matrix_runtime_state_store() -> &'static Mutex<HashMap<String, MatrixRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, MatrixRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn matrix_appservice_store() -> &'static Mutex<HashMap<String, MatrixAppserviceChannel>> {
    static STORE: OnceLock<Mutex<HashMap<String, MatrixAppserviceChannel>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn debug_matrix_appservice(message: impl AsRef<str>) {
    debug!("{}", message.as_ref());
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
        return "none".to_owned();
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

        let mut part = event_type.to_owned();
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
        store.insert(config_id.to_owned(), state);
    }
}

fn update_matrix_runtime_state(config_id: &str, apply: impl FnOnce(&mut MatrixRuntimeState)) {
    if let Ok(mut store) = matrix_runtime_state_store().lock() {
        apply(store.entry(config_id.to_owned()).or_default());
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

pub(crate) fn matrix_appservice_registration_preview(config: &MatrixPlatformConfig) -> Value {
    let server_name = config.server_name.as_deref().unwrap_or_default();
    let public_url = config.public_url.as_deref().unwrap_or_default();
    let appservice_id = config.appservice_id.as_deref().unwrap_or_default();
    let appservice_token = config.appservice_token.as_deref().unwrap_or_default();
    let homeserver_token = config.homeserver_token.as_deref().unwrap_or_default();
    let sender_localpart = config.sender_localpart.as_deref().unwrap_or_default();
    let escaped_server_name = regex::escape(server_name);
    let escaped_user_prefix = regex::escape(&config.user_prefix);
    let escaped_alias_prefix = regex::escape(&config.alias_prefix);
    let escaped_bot = regex::escape(sender_localpart);

    json!({
        "id": appservice_id,
        "url": public_url.trim_end_matches('/'),
        "as_token": appservice_token,
        "hs_token": homeserver_token,
        "sender_localpart": sender_localpart,
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

fn set_matrix_appservice_channel(config_id: &str, channel: MatrixAppserviceChannel) {
    if let Ok(mut store) = matrix_appservice_store().lock() {
        store.insert(config_id.to_owned(), channel);
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
    savfox_home: PathBuf,
    rooms: Vec<String>,
    auto_join: MatrixAutoJoin,
    auto_join_allowlist: Vec<String>,
    allowed_senders: HashSet<String>,
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
    rooms: Vec<String>,
    auto_join: MatrixAutoJoin,
    auto_join_allowlist: Vec<String>,
    allowed_senders: HashSet<String>,
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
            rooms,
            auto_join,
            auto_join_allowlist,
            allowed_senders,
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
            rooms,
            auto_join,
            auto_join_allowlist,
            allowed_senders: allowed_senders
                .into_iter()
                .map(|sender| sender.trim().to_owned())
                .filter(|sender| !sender.is_empty())
                .collect(),
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
            .map(str::to_owned);

        handle_matrix_invites(
            &MatrixSyncTask {
                config_id: self.config_id.clone(),
                homeserver: self.homeserver.clone(),
                user_id: resolved.user_id.clone(),
                client: resolved.client.clone(),
                savfox_home: self.gateway_channel.config().savfox_home.clone(),
                rooms: self.rooms.clone(),
                auto_join: self.auto_join,
                auto_join_allowlist: self.auto_join_allowlist.clone(),
                allowed_senders: self.allowed_senders.clone(),
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
                mode: Some("user".to_owned()),
                homeserver: Some(self.homeserver.clone()),
                user_id: Some(resolved.user_id.clone()),
                access_token: Some(resolved.access_token.clone()),
                connected: true,
                room_count,
                pending_invites: pending_matrix_invite_count(
                    &self.gateway_channel.config().savfox_home,
                    Some(&self.config_id),
                )
                .await,
                auto_join: Some(self.auto_join.as_str().to_owned()),
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
            savfox_home: self.gateway_channel.config().savfox_home.clone(),
            rooms: self.rooms.clone(),
            auto_join: self.auto_join,
            auto_join_allowlist: self.auto_join_allowlist.clone(),
            allowed_senders: self.allowed_senders.clone(),
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
    /// Optional allowlist of Matrix user ids permitted to drive the agent. When
    /// non-empty, transactions from any other sender are dropped. Mirrors the
    /// user-mode `MatrixSyncTask::allowed_senders` enforcement.
    allowed_senders: HashSet<String>,
    client: MatrixClient,
    room_users: Mutex<HashMap<String, String>>,
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
            .to_owned();
        let public_url = config
            .public_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires publicUrl")?
            .to_owned();
        let appservice_id = config
            .appservice_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires appserviceId")?
            .to_owned();
        let appservice_token = config
            .appservice_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires appserviceToken")?
            .to_owned();
        let homeserver_token = config
            .homeserver_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires homeserverToken")?
            .to_owned();
        let sender_localpart = config
            .sender_localpart
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("Matrix appservice mode requires senderLocalpart")?
            .to_owned();
        let homeserver_url = url::Url::parse(&config.homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {}", config.homeserver))?;
        let bot_user_id = format!("@{sender_localpart}:{server_name}");
        let allowed_senders: HashSet<String> = config
            .allowed_senders
            .iter()
            .map(|sender| sender.trim().to_owned())
            .filter(|sender| !sender.is_empty())
            .collect();
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
                allowed_senders,
                client,
                room_users: Mutex::new(HashMap::new()),
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
        self.inner.public_url.trim_end_matches('/').to_owned()
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

    fn client_for_user_id(&self, user_id: &str) -> anyhow::Result<MatrixClient> {
        let homeserver_url = url::Url::parse(&self.inner.homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {}", self.inner.homeserver))?;
        let auth =
            MatrixAuth::new(self.inner.appservice_token.clone()).with_user_id(user_id.to_owned());
        Ok(MatrixClient::new(homeserver_url, auth))
    }

    fn matrix_client_api_url(
        &self,
        path_segments: &[&str],
        acting_user_id: Option<&str>,
    ) -> anyhow::Result<url::Url> {
        let mut url = url::Url::parse(&self.inner.homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {}", self.inner.homeserver))?;
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                anyhow::anyhow!(
                    "invalid Matrix homeserver URL for client API paths: {}",
                    self.inner.homeserver
                )
            })?;
            segments.pop_if_empty();
            segments.extend(path_segments);
        }
        if let Some(acting_user_id) =
            acting_user_id.and_then(|value| non_empty_trimmed(Some(value)))
        {
            url.query_pairs_mut().append_pair("user_id", acting_user_id);
        }
        Ok(url)
    }

    async fn appservice_request_json_as_user(
        &self,
        method: Method,
        path_segments: &[&str],
        acting_user_id: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<(reqwest::StatusCode, Value)> {
        let url = self.matrix_client_api_url(path_segments, Some(acting_user_id))?;
        // Build a fresh SSRF-pinned client per request: re-validates the
        // URL against the SSRF policy and pins the resolved IP into the
        // resolver, defending against DNS rebinding between validation
        // and the actual TCP connect (S11 follow-up to #33).
        let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
        let client = crate::ssrf::build_pinned_client(url.as_str(), &ssrf_cfg)
            .await
            .with_context(|| format!("matrix appservice url rejected by ssrf policy: {url}"))?;
        let mut request = client
            .request(method.clone(), url.clone())
            .bearer_auth(&self.inner.appservice_token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.with_context(|| {
            format!(
                "Matrix appservice request failed method={} url={url}",
                method.as_str()
            )
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok((status, Value::Null));
        }
        let payload = response.json::<Value>().await.with_context(|| {
            format!(
                "failed to decode Matrix appservice response with status {} for {}",
                status.as_u16(),
                url
            )
        })?;
        Ok((status, payload))
    }

    fn bind_room_user(&self, room_id: &str, user_id: &str) {
        let room_id = room_id.trim();
        let user_id = user_id.trim();
        if room_id.is_empty() || user_id.is_empty() {
            return;
        }
        let previous_user_id = if let Ok(mut room_users) = self.inner.room_users.lock() {
            room_users.insert(room_id.to_owned(), user_id.to_owned())
        } else {
            None
        };
        match previous_user_id {
            Some(previous_user_id) if previous_user_id.eq_ignore_ascii_case(user_id) => {
                debug_matrix_appservice(format!(
                    "config='{}' room='{}' bound_user='{}' binding='unchanged'",
                    self.inner.config_id, room_id, user_id,
                ));
            }
            Some(previous_user_id) => {
                debug_matrix_appservice(format!(
                    "config='{}' room='{}' bound_user='{}' previous_user='{}' binding='updated'",
                    self.inner.config_id, room_id, user_id, previous_user_id,
                ));
            }
            None => {
                debug_matrix_appservice(format!(
                    "config='{}' room='{}' bound_user='{}' binding='created'",
                    self.inner.config_id, room_id, user_id,
                ));
            }
        }
    }

    fn room_user_for(&self, room_id: &str) -> Option<String> {
        let room_id = room_id.trim();
        if room_id.is_empty() {
            return None;
        }
        self.inner
            .room_users
            .lock()
            .ok()
            .and_then(|room_users| room_users.get(room_id).cloned())
    }

    fn effective_room_user_id(&self, room_id: &str) -> String {
        self.room_user_for(room_id)
            .unwrap_or_else(|| self.inner.bot_user_id.clone())
    }

    fn agent_id_for_user(&self, user_id: &str) -> Option<String> {
        if user_id.eq_ignore_ascii_case(&self.inner.bot_user_id) {
            return None;
        }
        if !user_id.ends_with(&format!(":{}", self.inner.server_name)) {
            return None;
        }
        let localpart = matrix_localpart(user_id)?;
        let agent_id = localpart.strip_prefix(&self.inner.user_prefix)?.trim();
        if agent_id.is_empty() {
            None
        } else {
            Some(agent_id.to_owned())
        }
    }

    fn sender_kind_for_user(&self, user_id: &str) -> runtime::SenderKind {
        if user_id.eq_ignore_ascii_case(&self.inner.bot_user_id) {
            return runtime::SenderKind::SelfBot;
        }
        if self.should_ignore_sender(user_id) {
            return runtime::SenderKind::OwnAgentGhost;
        }
        if matrix_localpart(user_id).is_none() {
            return runtime::SenderKind::Unknown;
        }
        if matrix_localpart_looks_like_bot(user_id) {
            runtime::SenderKind::ExternalBot
        } else {
            runtime::SenderKind::Human
        }
    }

    fn leading_agent_id_in_body(&self, body: &str) -> Option<String> {
        let trimmed = body.trim_start();
        let token = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches([':', ',', ';', '>']);
        let localpart = matrix_localpart(token)
            .or_else(|| Some(token.trim_start_matches('@').trim()))
            .filter(|value| !value.is_empty())?;
        let agent_id = localpart.strip_prefix(&self.inner.user_prefix)?.trim();
        if agent_id.is_empty() {
            None
        } else {
            Some(agent_id.to_owned())
        }
    }

    fn mentioned_agent_ids_for_event(&self, event: &Value) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(user_ids) = event
            .get("content")
            .and_then(|content| content.get("m.mentions"))
            .and_then(|mentions| mentions.get("user_ids"))
            .and_then(Value::as_array)
        {
            for user_id in user_ids.iter().filter_map(Value::as_str) {
                if let Some(agent_id) = self.agent_id_for_user(user_id) {
                    out.push(agent_id);
                }
            }
        }
        if let Some(body) = event
            .get("content")
            .and_then(|content| content.get("body"))
            .and_then(Value::as_str)
            && let Some(agent_id) = self.leading_agent_id_in_body(body)
        {
            out.push(agent_id);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    async fn participant_count_for_room(&self, room_id: &str) -> Option<u32> {
        let user_id = self.effective_room_user_id(room_id);
        let (status, payload) = self
            .appservice_request_json_as_user(
                Method::GET,
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    room_id,
                    "joined_members",
                ],
                &user_id,
                None,
            )
            .await
            .ok()?;
        if !status.is_success() {
            return None;
        }
        payload
            .get("joined")
            .and_then(Value::as_object)
            .map(|joined| joined.len() as u32)
            .or_else(|| {
                payload
                    .get("joined_members")
                    .and_then(Value::as_object)
                    .map(|joined| joined.len() as u32)
            })
    }

    fn joined_membership_user_for_event(
        &self,
        event: &Value,
        expected_user_id: Option<&str>,
    ) -> Option<(String, String)> {
        if event.get("type").and_then(Value::as_str) != Some("m.room.member") {
            return None;
        }
        let membership = event
            .get("content")
            .and_then(|content| content.get("membership"))
            .and_then(Value::as_str)?;
        if !membership.eq_ignore_ascii_case("join") {
            return None;
        }
        let room_id = event
            .get("room_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let user_id = event
            .get("state_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let sender = event
            .get("sender")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        let bound_user_id = self.room_user_for(room_id);
        let expected_user_id = expected_user_id
            .and_then(|value| non_empty_trimmed(Some(value)))
            .map(ToOwned::to_owned)
            .or(bound_user_id);

        if let Some(expected_user_id) = expected_user_id.as_deref()
            && !expected_user_id.eq_ignore_ascii_case(user_id)
        {
            debug_matrix_appservice(format!(
                "config='{}' room='{}' membership='join' joined_user='{}' sender='{}' expected_user='{}' match_expected=false",
                self.inner.config_id, room_id, user_id, sender, expected_user_id,
            ));
        }

        if !self.matches_user_id(user_id) {
            if let Some(expected_user_id) = expected_user_id.as_deref() {
                debug_matrix_appservice(format!(
                    "config='{}' room='{}' membership='join' joined_user='{}' sender='{}' expected_user='{}' reason='join_event_not_in_appservice_namespace'",
                    self.inner.config_id, room_id, user_id, sender, expected_user_id,
                ));
            }
            return None;
        }

        debug_matrix_appservice(format!(
            "config='{}' room='{}' membership='join' joined_user='{}' sender='{}' expected_user='{}' match_expected={}",
            self.inner.config_id,
            room_id,
            user_id,
            sender,
            expected_user_id.as_deref().unwrap_or(""),
            expected_user_id
                .as_deref()
                .map(|expected| expected.eq_ignore_ascii_case(user_id))
                .unwrap_or(true),
        ));
        Some((room_id.to_owned(), user_id.to_owned()))
    }

    async fn refresh_room_count(&self) {
        let bound_count = self
            .inner
            .room_users
            .lock()
            .ok()
            .map(|room_users| room_users.len() as u32)
            .filter(|count| *count > 0);
        let room_count = match bound_count {
            Some(count) => Some(count),
            None => self
                .inner
                .client
                .get_joined_rooms()
                .await
                .ok()
                .map(|rooms| rooms.len() as u32),
        };
        update_matrix_runtime_state(&self.inner.config_id, |state| {
            state.room_count = room_count;
        });
    }

    pub(crate) async fn join_room(&self, room_id: &str) -> anyhow::Result<()> {
        let user_id = self.inner.bot_user_id.clone();
        self.join_room_as_user(room_id, &user_id).await
    }

    async fn join_room_as_user(&self, room_id: &str, user_id: &str) -> anyhow::Result<()> {
        let room_id = room_id.trim();
        let user_id = user_id.trim();
        let previous_user_id = self.room_user_for(room_id);
        debug_matrix_appservice(format!(
            "config='{}' room='{}' join_request='sending' as_user='{}' previous_user='{}'",
            self.inner.config_id,
            room_id,
            user_id,
            previous_user_id.as_deref().unwrap_or(""),
        ));
        let request_body = json!({});
        let (status, payload) = match self
            .appservice_request_json_as_user(
                Method::POST,
                &["_matrix", "client", "v3", "join", room_id],
                user_id,
                Some(&request_body),
            )
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let err = err.context(format!(
                    "failed to auto-join Matrix room {room_id} as {user_id}"
                ));
                debug_matrix_appservice(format!(
                    "config='{}' room='{}' join_request='failed' as_user='{}' error='{}'",
                    self.inner.config_id, room_id, user_id, err,
                ));
                return Err(err);
            }
        };
        if !status.is_success() {
            let err = anyhow::anyhow!(
                "failed to auto-join Matrix room {room_id} as {user_id}: status={} errcode='{}' error='{}' body={}",
                status.as_u16(),
                payload.get("errcode").and_then(Value::as_str).unwrap_or(""),
                payload.get("error").and_then(Value::as_str).unwrap_or(""),
                payload,
            );
            debug_matrix_appservice(format!(
                "config='{}' room='{}' join_request='failed' as_user='{}' status={} errcode='{}' error='{}' body={}",
                self.inner.config_id,
                room_id,
                user_id,
                status.as_u16(),
                payload.get("errcode").and_then(Value::as_str).unwrap_or(""),
                payload.get("error").and_then(Value::as_str).unwrap_or(""),
                payload,
            ));
            return Err(err);
        }
        let joined_room_id = payload
            .get("room_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(room_id);
        debug_matrix_appservice(format!(
            "config='{}' room='{}' join_request='accepted' as_user='{}' status={} joined_room='{}'",
            self.inner.config_id,
            room_id,
            user_id,
            status.as_u16(),
            joined_room_id,
        ));
        self.bind_room_user(joined_room_id, user_id);
        self.refresh_room_count().await;
        Ok(())
    }

    async fn send_room_message_as_user(
        &self,
        room_id: &str,
        user_id: &str,
        content: &Value,
    ) -> anyhow::Result<String> {
        let room_id = room_id.trim();
        let user_id = user_id.trim();
        let txn_id = uuid::Uuid::now_v7().to_string();
        let (status, payload) = self
            .appservice_request_json_as_user(
                Method::PUT,
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    room_id,
                    "send",
                    "m.room.message",
                    &txn_id,
                ],
                user_id,
                Some(content),
            )
            .await
            .with_context(|| {
                format!("failed to send Matrix appservice message to room {room_id} as {user_id}")
            })?;
        if !status.is_success() {
            anyhow::bail!(
                "failed to send Matrix appservice message to room {room_id} as {user_id}: status={} errcode='{}' error='{}' body={}",
                status.as_u16(),
                payload.get("errcode").and_then(Value::as_str).unwrap_or(""),
                payload.get("error").and_then(Value::as_str).unwrap_or(""),
                payload,
            );
        }
        payload
            .get("event_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .context("missing event_id in Matrix appservice send response")
    }

    async fn handle_transaction(&self, txn_id: &str, body: &Value) -> anyhow::Result<()> {
        debug_matrix_appservice(format!(
            "config='{}' txn='{}' received event_count={} preview=[{}]",
            self.inner.config_id,
            txn_id,
            matrix_event_count(body),
            matrix_event_preview(body),
        ));
        let events = body
            .get("events")
            .and_then(Value::as_array)
            .context("Matrix appservice transaction missing events array")?;
        update_matrix_runtime_state(&self.inner.config_id, |state| {
            state.connected = true;
            state.last_error = None;
        });

        let mut rooms_to_auto_join = Vec::new();
        for event in events {
            if let Some(invite) = parse_invite_event(event, None)
                && !rooms_to_auto_join
                    .iter()
                    .any(|existing: &MatrixInviteEvent| {
                        existing.room_id.eq_ignore_ascii_case(&invite.room_id)
                    })
            {
                rooms_to_auto_join.push(invite);
            }
        }

        for event in events {
            let expected_join_user_id = event
                .get("room_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .and_then(|room_id| {
                    rooms_to_auto_join
                        .iter()
                        .find(|candidate| candidate.room_id.eq_ignore_ascii_case(room_id))
                        .and_then(|invite| {
                            match invite
                                .invited_user_id
                                .as_deref()
                                .and_then(|value| non_empty_trimmed(Some(value)))
                            {
                                Some(invited_user_id) if self.matches_user_id(invited_user_id) => {
                                    Some(invited_user_id)
                                }
                                Some(_) => None,
                                None => Some(self.inner.bot_user_id.as_str()),
                            }
                        })
                });
            if let Some((room_id, user_id)) =
                self.joined_membership_user_for_event(event, expected_join_user_id)
            {
                self.bind_room_user(&room_id, &user_id);
            }
        }

        let mut joined_rooms = 0_u32;
        let mut skipped_invites = 0_u32;

        for invite in &rooms_to_auto_join {
            if let Some(invited_user_id) = invite
                .invited_user_id
                .as_deref()
                .and_then(|value| non_empty_trimmed(Some(value)))
                && !self.matches_user_id(invited_user_id)
            {
                skipped_invites = skipped_invites.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' skipping invite room='{}' invited_user='{}' reason='invite_not_for_bot'",
                    self.inner.config_id, txn_id, invite.room_id, invited_user_id,
                ));
                continue;
            }
            let join_user_id = invite
                .invited_user_id
                .as_deref()
                .and_then(|value| non_empty_trimmed(Some(value)))
                .unwrap_or(&self.inner.bot_user_id);
            debug_matrix_appservice(format!(
                "config='{}' txn='{}' room='{}' invite='auto_join_planned' invited_user='{}' join_as='{}'",
                self.inner.config_id,
                txn_id,
                invite.room_id,
                invite.invited_user_id.as_deref().unwrap_or(""),
                join_user_id,
            ));
            self.join_room_as_user(&invite.room_id, join_user_id)
                .await?;
            joined_rooms = joined_rooms.saturating_add(1);
            debug_matrix_appservice(format!(
                "config='{}' txn='{}' joined room='{}' as_user='{}'",
                self.inner.config_id, txn_id, invite.room_id, join_user_id
            ));
        }

        let mut commands = Vec::new();
        for event in events {
            let room_id = event
                .get("room_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let room_user_id = self.effective_room_user_id(room_id);
            if let Some(command) =
                parse_appservice_message_event_for_user(event, Some(&room_user_id))
            {
                let mentioned_agent_ids = self.mentioned_agent_ids_for_event(event);
                let mut forced_agent_id = self.agent_id_for_user(&room_user_id);
                if forced_agent_id.is_none() && mentioned_agent_ids.len() == 1 {
                    forced_agent_id = mentioned_agent_ids.first().cloned();
                }
                let explicitly_targets_other_agent = mentioned_agent_ids
                    .iter()
                    .any(|agent_id| Some(agent_id.as_str()) != forced_agent_id.as_deref());
                let is_mentioned = command.is_mentioned
                    || forced_agent_id.as_deref().is_some_and(|agent_id| {
                        mentioned_agent_ids.iter().any(|value| value == agent_id)
                    });
                let participant_count = self.participant_count_for_room(room_id).await;
                let sender_kind = self.sender_kind_for_user(&command.sender);
                commands.push((
                    command,
                    forced_agent_id,
                    is_mentioned,
                    explicitly_targets_other_agent,
                    participant_count,
                    sender_kind,
                ));
            }
        }
        debug_matrix_appservice(format!(
            "config='{}' txn='{}' parsed commands={} invites={}",
            self.inner.config_id,
            txn_id,
            commands.len(),
            rooms_to_auto_join.len(),
        ));

        let mut ignored_senders = 0_u32;
        let mut ignored_duplicates = 0_u32;
        let mut dispatched_commands = 0_u32;
        for (
            command,
            forced_agent_id,
            is_mentioned,
            explicitly_targets_other_agent,
            participant_count,
            sender_kind,
        ) in commands
        {
            if self.should_ignore_sender(&command.sender) {
                ignored_senders = ignored_senders.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' ignoring command room='{}' sender='{}' reason='sender_in_appservice_namespace'",
                    self.inner.config_id, txn_id, command.room_id, command.sender,
                ));
                continue;
            }
            if !self.inner.allowed_senders.is_empty()
                && !self.inner.allowed_senders.contains(command.sender.trim())
            {
                ignored_senders = ignored_senders.saturating_add(1);
                debug_matrix_appservice(format!(
                    "config='{}' txn='{}' ignoring command room='{}' sender='{}' reason='sender_not_in_allowlist'",
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
                "config='{}' txn='{}' dispatching command room='{}' sender='{}' forced_agent='{}'",
                self.inner.config_id,
                txn_id,
                command.room_id,
                command.sender,
                forced_agent_id.as_deref().unwrap_or(""),
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
                        forced_agent_id,
                        group_id: Some(command.room_id),
                        chat_type: Some("group".to_owned()),
                        saved_channel_config_id: Some(config_id),
                        sender_kind,
                        is_mentioned,
                        is_command: false,
                        used_plain_text_fallback: command.used_plain_text_fallback,
                        participant_count,
                        explicitly_targets_other_agent,
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
                mode: Some("appservice".to_owned()),
                homeserver: Some(self.inner.homeserver.clone()),
                user_id: Some(self.inner.bot_user_id.clone()),
                access_token: None,
                connected: true,
                room_count,
                pending_invites: pending_matrix_invite_count(
                    &self.inner.gateway_channel.config().savfox_home,
                    Some(&self.inner.config_id),
                )
                .await,
                auto_join: Some(MatrixAutoJoin::Always.as_str().to_owned()),
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
                .unwrap_or_else(|| "unknown".to_owned()),
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
        let user_id = self.effective_room_user_id(channel);
        let content = json!({
            "msgtype": "m.text",
            "body": message,
        });
        self.send_room_message_as_user(channel, &user_id, &content)
            .await
            .map(|_| ())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let user_id = self.effective_room_user_id(channel);
        let content = json!({
            "msgtype": "m.text",
            "body": render_matrix_rich_text(&msg),
            "format": "org.matrix.custom.html",
            "formatted_body": render_matrix_rich_html(&msg),
        });
        self.send_room_message_as_user(channel, &user_id, &content)
            .await
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

/// Heuristic: does this Matrix user id's localpart look like a bot account?
fn matrix_localpart_looks_like_bot(user_id: &str) -> bool {
    let Some(localpart) = matrix_localpart(user_id) else {
        return false;
    };
    let localpart = localpart.to_ascii_lowercase();
    localpart.ends_with("bot")
        || localpart.starts_with("bot")
        || localpart.contains("_bot")
        || localpart.contains("bot_")
}

/// Classify an inbound Matrix sender for the user-mode and webhook paths, which
/// lack the appservice namespace context. `self_user_id` is the bot's own
/// Matrix id so its own echoes classify as `SelfBot`. Mirrors the appservice
/// `sender_kind_for_user` so `ExternalBotPolicy` applies consistently across all
/// three Matrix ingest paths.
fn matrix_user_mode_sender_kind(sender: &str, self_user_id: Option<&str>) -> runtime::SenderKind {
    if let Some(self_id) = self_user_id
        && sender.eq_ignore_ascii_case(self_id)
    {
        return runtime::SenderKind::SelfBot;
    }
    if matrix_localpart(sender).is_none() {
        return runtime::SenderKind::Unknown;
    }
    if matrix_localpart_looks_like_bot(sender) {
        runtime::SenderKind::ExternalBot
    } else {
        runtime::SenderKind::Human
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

async fn pending_matrix_invite_count(savfox_home: &Path, config_id: Option<&str>) -> Option<u32> {
    let store = MatrixInviteStore::for_savfox_home(savfox_home);
    let invites = store.list(false).await.ok()?;
    let count = invites
        .iter()
        .filter(|invite| config_id.map(|id| invite.config_id == id).unwrap_or(true))
        .count();
    Some(count as u32)
}

fn matrix_invite_matches_target(invite: &MatrixInviteEvent, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    if target == "*" {
        return true;
    }
    invite.room_id.eq_ignore_ascii_case(target)
        || invite
            .canonical_alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(target))
}

fn matrix_invite_auto_join_allowed(task: &MatrixSyncTask, invite: &MatrixInviteEvent) -> bool {
    match task.auto_join {
        MatrixAutoJoin::Off => false,
        MatrixAutoJoin::Always => true,
        MatrixAutoJoin::Allowlist => {
            let targets = if task.auto_join_allowlist.is_empty() {
                &task.rooms
            } else {
                &task.auto_join_allowlist
            };
            !targets.is_empty()
                && targets
                    .iter()
                    .any(|target| matrix_invite_matches_target(invite, target))
        }
    }
}

async fn record_matrix_pending_invite(task: &MatrixSyncTask, invite: &MatrixInviteEvent) {
    let store = MatrixInviteStore::for_savfox_home(&task.savfox_home);
    if let Err(err) = store
        .upsert(MatrixPendingInvite::from_event(&task.config_id, invite))
        .await
    {
        warn!(
            config_id = %task.config_id,
            room_id = %invite.room_id,
            error = %err,
            "failed to record pending Matrix invite"
        );
    }
    let pending = pending_matrix_invite_count(&task.savfox_home, Some(&task.config_id)).await;
    update_matrix_runtime_state(&task.config_id, |state| {
        state.pending_invites = pending;
    });
}

async fn clear_matrix_pending_invite(task: &MatrixSyncTask, room_id: &str) {
    let store = MatrixInviteStore::for_savfox_home(&task.savfox_home);
    if let Err(err) = store.remove(&task.config_id, room_id).await {
        warn!(
            config_id = %task.config_id,
            room_id,
            error = %err,
            "failed to clear pending Matrix invite"
        );
    }
    let pending = pending_matrix_invite_count(&task.savfox_home, Some(&task.config_id)).await;
    update_matrix_runtime_state(&task.config_id, |state| {
        state.pending_invites = pending;
    });
}

async fn handle_matrix_invites(task: &MatrixSyncTask, invites: Vec<MatrixInviteEvent>) {
    for invite in invites {
        if let Some(invited_user_id) = invite
            .invited_user_id
            .as_deref()
            .and_then(|value| non_empty_trimmed(Some(value)))
            && !invited_user_id.eq_ignore_ascii_case(&task.user_id)
        {
            continue;
        }

        if !matrix_invite_auto_join_allowed(task, &invite) {
            record_matrix_pending_invite(task, &invite).await;
            continue;
        }

        match task.client.join_room(&invite.room_id).await {
            Ok(_) => {
                if let Ok(joined_rooms) = task.client.get_joined_rooms().await {
                    update_matrix_runtime_state(&task.config_id, |state| {
                        state.room_count = Some(joined_rooms.len() as u32);
                    });
                }
                clear_matrix_pending_invite(task, &invite.room_id).await;
            }
            Err(err) => {
                update_matrix_runtime_state(&task.config_id, |state| {
                    state.last_error = Some(err.to_string());
                });
                warn!(
                    config_id = %task.config_id,
                    room_id = %invite.room_id,
                    error = %err,
                    "Matrix invite auto-join failed"
                );
                record_matrix_pending_invite(task, &invite).await;
            }
        }
    }
}

async fn dispatch_matrix_commands(task: &MatrixSyncTask, commands: Vec<MatrixCommandEvent>) {
    for command in commands {
        if command.sender.eq_ignore_ascii_case(&task.user_id) {
            continue;
        }
        if !task.allowed_senders.is_empty() && !task.allowed_senders.contains(command.sender.trim())
        {
            continue;
        }
        if runtime::should_drop_duplicate(command.dedupe_key.clone()).await {
            continue;
        }

        let gateway_channel = Arc::clone(&task.gateway_channel);
        let session_store = Arc::clone(&task.session_store);
        let config_id = task.config_id.clone();
        let sender_kind = matrix_user_mode_sender_kind(&command.sender, Some(&task.user_id));
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
                    chat_type: Some("group".to_owned()),
                    saved_channel_config_id: Some(config_id),
                    sender_kind,
                    is_mentioned: command.is_mentioned,
                    used_plain_text_fallback: command.used_plain_text_fallback,
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
                    .map(str::to_owned)
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
                "direct route auth failed reason='appservice_store_lock_failed' error='{err}'"
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

/// Compatibility routes mounted under `/appservices/matrix/{config_id}/...`.
pub(crate) fn appservice_router() -> Router {
    Router::with_path("appservices/matrix/{config_id}").push(matrix_appservice_router())
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(body) = parse_json_body(req, res, "matrix").await else {
        return;
    };
    debug!(
        event_count = matrix_event_count(&body),
        preview = %matrix_event_preview(&body),
        "Webhook request received"
    );

    let gateway_channel = depot.get_typed::<Arc<GatewayChannel>>().ok().cloned();
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
        if let Ok(channel) = depot.get_typed::<Arc<GatewayChannel>>() {
            let channel = channel.clone();
            for invite in &parsed.rooms_to_auto_join {
                if let Err(err) = channel.auto_join_matrix_invited_room(invite).await {
                    warn!(
                        room_id = %invite.room_id,
                        invited_user_id = invite.invited_user_id.as_deref().unwrap_or(""),
                        error = %err,
                        "Matrix invite auto-join failed"
                    );
                }
            }
        } else {
            warn!("Matrix invite received but gateway channel state is unavailable")
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
        let outbound_config = gateway_channel
            .resolve_matrix_outbound_config(&command.room_id)
            .await
            .ok()
            .flatten();
        // Enforce the per-channel `allowed_senders` allowlist on this public
        // ingest path too — without this the webhook route bypassed the
        // allowlist that user-mode sync and appservice mode enforce.
        if let Some(config) = outbound_config.as_ref()
            && !config.allowed_senders.is_empty()
            && !config
                .allowed_senders
                .iter()
                .any(|allowed| allowed.trim() == command.sender.trim())
        {
            warn!(
                sender = %command.sender,
                room_id = %command.room_id,
                "Matrix webhook sender not in allowed_senders; dropping command"
            );
            res.status_code(StatusCode::OK);
            res.render(Json(json!({ "status": "sender_not_allowed" })));
            return;
        }
        let saved_channel_config_id = outbound_config.map(|config| config.id);
        let sender_kind = matrix_user_mode_sender_kind(&command.sender, self_user_id.as_deref());
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
                    chat_type: Some("group".to_owned()),
                    saved_channel_config_id,
                    sender_kind,
                    is_mentioned: command.is_mentioned,
                    used_plain_text_fallback: command.used_plain_text_fallback,
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
    use salvo::Service;
    use salvo::conn::SocketAddr;
    use salvo::http::Method;
    use salvo::http::uri::{Scheme, Uri};

    use super::*;

    #[test]
    fn matrix_user_mode_sender_kind_classifies_bots_and_self() {
        let me = "@savfox:example.org";
        assert_eq!(
            matrix_user_mode_sender_kind(me, Some(me)),
            runtime::SenderKind::SelfBot
        );
        assert_eq!(
            matrix_user_mode_sender_kind("@weatherbot:example.org", Some(me)),
            runtime::SenderKind::ExternalBot
        );
        assert_eq!(
            matrix_user_mode_sender_kind("@bot_helper:example.org", Some(me)),
            runtime::SenderKind::ExternalBot
        );
        assert_eq!(
            matrix_user_mode_sender_kind("@alice:example.org", Some(me)),
            runtime::SenderKind::Human
        );
        // No self id configured: a normal human is still Human.
        assert_eq!(
            matrix_user_mode_sender_kind("@alice:example.org", None),
            runtime::SenderKind::Human
        );
        // Malformed sender (empty localpart) is Unknown.
        assert_eq!(
            matrix_user_mode_sender_kind("@:example.org", None),
            runtime::SenderKind::Unknown
        );
    }

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

    #[tokio::test]
    async fn matrix_invite_store_tracks_dismiss_and_new_membership_event() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = MatrixInviteStore::new(temp.path().join("matrix-invites.json"));

        let first_invite = MatrixInviteEvent {
            room_id: "!Room:Example.org".to_owned(),
            invited_user_id: Some("@bot:example.org".to_owned()),
            inviter: Some("@alice:example.org".to_owned()),
            room_name: Some("Ops".to_owned()),
            canonical_alias: Some("#ops:example.org".to_owned()),
            membership_event_id: Some("$event-1".to_owned()),
        };
        store
            .upsert(MatrixPendingInvite::from_event("matrix", &first_invite))
            .await
            .expect("upsert first invite");

        let active = store.list(false).await.expect("list active invites");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].room_id, "!Room:Example.org");

        assert!(
            store
                .dismiss("matrix", "!room:example.org")
                .await
                .expect("dismiss invite")
        );
        assert!(store.list(false).await.expect("list active").is_empty());
        let dismissed = store.list(true).await.expect("list dismissed invites");
        assert_eq!(dismissed.len(), 1);
        assert!(dismissed[0].dismissed_at.is_some());

        let renewed_invite = MatrixInviteEvent {
            membership_event_id: Some("$event-2".to_owned()),
            ..first_invite
        };
        store
            .upsert(MatrixPendingInvite::from_event("matrix", &renewed_invite))
            .await
            .expect("upsert renewed invite");

        let active = store.list(false).await.expect("list renewed active");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].membership_event_id.as_deref(), Some("$event-2"));
        assert!(active[0].dismissed_at.is_none());

        assert!(
            store
                .remove("matrix", "!room:example.org")
                .await
                .expect("remove invite")
        );
        assert!(store.list(true).await.expect("list all invites").is_empty());
    }
}
