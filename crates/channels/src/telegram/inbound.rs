use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures_util::StreamExt;
use teloxide::Bot;
use teloxide::prelude::Request;
use teloxide::requests::Requester;
use teloxide::types::AllowedUpdate;
use teloxide::update_listeners::{AsUpdateStream, Polling};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::config::TelegramChannelConfig;

fn telegram_log_preview(text: &str, max_chars: usize) -> String {
    let mut preview = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn describe_update(payload: &serde_json::Value) -> (String, String, usize) {
    let kind = if payload.get("message").is_some() {
        "message"
    } else if payload.get("channel_post").is_some() {
        "channel_post"
    } else if payload.get("callback_query").is_some() {
        "callback_query"
    } else {
        "unknown"
    };
    let message = payload
        .get("message")
        .or_else(|| payload.get("channel_post"))
        .unwrap_or(&serde_json::Value::Null);
    let chat_id = message
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned());
    let text = message
        .get("text")
        .and_then(serde_json::Value::as_str)
        .or_else(|| message.get("caption").and_then(serde_json::Value::as_str))
        .unwrap_or("");
    (kind.to_owned(), chat_id, text.len())
}

#[async_trait]
pub trait TelegramUpdateSink: Send + Sync {
    async fn handle_update(&self, payload: serde_json::Value);
}

#[derive(Debug)]
struct PollingTaskEntry {
    generation: u64,
    handle: JoinHandle<()>,
}

fn polling_handles() -> &'static Mutex<HashMap<String, PollingTaskEntry>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, PollingTaskEntry>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_polling_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

pub async fn start_telegram_polling(
    channel_id: &str,
    config: &TelegramChannelConfig,
    sink: Arc<dyn TelegramUpdateSink>,
) -> anyhow::Result<()> {
    if !config.polling {
        return Ok(());
    }

    let bot_token = config
        .bot_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Telegram polling mode requires bot_token"))?;
    info!(
        target: "savfox::channels::telegram",
        channel_id,
        polling = config.polling,
        ?config.webhook_url,
        "polling start requested"
    );

    let bot = Bot::new(bot_token.to_owned());
    bot.delete_webhook()
        .send()
        .await
        .map_err(|err| anyhow::anyhow!("failed to disable Telegram webhook for polling: {err}"))?;
    info!(
        target: "savfox::channels::telegram",
        channel_id,
        "polling mode enabled; existing webhook cleared"
    );

    let generation = next_polling_generation();
    let tracked_channel_id = channel_id.to_owned();
    let task_channel_id = tracked_channel_id.clone();
    let mut listener = Polling::builder(bot)
        .drop_pending_updates()
        .allowed_updates(vec![
            AllowedUpdate::Message,
            AllowedUpdate::ChannelPost,
            AllowedUpdate::CallbackQuery,
            AllowedUpdate::EditedMessage,
        ])
        .build();
    debug!(
        target: "savfox::channels::telegram",
        channel_id,
        "polling listener built; spawning polling task"
    );
    let handle = tokio::spawn(async move {
        info!(
            target: "savfox::channels::telegram",
            channel_id = %task_channel_id,
            "polling channel starting"
        );

        let stream = listener.as_stream();
        tokio::pin!(stream);
        debug!(
            target: "savfox::channels::telegram",
            channel_id = %task_channel_id,
            "polling stream created; waiting for updates"
        );
        while let Some(update_result) = stream.next().await {
            match update_result {
                Ok(update) => match serde_json::to_value(&update) {
                    Ok(payload) => {
                        let update_id = payload
                            .get("update_id")
                            .and_then(serde_json::Value::as_i64)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_owned());
                        let (kind, chat_id, text_len) = describe_update(&payload);
                        debug!(
                            target: "savfox::channels::telegram",
                            channel_id = %task_channel_id,
                            update_id,
                            kind,
                            chat_id,
                            text_len,
                            payload_preview = %telegram_log_preview(&payload.to_string(), 220),
                            "polling update received"
                        );
                        sink.handle_update(payload).await;
                    }
                    Err(err) => {
                        warn!(
                            target: "savfox::channels::telegram",
                            channel_id = %task_channel_id,
                            error = %err,
                            "polling update serialization failed"
                        );
                    }
                },
                Err(err) => {
                    warn!(
                        target: "savfox::channels::telegram",
                        channel_id = %task_channel_id,
                        error = %err,
                        "polling listener error"
                    );
                }
            }
        }

        info!(
            target: "savfox::channels::telegram",
            channel_id = %task_channel_id,
            "polling channel stopped"
        );
        let mut handles = polling_handles().lock().await;
        if handles
            .get(&tracked_channel_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            handles.remove(&tracked_channel_id);
        }
    });

    let mut handles = polling_handles().lock().await;
    if let Some(previous) = handles.insert(
        channel_id.to_owned(),
        PollingTaskEntry { generation, handle },
    ) {
        info!(
            target: "savfox::channels::telegram",
            channel_id,
            "replacing existing polling task"
        );
        previous.handle.abort();
    }
    Ok(())
}

pub async fn stop_telegram_polling(channel_id: &str) -> bool {
    let mut handles = polling_handles().lock().await;
    if let Some(entry) = handles.remove(channel_id) {
        info!(
            target: "savfox::channels::telegram",
            channel_id,
            "stopping polling task"
        );
        entry.handle.abort();
        true
    } else {
        debug!(
            target: "savfox::channels::telegram",
            channel_id,
            "stop polling requested but no task was running"
        );
        false
    }
}

pub async fn is_telegram_polling_running(channel_id: &str) -> bool {
    let mut handles = polling_handles().lock().await;
    if handles
        .get(channel_id)
        .is_some_and(|entry| entry.handle.is_finished())
    {
        handles.remove(channel_id);
        return false;
    }
    handles.contains_key(channel_id)
}
