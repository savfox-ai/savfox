use crate::utils::icons::{get_icon, get_platform_icon};
use dioxus::prelude::*;

#[component]
pub fn Icon(name: String, class: Option<String>) -> Element {
    let icon_svg = get_icon(&name).unwrap_or("");
    let class_str = class.unwrap_or_else(|| "icon".to_string());

    rsx! {
        span {
            class: "{class_str}",
            dangerous_inner_html: icon_svg,
        }
    }
}

#[component]
pub fn PlatformIcon(platform: String, class: Option<String>) -> Element {
    let icon_svg = get_platform_icon(&platform);
    let class_str = class.unwrap_or_else(|| "icon platform-icon".to_string());

    rsx! {
        span {
            class: "{class_str}",
            dangerous_inner_html: icon_svg,
        }
    }
}

#[component]
pub fn LoadingSpinner(class: Option<String>) -> Element {
    let class_str = class.unwrap_or_else(|| "loading-spinner".to_string());

    rsx! {
        span {
            class: "{class_str}",
            dangerous_inner_html: crate::utils::icons::ICON_LOADER,
        }
    }
}

pub fn get_icon_svg(name: &str) -> &'static str {
    get_icon(name).unwrap_or("")
}
