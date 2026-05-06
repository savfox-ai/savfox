use crate::api::types::DiscordStatus;
use crate::channel_status_page;
use crate::pages::channels::common::ChannelStatusView;

impl ChannelStatusView for DiscordStatus {
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
        if let Some(c) = self.guild_count {
            v.push(("Servers".into(), c.to_string()));
        }
        v
    }
}

channel_status_page! {
    component: DiscordChannel,
    status: DiscordStatus,
    channel_id: "discord",
    title: "Discord",
    icon: crate::utils::icons::ICON_DISCORD,
    subtitle: "Bot status and channel configuration.",
}
