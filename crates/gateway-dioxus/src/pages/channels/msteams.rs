use crate::api::types::MsTeamsStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for MsTeamsStatus {
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
    fn extra_stats(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(t) = &self.tenant_id {
            v.push(("Tenant ID".into(), t.clone()));
        }
        v
    }
}

channel_status_page! {
    component: MsTeamsChannel,
    status: MsTeamsStatus,
    channel_id: "msteams",
    title: "Microsoft Teams",
    icon: crate::utils::icons::ICON_MSTEAMS,
    subtitle: "Bot status and channel configuration.",
}
