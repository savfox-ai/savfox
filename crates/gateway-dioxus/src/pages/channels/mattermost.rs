use crate::api::types::MattermostStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for MattermostStatus {
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
        self.bot_username.clone()
    }
    fn extra_stats(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(s) = &self.server_url {
            v.push(("Server URL".into(), s.clone()));
        }
        if let Some(t) = &self.team_name {
            v.push(("Team".into(), t.clone()));
        }
        v
    }
}

channel_status_page! {
    component: MattermostChannel,
    status: MattermostStatus,
    channel_id: "mattermost",
    title: "Mattermost",
    icon: crate::utils::icons::ICON_MATTERMOST,
    subtitle: "Bot status and channel configuration.",
}
