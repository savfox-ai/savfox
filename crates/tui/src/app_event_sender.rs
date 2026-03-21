use tokio::sync::mpsc::UnboundedSender;

use crate::app_event::AppEvent;
use crate::history_cell;
use crate::session_log;

#[derive(Clone, Debug)]
pub(crate) struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
}

impl AppEventSender {
    pub(crate) fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self { app_event_tx }
    }

    /// Send an event to the app event channel. If it fails, we swallow the
    /// error and log it.
    pub(crate) fn send(&self, event: AppEvent) {
        // Record inbound events for high-fidelity session replay.
        // Avoid double-logging Ops; those are logged at the point of submission.
        if !matches!(event, AppEvent::SavfoxOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }

    #[allow(dead_code)]
    /// Log an error and display it as a visible error cell in the transcript
    /// so the user can see operational failures instead of having them silently
    /// swallowed.
    pub(crate) fn send_visible_error(&self, message: impl Into<String>) {
        let msg = message.into();
        tracing::error!("{msg}");
        let cell = history_cell::new_error_event(msg);
        self.send(AppEvent::InsertHistoryCell(Box::new(cell)));
    }
}
