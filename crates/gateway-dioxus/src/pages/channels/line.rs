use crate::api::types::LineStatus;
use crate::channel_status_page;
use crate::pages::channels::common::ChannelStatusView;

impl ChannelStatusView for LineStatus {
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
        self.bot_name.clone()
    }
}

channel_status_page! {
    component: LineChannel,
    status: LineStatus,
    channel_id: "line",
    title: "Line",
    icon: crate::utils::icons::ICON_LINE,
    subtitle: "Bot status and channel configuration.",
}
