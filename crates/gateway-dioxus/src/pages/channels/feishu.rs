use crate::api::types::FeishuStatus;
use crate::channel_status_page;
use crate::pages::channels::common::ChannelStatusView;

impl ChannelStatusView for FeishuStatus {
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
        if let Some(app_id) = &self.app_id {
            v.push(("App ID".into(), app_id.clone()));
        }
        v
    }
}

channel_status_page! {
    component: FeishuChannel,
    status: FeishuStatus,
    channel_id: "feishu",
    title: "Feishu / Lark",
    icon: crate::utils::icons::ICON_FEISHU,
    subtitle: "Bot status and channel configuration.",
}
