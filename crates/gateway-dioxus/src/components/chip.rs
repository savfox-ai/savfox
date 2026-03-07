use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
pub enum ChipVariant {
    Success,
    Warning,
    Danger,
    Muted,
    Info,
}

impl ChipVariant {
    fn class_suffix(&self) -> &'static str {
        match self {
            ChipVariant::Success => "success",
            ChipVariant::Warning => "warning",
            ChipVariant::Danger => "danger",
            ChipVariant::Muted => "muted",
            ChipVariant::Info => "info",
        }
    }
}

#[component]
pub fn Chip(label: String, variant: ChipVariant) -> Element {
    let class = format!("chip chip--{}", variant.class_suffix());
    rsx! {
        span { class: "{class}", role: "status", "{label}" }
    }
}
