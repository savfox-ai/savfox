use dioxus::prelude::*;

/// A single breadcrumb segment.
#[derive(Clone, PartialEq)]
pub struct BreadcrumbItem {
    pub label: String,
    pub href: Option<String>,
}

/// Breadcrumb navigation bar.
#[component]
pub fn Breadcrumb(items: Vec<BreadcrumbItem>) -> Element {
    if items.is_empty() {
        return rsx! {};
    }

    rsx! {
        nav { class: "breadcrumb", aria_label: "Breadcrumb",
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    span { class: "breadcrumb__sep", aria_hidden: "true", "/" }
                }
                if let Some(ref href) = item.href {
                    a { class: "breadcrumb__link", href: "{href}", "{item.label}" }
                } else {
                    span { class: "breadcrumb__current", aria_current: "page", "{item.label}" }
                }
            }
        }
    }
}
