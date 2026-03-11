use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::{Value, json};
use serenity::all::{
    Client as SerenityClient, Context as SerenityContext, EventHandler as SerenityEventHandler,
    GatewayIntents, Message as SerenityMessage, Ready,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::config::DiscordChannelConfig;

#[derive(Debug, Clone)]
pub struct DiscordInboundMessage {
    pub payload: Value,
    pub bot_user_id: String,
    pub bot_username: String,
}

#[async_trait]
pub trait DiscordEventSink: Send + Sync {
    async fn handle_message(&self, event: DiscordInboundMessage);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordStreamRuntimeState {
    pub connected: bool,
    pub bot_user_id: Option<String>,
    pub bot_username: Option<String>,
    pub guild_count: Option<u32>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct DiscordStreamTaskEntry {
    generation: u64,
    handle: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct DiscordReadyState {
    bot_user_id: String,
    bot_username: String,
    guild_count: u32,
}

#[derive(Debug, Clone)]
struct DiscordBotIdentity {
    user_id: String,
    username: String,
}

struct DiscordGatewayHandler {
    sink: Arc<dyn DiscordEventSink>,
    channel_id: String,
    guild_id_filter: Option<String>,
    ready_sender: tokio::sync::Mutex<Option<oneshot::Sender<DiscordReadyState>>>,
    bot_identity: tokio::sync::RwLock<Option<DiscordBotIdentity>>,
}

fn stream_handles() -> &'static Mutex<HashMap<String, DiscordStreamTaskEntry>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, DiscordStreamTaskEntry>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime_state_store() -> &'static Mutex<HashMap<String, DiscordStreamRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, DiscordStreamRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_stream_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn set_runtime_state(channel_id: &str, state: DiscordStreamRuntimeState) {
    if let Ok(mut store) = runtime_state_store().lock() {
        store.insert(channel_id.to_string(), state);
    }
}

fn update_runtime_state(channel_id: &str, apply: impl FnOnce(&mut DiscordStreamRuntimeState)) {
    if let Ok(mut store) = runtime_state_store().lock() {
        apply(store.entry(channel_id.to_string()).or_default());
    }
}

pub fn discord_stream_state_snapshot() -> HashMap<String, DiscordStreamRuntimeState> {
    runtime_state_store()
        .lock()
        .map(|store| store.clone())
        .unwrap_or_default()
}

pub fn discord_stream_state_for(channel_id: &str) -> Option<DiscordStreamRuntimeState> {
    runtime_state_store()
        .lock()
        .ok()
        .and_then(|store| store.get(channel_id).cloned())
}

fn serialize_message_payload(message: &SerenityMessage) -> Value {
    let message_reference = message.message_reference.as_ref().map(|reference| {
        json!({
            "message_id": reference.message_id.map(|id| id.get().to_string()),
            "channel_id": reference.channel_id.get().to_string(),
            "guild_id": reference.guild_id.map(|id| id.get().to_string()),
        })
    });
    let referenced_message = message.referenced_message.as_ref().map(|referenced| {
        json!({
            "id": referenced.id.get().to_string(),
            "channel_id": referenced.channel_id.get().to_string(),
            "author": {
                "id": referenced.author.id.get().to_string(),
                "username": referenced.author.name,
                "global_name": referenced.author.global_name,
                "bot": referenced.author.bot,
            }
        })
    });
    let member = message.member.as_ref().map(|member| {
        json!({
            "nick": member.nick,
            "roles": member
                .roles
                .iter()
                .map(|role_id| role_id.get().to_string())
                .collect::<Vec<_>>(),
        })
    });

    json!({
        "id": message.id.get().to_string(),
        "channel_id": message.channel_id.get().to_string(),
        "guild_id": message.guild_id.map(|id| id.get().to_string()),
        "content": message.content,
        "author": {
            "id": message.author.id.get().to_string(),
            "username": message.author.name,
            "global_name": message.author.global_name,
            "bot": message.author.bot,
        },
        "member": member,
        "message_reference": message_reference,
        "referenced_message": referenced_message,
        "mentions": message
            .mentions
            .iter()
            .map(|user| {
                json!({
                    "id": user.id.get().to_string(),
                    "username": user.name,
                    "global_name": user.global_name,
                    "bot": user.bot,
                })
            })
            .collect::<Vec<_>>(),
        "mention_roles": message
            .mention_roles
            .iter()
            .map(|role_id| role_id.get().to_string())
            .collect::<Vec<_>>(),
        "webhook_id": message.webhook_id.map(|id| id.get().to_string()),
    })
}

#[serenity::async_trait]
impl SerenityEventHandler for DiscordGatewayHandler {
    async fn ready(&self, _ctx: SerenityContext, ready: Ready) {
        let state = DiscordReadyState {
            bot_user_id: ready.user.id.get().to_string(),
            bot_username: ready.user.name.clone(),
            guild_count: ready.guilds.len() as u32,
        };
        *self.bot_identity.write().await = Some(DiscordBotIdentity {
            user_id: state.bot_user_id.clone(),
            username: state.bot_username.clone(),
        });
        if let Some(sender) = self.ready_sender.lock().await.take() {
            let _ = sender.send(state.clone());
        }
        info!(
            channel_id = %self.channel_id,
            bot_user_id = %state.bot_user_id,
            bot_username = %state.bot_username,
            guild_count = state.guild_count,
            "Discord gateway ready"
        );
    }

    async fn message(&self, _ctx: SerenityContext, message: SerenityMessage) {
        if message.author.bot || message.webhook_id.is_some() {
            return;
        }

        if let Some(expected_guild_id) = self.guild_id_filter.as_deref()
            && let Some(actual_guild_id) = message.guild_id
            && actual_guild_id.get().to_string() != expected_guild_id
        {
            return;
        }

        let Some(bot_identity) = self.bot_identity.read().await.clone() else {
            return;
        };
        let payload = serialize_message_payload(&message);
        self.sink
            .handle_message(DiscordInboundMessage {
                payload,
                bot_user_id: bot_identity.user_id,
                bot_username: bot_identity.username,
            })
            .await;
    }
}

pub async fn start_discord_stream(
    channel_id: &str,
    config: &DiscordChannelConfig,
    sink: Arc<dyn DiscordEventSink>,
) -> anyhow::Result<()> {
    if !config.stream_enabled() {
        return Ok(());
    }

    let bot_token = config
        .bot_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Discord stream mode requires bot_token"))?;

    set_runtime_state(
        channel_id,
        DiscordStreamRuntimeState {
            connected: false,
            bot_user_id: None,
            bot_username: None,
            guild_count: None,
            last_error: None,
        },
    );

    let generation = next_stream_generation();
    let tracked_channel_id = channel_id.to_string();
    let cleanup_channel_id = tracked_channel_id.clone();
    let guild_id_filter = config.guild_id.clone();
    let (ready_tx, ready_rx) = oneshot::channel::<DiscordReadyState>();
    let handler = DiscordGatewayHandler {
        sink,
        channel_id: tracked_channel_id.clone(),
        guild_id_filter,
        ready_sender: tokio::sync::Mutex::new(Some(ready_tx)),
        bot_identity: tokio::sync::RwLock::new(None),
    };
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let mut gateway_client = SerenityClient::builder(bot_token.to_string(), intents)
        .event_handler(handler)
        .await
        .map_err(|err| anyhow!("failed to build Discord gateway client: {err}"))?;

    let handle = tokio::spawn(async move {
        if let Err(err) = gateway_client.start_autosharded().await {
            update_runtime_state(&cleanup_channel_id, |state| {
                state.connected = false;
                state.last_error = Some(err.to_string());
            });
            warn!(
                channel_id = %cleanup_channel_id,
                error = %err,
                "Discord gateway stream stopped with error"
            );
        } else {
            update_runtime_state(&cleanup_channel_id, |state| {
                state.connected = false;
            });
            info!(channel_id = %cleanup_channel_id, "Discord gateway stream stopped");
        }

        if let Ok(mut handles) = stream_handles().lock()
            && handles
                .get(&cleanup_channel_id)
                .is_some_and(|entry| entry.generation == generation)
        {
            handles.remove(&cleanup_channel_id);
        }
    });

    let ready = match tokio::time::timeout(Duration::from_secs(30), ready_rx).await {
        Ok(Ok(ready)) => ready,
        Ok(Err(_)) => {
            handle.abort();
            update_runtime_state(channel_id, |state| {
                state.connected = false;
                state.last_error =
                    Some("discord gateway exited before the Ready event".to_string());
            });
            return Err(anyhow!("discord gateway exited before the Ready event"));
        }
        Err(_) => {
            handle.abort();
            update_runtime_state(channel_id, |state| {
                state.connected = false;
                state.last_error =
                    Some("timed out waiting for the Discord gateway Ready event".to_string());
            });
            return Err(anyhow!(
                "timed out waiting for the Discord gateway Ready event"
            ));
        }
    };

    update_runtime_state(channel_id, |state| {
        state.connected = true;
        state.bot_user_id = Some(ready.bot_user_id.clone());
        state.bot_username = Some(ready.bot_username.clone());
        state.guild_count = Some(ready.guild_count);
        state.last_error = None;
    });

    if let Ok(mut handles) = stream_handles().lock() {
        if let Some(previous) = handles.insert(
            channel_id.to_string(),
            DiscordStreamTaskEntry { generation, handle },
        ) {
            previous.handle.abort();
        }
    }

    info!(channel_id = %channel_id, "Discord gateway stream started");
    Ok(())
}

pub async fn stop_discord_stream(channel_id: &str) -> bool {
    let entry = stream_handles()
        .lock()
        .ok()
        .and_then(|mut handles| handles.remove(channel_id));
    if let Some(entry) = entry {
        entry.handle.abort();
        update_runtime_state(channel_id, |state| {
            state.connected = false;
        });
        true
    } else {
        false
    }
}

pub async fn is_discord_stream_running(channel_id: &str) -> bool {
    stream_handles()
        .lock()
        .map(|handles| handles.contains_key(channel_id))
        .unwrap_or(false)
}
