use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn TwitchChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "twitch",
            title: "Twitch",
            subtitle: "Twitch chat connector and webhook status.",
            icon_svg: crate::utils::icons::ICON_RADIO,
        }
    }
}
