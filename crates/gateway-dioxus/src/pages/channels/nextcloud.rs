use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn NextCloudChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "nextcloud",
            title: "NextCloud Talk",
            subtitle: "NextCloud Talk room monitoring and outbound status.",
            icon_svg: crate::utils::icons::ICON_MESSAGE_SQUARE,
        }
    }
}
