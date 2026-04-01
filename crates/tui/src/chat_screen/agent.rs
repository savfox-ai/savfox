use std::sync::Arc;

use savfox_core::config::Config;
use savfox_core::protocol::{Event, EventMsg, Op};
use savfox_core::{NewSession, SavfoxSession, SessionManager};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

/// Spawn the agent bootstrapper and op forwarding loop, returning the
/// `UnboundedSender<Op>` used by the UI to submit operations.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<SessionManager>,
) -> UnboundedSender<Op> {
    let (savfox_op_tx, mut savfox_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        let Some(first_op) = savfox_op_rx.recv().await else {
            return;
        };

        if matches!(first_op, Op::Shutdown) {
            app_event_tx_clone.send(AppEvent::SavfoxEvent(Event {
                id: "".to_string(),
                msg: EventMsg::ShutdownComplete,
            }));
            return;
        }

        let NewSession {
            session,
            session_configured,
            ..
        } = match server.start_session(config).await {
            Ok(v) => v,
            Err(err) => {
                let message = format!("Failed to initialize savfox: {err}");
                tracing::error!("{message}");
                app_event_tx_clone.send(AppEvent::SavfoxEvent(Event {
                    id: "".to_string(),
                    msg: EventMsg::Error(err.to_error_event(None)),
                }));
                app_event_tx_clone.send(AppEvent::FatalExitRequest(message));
                tracing::error!("failed to initialize savfox: {err}");
                return;
            }
        };

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = savfox_core::protocol::Event {
            // The `id` does not matter for rendering, so we can use a fake value.
            id: "".to_string(),
            msg: savfox_core::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::SavfoxEvent(ev));

        if let Err(e) = session.submit(first_op).await {
            tracing::error!("failed to submit op: {e}");
        }

        let session_clone = session.clone();
        tokio::spawn(async move {
            while let Some(op) = savfox_op_rx.recv().await {
                let id = session_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        while let Ok(event) = session.next_event().await {
            app_event_tx_clone.send(AppEvent::SavfoxEvent(event));
        }
    });

    savfox_op_tx
}

/// Spawn agent loops for an existing session (e.g., a forked session).
/// Sends the provided `SessionConfiguredEvent` immediately, then forwards subsequent
/// events and accepts Ops for submission.
pub(crate) fn spawn_agent_from_existing(
    session: std::sync::Arc<SavfoxSession>,
    session_configured: savfox_core::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> UnboundedSender<Op> {
    let (savfox_op_tx, mut savfox_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = savfox_core::protocol::Event {
            id: "".to_string(),
            msg: savfox_core::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::SavfoxEvent(ev));

        let session_clone = session.clone();
        tokio::spawn(async move {
            while let Some(op) = savfox_op_rx.recv().await {
                let id = session_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        while let Ok(event) = session.next_event().await {
            app_event_tx_clone.send(AppEvent::SavfoxEvent(event));
        }
    });

    savfox_op_tx
}

/// Spawn an op-forwarding loop for an existing session without subscribing to events.
pub(crate) fn spawn_op_forwarder(session: std::sync::Arc<SavfoxSession>) -> UnboundedSender<Op> {
    let (savfox_op_tx, mut savfox_op_rx) = unbounded_channel::<Op>();

    tokio::spawn(async move {
        while let Some(op) = savfox_op_rx.recv().await {
            if let Err(e) = session.submit(op).await {
                tracing::error!("failed to submit op: {e}");
            }
        }
    });

    savfox_op_tx
}
