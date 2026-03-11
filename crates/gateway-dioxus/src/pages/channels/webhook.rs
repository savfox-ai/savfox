use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn WebhookChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "webhook",
            title: "Generic Webhook",
            subtitle: "Generic webhook ingress and callback channel status.",
            icon_svg: crate::utils::icons::ICON_LINK,
        }
    }
}
