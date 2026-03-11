use dioxus::prelude::*;

/// Tooltip position relative to the trigger.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum TooltipPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

/// A tooltip that shows on hover/focus. Uses CSS-only positioning.
#[component]
pub fn Tooltip(
    text: String,
    #[props(default)] position: TooltipPosition,
    children: Element,
) -> Element {
    let pos_class = match position {
        TooltipPosition::Top => "tooltip-content tooltip-content--top",
        TooltipPosition::Bottom => "tooltip-content tooltip-content--bottom",
        TooltipPosition::Left => "tooltip-content tooltip-content--left",
        TooltipPosition::Right => "tooltip-content tooltip-content--right",
    };

    rsx! {
        span { class: "tooltip-wrap",
            {children}
            span { class: "{pos_class}", "{text}" }
        }
    }
}

/// A small "?" help icon that shows a tooltip on hover.
#[component]
pub fn HelpTip(text: String, #[props(default)] position: TooltipPosition) -> Element {
    rsx! {
        Tooltip { text, position,
            span { class: "tooltip-trigger", tabindex: "0", "?" }
        }
    }
}
