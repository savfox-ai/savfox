use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn TlonChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "tlon",
            title: "Tlon",
            subtitle: "Urbit / Tlon channel connection and routing status.",
            icon_svg: crate::utils::icons::ICON_GLOBE,
        }
    }
}
