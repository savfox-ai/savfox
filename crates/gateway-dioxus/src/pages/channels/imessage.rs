use crate::api::types::IMessageStatus;
use crate::channel_status_page;
use crate::pages::channels::common::ChannelStatusView;

impl ChannelStatusView for IMessageStatus {
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
    component: IMessageChannel,
    status: IMessageStatus,
    channel_id: "imessage",
    title: "iMessage",
    icon: crate::utils::icons::ICON_MESSAGE_SQUARE,
    subtitle: "macOS channel status and channel configuration.",
}
