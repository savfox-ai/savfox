use crate::api::types::CokretStatus;
use crate::pages::channels::common::{ChannelStatusView, channel_status_page};

impl ChannelStatusView for CokretStatus {
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
        self.account_id
            .clone()
            .or_else(|| self.applet_id.clone())
            .or_else(|| self.service_did.clone())
    }

    fn extra_stats(&self) -> Vec<(String, String)> {
        let mut stats = Vec::new();
        if let Some(mode) = &self.mode {
            stats.push(("Mode".into(), mode.clone()));
        }
        if let Some(base_url) = &self.base_url {
            stats.push(("Base URL".into(), base_url.clone()));
        }
        if let Some(service_did) = &self.service_did {
            stats.push(("Service DID".into(), service_did.clone()));
        }
        if let Some(account_id) = &self.account_id {
            stats.push(("Account".into(), account_id.clone()));
        }
        if let Some(principal_id) = &self.principal_id {
            stats.push(("Principal".into(), principal_id.clone()));
        }
        if let Some(realm_id) = &self.default_realm_id {
            stats.push(("Default Realm".into(), realm_id.clone()));
        }
        if let Some(applet_id) = &self.applet_id {
            stats.push(("Applet".into(), applet_id.clone()));
        }
        if let Some(bot_actor_id) = &self.bot_actor_id {
            stats.push(("Bot Actor".into(), bot_actor_id.clone()));
        }
        if let Some(protocol_count) = self.protocol_count {
            stats.push(("Protocols".into(), protocol_count.to_string()));
        }
        if let Some(namespace_count) = self.namespace_count {
            stats.push(("Namespaces".into(), namespace_count.to_string()));
        }
        stats
    }
}

channel_status_page! {
    component: CokretChannel,
    status: CokretStatus,
    channel_id: "cokret",
    title: "Cokret",
    icon: crate::utils::icons::ICON_RADIO,
    subtitle: "Account client and applet bridge status.",
}
