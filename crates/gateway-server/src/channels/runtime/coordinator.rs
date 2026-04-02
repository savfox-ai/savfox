//! Per-session coordinator for fine-grained concurrency control.
//!
//! Instead of spawning fully independent tasks per webhook, messages for the
//! same logical session are routed through a lightweight coordinator actor.
//! The coordinator uses `tokio::select!` to monitor both the running agent
//! and the inbox, enabling queue-mode behaviours inspired by crew-rs:
//!
//! - **Speculative** (default): spawn a concurrent overflow task for the new message while the
//!   primary continues, delivering both results.
//! - **Interrupt**: cancel the running primary and start the new message.
//! - **Followup**: queue the new message and process it after the primary.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::debug;

use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// How to handle new messages that arrive while the agent is running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum QueueMode {
    /// Spawn a concurrent overflow task. Both results are delivered.
    #[default]
    Speculative,
    /// Cancel the running primary and process the new message immediately.
    Interrupt,
    /// Queue the new message and process it after the primary finishes.
    Followup,
}

/// A pending inbound message to be processed by the coordinator.
pub(crate) struct InboundTask {
    pub platform: &'static str,
    pub channel_id: String,
    pub prompt: String,
    pub name: Option<String>,
    pub meta: Option<super::StartThreadMeta>,
    pub gateway_channel: Arc<GatewayChannel>,
    pub session_store: Arc<SessionStore>,
}

type CoordinatorMap = HashMap<String, mpsc::Sender<InboundTask>>;

fn coordinators() -> &'static tokio::sync::Mutex<CoordinatorMap> {
    static MAP: OnceLock<tokio::sync::Mutex<CoordinatorMap>> = OnceLock::new();
    MAP.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// How long a coordinator waits for a new message before shutting down.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Dispatch a task to the per-session coordinator, creating one if necessary.
pub(crate) async fn dispatch(session_key: String, task: InboundTask) {
    let mut map = coordinators().lock().await;

    // Try existing coordinator.
    if let Some(tx) = map.get(&session_key) {
        match tx.send(task).await {
            Ok(()) => return,
            Err(mpsc::error::SendError(returned_task)) => {
                // Coordinator has exited – remove stale entry and re-send below.
                map.remove(&session_key);
                // Create new coordinator with the returned task.
                let (tx, rx) = mpsc::channel::<InboundTask>(32);
                let key = session_key.clone();
                tokio::spawn(run_coordinator(key, rx));
                let _ = tx.send(returned_task).await;
                map.insert(session_key, tx);
                return;
            }
        }
    }

    let (tx, rx) = mpsc::channel::<InboundTask>(32);
    let key = session_key.clone();
    tokio::spawn(run_coordinator(key, rx));
    let _ = tx.send(task).await;
    map.insert(session_key, tx);
}

/// Remove a coordinator entry (called when the actor exits).
async fn remove_coordinator(key: &str) {
    coordinators().lock().await.remove(key);
}

/// The coordinator actor: processes one primary task at a time while monitoring
/// the inbox for concurrent arrivals.
async fn run_coordinator(session_key: String, mut inbox: mpsc::Receiver<InboundTask>) {
    loop {
        // Wait for the next task, with idle timeout.
        let task = match tokio::time::timeout(IDLE_TIMEOUT, inbox.recv()).await {
            Ok(Some(task)) => task,
            _ => break, // timeout or channel closed
        };

        // Spawn the primary agent processing as a background task.
        let primary_gw = Arc::clone(&task.gateway_channel);
        let primary_store = Arc::clone(&task.session_store);
        let primary_platform = task.platform;
        let primary_channel = task.channel_id.clone();
        let primary_prompt = task.prompt.clone();
        let primary_name = task.name.clone();
        let primary_meta = task.meta.clone();
        let primary_outbound = format!("{}:{}", task.platform, task.channel_id);

        let (primary_done_tx, mut primary_done_rx) = mpsc::channel::<bool>(1);

        let primary_handle = tokio::spawn(async move {
            super::spawn_start_thread_pipeline_with_meta(
                primary_gw,
                primary_store,
                primary_platform,
                primary_channel,
                primary_prompt,
                primary_name,
                primary_meta,
            )
            .await;
            let _ = primary_done_tx.send(true).await;
        });

        let mut overflow_served = false;

        // Select loop: wait for primary completion OR new inbox message.
        loop {
            tokio::select! {
                _ = primary_done_rx.recv() => {
                    // Primary completed normally.
                    break;
                }
                next = inbox.recv() => {
                    if let Some(overflow_task) = next {
                        // New message while primary is running → Speculative mode:
                        // spawn an independent pipeline for the overflow message.
                        overflow_served = true;
                        let ogw = Arc::clone(&overflow_task.gateway_channel);
                        let oss = Arc::clone(&overflow_task.session_store);
                        let op = overflow_task.platform;
                        let oc = overflow_task.channel_id;
                        let oprompt = overflow_task.prompt;
                        let oname = overflow_task.name;
                        let ometa = overflow_task.meta;
                        tokio::spawn(async move {
                            super::spawn_start_thread_pipeline_with_meta(
                                ogw,
                                oss,
                                op,
                                oc,
                                oprompt,
                                oname,
                                ometa,
                            )
                            .await;
                        });
                    } else {
                        // Inbox closed, wait for primary then exit.
                        let _ = primary_handle.await;
                        break;
                    }
                }
            }
        }

        if overflow_served {
            // In crew-rs style, the primary response would be prefixed with
            // "⬆️" to indicate it was the earlier task. Currently the prefix
            // is applied inside the pipeline via send_with_retry, so this is
            // a future enhancement point.  For now we just log.
            debug!(
                session_key = %session_key,
                outbound = %primary_outbound,
                "Overflow was served during primary"
            );
        }
    }

    remove_coordinator(&session_key).await;
}
