use crate::api::types::IrcStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for IrcStatus {
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
        self.nickname.clone()
    }
    fn extra_stats(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(s) = &self.server {
            v.push(("Server".into(), s.clone()));
        }
        if let Some(c) = self.channel_count {
            v.push(("Channels".into(), c.to_string()));
        }
        v
    }
}

channel_status_page! {
    component: IrcChannel,
    status: IrcStatus,
    channel_id: "irc",
    title: "IRC",
    icon: crate::utils::icons::ICON_IRC,
    subtitle: "IRC server status and channel configuration.",
}
