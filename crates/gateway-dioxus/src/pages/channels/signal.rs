use crate::api::types::SignalStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for SignalStatus {
    fn configured(&self) -> Option<bool> {
        self.configured
    }
    fn running(&self) -> Option<bool> {
        self.running
    }
    fn connected(&self) -> Option<bool> {
        self.connected
    }
    fn last_error(&self) -> Option<String> {
        self.last_error.clone()
    }
    fn display_name(&self) -> Option<String> {
        self.phone_number.clone()
    }
}

channel_status_page! {
    component: SignalChannel,
    status: SignalStatus,
    channel_id: "signal",
    title: "Signal",
    icon: crate::utils::icons::ICON_SIGNAL,
    subtitle: "signal-cli status and channel configuration.",
}
