use dioxus::prelude::*;

/// Skeleton loading placeholder — pulsing rectangle.
#[component]
pub fn Skeleton(
    #[props(default = "100%".to_string())] width: String,
    #[props(default = "16px".to_string())] height: String,
    #[props(default = "var(--radius)".to_string())] radius: String,
) -> Element {
    rsx! {
        div {
            class: "skeleton",
            style: "width:{width};height:{height};border-radius:{radius};",
        }
    }
}

/// Multiple skeleton lines for text content.
#[component]
pub fn SkeletonLines(#[props(default = 3)] count: usize) -> Element {
    rsx! {
        div { class: "skeleton-lines",
            for i in 0..count {
                {
                    let w = if i == count - 1 { "60%" } else { "100%" };
                    rsx! {
                        div {
                            key: "{i}",
                            class: "skeleton",
                            style: "height:14px;border-radius:4px;width:{w};",
                        }
                    }
                }
            }
        }
    }
}

/// Skeleton card with header + lines.
#[component]
pub fn SkeletonCard() -> Element {
    rsx! {
        div { class: "skeleton-card", aria_busy: "true",
            span { class: "sr-only", "Loading..." }
            Skeleton { width: "40%", height: "20px" }
            SkeletonLines { count: 3 }
        }
    }
}
