use dioxus::prelude::*;

use super::generic::GenericChannelPage;

#[component]
pub fn DingtalkChannel() -> Element {
    rsx! {
        GenericChannelPage {
            platform: "dingtalk",
            title: "DingTalk",
            subtitle: "DingTalk robot and stream channel status.",
            icon_svg: crate::utils::icons::ICON_GLOBE,
        }
    }
}
