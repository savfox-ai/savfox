use crate::api::types::GoogleChatStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for GoogleChatStatus {
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
}

channel_status_page! {
    component: GoogleChatChannel,
    status: GoogleChatStatus,
    channel_id: "googlechat",
    title: "Google Chat",
    icon: crate::utils::icons::ICON_GOOGLE_CHAT,
    subtitle: "Chat API webhook status and channel configuration.",
}
