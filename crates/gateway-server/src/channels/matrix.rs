use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use form_urlencoded::Serializer;
use matrix_bot_sdk::client::MatrixClient;
use reqwest::Method;
use salvo::prelude::*;
use savfox_channels::matrix::{
    MatrixCommandEvent, parse_inbound_payload_for_user, parse_webhook_payload_for_user,
};
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::runtime;
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

#[derive(Debug, Clone, Default)]
pub(crate) struct MatrixRuntimeState {
    pub homeserver: Option<String>,
    pub user_id: Option<String>,
    pub access_token: Option<String>,
    pub connected: bool,
    pub room_count: Option<u32>,
    pub last_error: Option<String>,
}

fn matrix_runtime_state_store() -> &'static Mutex<HashMap<String, MatrixRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, MatrixRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config_id: String,
        homeserver: String,
        user_id: Option<String>,
        access_token: Option<String>,
        password: Option<String>,
        device_name: Option<String>,
        gateway_channel: Arc<GatewayChannel>,
        session_store: Arc<SessionStore>,
    ) -> Self {
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
        debug_matrix_sync_payload(&self.config_id, "initial", &initial_payload);

        let initial_batch =
            parse_inbound_payload_for_user(&initial_payload, Some(&resolved.user_id));
        let next_batch = initial_payload
            .get("next_batch")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        debug_matrix_sync_batch(
            &self.config_id,
            "initial",
            initial_batch.rooms_to_auto_join.len(),
            initial_batch.commands.len(),
            next_batch.as_deref(),
        );

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

        if !initial_batch.commands.is_empty() {
            println!(
                "[matrix][sync] config={} skipped_initial_command_backlog={}",
                self.config_id,
                initial_batch.commands.len()
            );
        }

        let room_count = resolved
            .client
            .get_joined_rooms()
            .await
            .ok()
            .map(|rooms| rooms.len() as u32);
        set_matrix_runtime_state(
            &self.config_id,
            MatrixRuntimeState {
                homeserver: Some(self.homeserver.clone()),
                user_id: Some(resolved.user_id.clone()),
                access_token: Some(resolved.access_token.clone()),
                connected: true,
                room_count,
                last_error: None,
            },
        );

        if let Ok(mut client) = self.client.lock() {
            *client = Some(resolved.client.clone());
        }

        let sync_task = MatrixSyncTask {
            config_id: self.config_id.clone(),
            homeserver: self.homeserver.clone(),
            user_id: resolved.user_id.clone(),
            client: resolved.client.clone(),
            gateway_channel: Arc::clone(&self.gateway_channel),
            session_store: Arc::clone(&self.session_store),
        };
        let handle = tokio::spawn(run_matrix_sync_task(sync_task, next_batch));
        if let Ok(mut task) = self.sync_task.lock() {
            *task = Some(handle);
        }

        println!(
            "[matrix][client] started config={} homeserver={} user_id={}",
            self.config_id, self.homeserver, resolved.user_id
        );
        info!(
            config_id = %self.config_id,
            homeserver = %self.homeserver,
            user_id = %resolved.user_id,
            "Matrix client started"
        );
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
        let text = render_matrix_rich_text(&msg);
        let html = render_matrix_rich_html(&msg);
        client
            .send_html_text(channel, &text, &html)
            .await
            .with_context(|| format!("failed to send Matrix rich message to room {channel}"))?;
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(parse_webhook_payload_for_user(&payload, None).action)
    }
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
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

#[allow(clippy::print_stdout)]
fn debug_matrix_sync_payload(config_id: &str, phase: &str, payload: &Value) {
    let rendered = serde_json::to_string(payload)
        .unwrap_or_else(|err| format!(r#"{{"serialize_error":"{err}"}}"#));
    println!("[matrix][sync] config={config_id} phase={phase} payload={rendered}");
}

#[allow(clippy::print_stdout)]
fn debug_matrix_sync_batch(
    config_id: &str,
    phase: &str,
    invite_count: usize,
    command_count: usize,
    next_batch: Option<&str>,
) {
    println!(
        "[matrix][sync] config={} phase={} invites={} commands={} next_batch={}",
        config_id,
        phase,
        invite_count,
        command_count,
        next_batch.unwrap_or("")
    );
}

async fn handle_matrix_invites(task: &MatrixSyncTask, invites: Vec<(String, Option<String>)>) {
    for (room_id, invited_user_id) in invites {
        if let Some(invited_user_id) = invited_user_id.as_deref().and_then(|value| {
            let value = value.trim();
            if value.is_empty() { None } else { Some(value) }
        }) && !invited_user_id.eq_ignore_ascii_case(&task.user_id)
        {
            println!(
                "[matrix][invite] ignored config={} room={} invited_user={} self_user={}",
                task.config_id, room_id, invited_user_id, task.user_id
            );
            continue;
        }

        println!(
            "[matrix][invite] auto-join attempt config={} room={} homeserver={}",
            task.config_id, room_id, task.homeserver
        );
        match task.client.join_room(&room_id).await {
            Ok(joined_room_id) => {
                println!(
                    "[matrix][invite] auto-join ok config={} room={} joined_room_id={}",
                    task.config_id, room_id, joined_room_id
                );
                if let Ok(joined_rooms) = task.client.get_joined_rooms().await {
                    update_matrix_runtime_state(&task.config_id, |state| {
                        state.room_count = Some(joined_rooms.len() as u32);
                    });
                }
            }
            Err(err) => {
                println!(
                    "[matrix][invite] auto-join failed config={} room={} error={}",
                    task.config_id, room_id, err
                );
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
            println!(
                "[matrix][command] ignored_self_message config={} room={} sender={}",
                task.config_id, command.room_id, command.sender
            );
            continue;
        }

        if runtime::should_drop_duplicate(command.dedupe_key.clone()).await {
            println!(
                "[matrix][command] duplicate_ignored config={} room={} sender={} dedupe_key={}",
                task.config_id,
                command.room_id,
                command.sender,
                command.dedupe_key.as_deref().unwrap_or("")
            );
            continue;
        }

        println!(
            "[matrix][command] dispatch config={} room={} sender={} prompt={}",
            task.config_id, command.room_id, command.sender, command.prompt
        );

        let gateway_channel = Arc::clone(&task.gateway_channel);
        let session_store = Arc::clone(&task.session_store);
        let room_id = command.room_id;
        let sender = command.sender;
        let prompt = command.prompt;
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta(
                gateway_channel,
                session_store,
                "matrix",
                room_id.clone(),
                prompt,
                Some(sender.clone()),
                Some(runtime::StartThreadMeta {
                    peer_id: Some(sender),
                    group_id: Some(room_id),
                    chat_type: Some("group".to_string()),
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
                debug_matrix_sync_payload(&task.config_id, "incremental", &payload);
                if let Some(next_batch) = payload
                    .get("next_batch")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                {
                    since = Some(next_batch);
                }

                let parsed = parse_inbound_payload_for_user(&payload, Some(&task.user_id));
                debug_matrix_sync_batch(
                    &task.config_id,
                    "incremental",
                    parsed.rooms_to_auto_join.len(),
                    parsed.commands.len(),
                    since.as_deref(),
                );
                update_matrix_runtime_state(&task.config_id, |state| {
                    state.connected = true;
                    state.last_error = None;
                });

                handle_matrix_invites(&task, parsed.rooms_to_auto_join).await;
                dispatch_matrix_commands(&task, parsed.commands).await;
                backoff = Duration::from_secs(1);
            }
            Err(err) => {
                println!(
                    "[matrix][sync] error config={} homeserver={} error={}",
                    task.config_id, task.homeserver, err
                );
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

#[allow(clippy::print_stdout)]
fn debug_matrix_webhook_payload(body: &Value) {
    let rendered = serde_json::to_string(body)
        .unwrap_or_else(|err| format!(r#"{{"serialize_error":"{err}"}}"#));
    println!("[matrix][webhook] received payload={rendered}");
}

#[allow(clippy::print_stdout)]
fn debug_matrix_webhook_summary(
    invite_count: usize,
    dedupe_key: Option<&str>,
    action: &ChannelAction,
) {
    let action_summary = match action {
        ChannelAction::Ignore => "ignore".to_string(),
        ChannelAction::StartThread { channel, prompt } => {
            format!("start_thread room={channel} prompt={prompt}")
        }
        other => format!("{other:?}"),
    };
    println!(
        "[matrix][webhook] parsed invites={} dedupe_key={} action={}",
        invite_count,
        dedupe_key.unwrap_or(""),
        action_summary
    );
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Json(json!({
        "error": {
            "code": code,
            "message": message.into(),
        }
    })));
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

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<Value>().await {
        Ok(body) => body,
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("invalid JSON: {err}"),
            );
            return;
        }
    };
    debug_matrix_webhook_payload(&body);

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

    let parsed = parse_webhook_payload_for_user(&body, self_user_id.as_deref());
    debug_matrix_webhook_summary(
        parsed.rooms_to_auto_join.len(),
        parsed.dedupe_key.as_deref(),
        &parsed.action,
    );

    if !parsed.rooms_to_auto_join.is_empty() {
        match depot.obtain::<Arc<GatewayChannel>>() {
            Ok(channel) => {
                let channel = channel.clone();
                for (room_id, invited_user_id) in parsed.rooms_to_auto_join {
                    println!(
                        "[matrix][invite] auto-join attempt room={} invited_user={}",
                        room_id,
                        invited_user_id.as_deref().unwrap_or("")
                    );
                    if let Err(err) = channel
                        .auto_join_matrix_invited_room(&room_id, invited_user_id.as_deref())
                        .await
                    {
                        println!(
                            "[matrix][invite] auto-join failed room={} invited_user={} error={}",
                            room_id,
                            invited_user_id.as_deref().unwrap_or(""),
                            err
                        );
                        warn!(
                            room_id,
                            invited_user_id = invited_user_id.as_deref().unwrap_or(""),
                            error = %err,
                            "Matrix invite auto-join failed"
                        );
                    } else {
                        println!(
                            "[matrix][invite] auto-join ok room={} invited_user={}",
                            room_id,
                            invited_user_id.as_deref().unwrap_or("")
                        );
                    }
                }
            }
            Err(_) => {
                println!(
                    "[matrix][invite] gateway channel state unavailable while handling invite"
                );
                warn!("Matrix invite received but gateway channel state is unavailable");
            }
        }
    }

    if runtime::should_drop_duplicate(parsed.dedupe_key).await {
        println!("[matrix][webhook] duplicate event ignored");
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let ChannelAction::StartThread {
        channel: channel_id,
        prompt,
    } = parsed.action
    {
        let gateway_channel = match gateway_channel {
            Some(channel) => channel,
            None => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "gateway channel state unavailable",
                );
                return;
            }
        };
        let session_store = match depot.obtain::<Arc<SessionStore>>() {
            Ok(store) => store.clone(),
            Err(_) => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "session store state unavailable",
                );
                return;
            }
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                gateway_channel,
                session_store,
                "matrix",
                channel_id,
                prompt,
                None,
            )
            .await;
        });
    }

    println!("[matrix][webhook] request handled");
    info!("Matrix webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
