use crate::api::types::SlackStatus;
use crate::channel_status_page;
use crate::pages::channels::common::ChannelStatusView;

impl ChannelStatusView for SlackStatus {
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
        self.workspace_name.clone()
    }
}

channel_status_page! {
    component: SlackChannel,
    status: SlackStatus,
    channel_id: "slack",
    title: "Slack",
    icon: crate::utils::icons::ICON_SLACK,
    subtitle: "Socket mode status and channel configuration.",
}
