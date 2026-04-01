use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn ZaloChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "zalo",
            title: "Zalo",
            subtitle: "Zalo OA webhook and outbound messaging status.",
            icon_svg: crate::utils::icons::ICON_GLOBE,
        }
    }
}
