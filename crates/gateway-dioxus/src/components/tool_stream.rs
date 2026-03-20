use dioxus::prelude::*;
use lucide_dioxus::{Loader, Settings, CircleCheck, CircleX, ChevronDown, ChevronRight};
use serde::{Deserialize, Serialize};

/// A single tool invocation entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub status: ToolStatus,
    pub input_preview: Option<String>,
    pub output_preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Shows a collapsible list of tool invocations with status indicators.
#[component]
pub fn ToolStream(calls: Vec<ToolCall>) -> Element {
    if calls.is_empty() {
        return rsx! {};
    }

    rsx! {
        div {
            class: "tool-stream",
            style: "border: 1px solid var(--border, #333); border-radius: 8px; margin: 8px 0; overflow: hidden;",
            div {
                style: "padding: 6px 12px; background: var(--surface-alt, #1a1a2e); font-size: 0.8em; font-weight: 600; color: var(--text-secondary, #aaa);",
                "Tool Calls ({calls.len()})"
            }
            for call in calls.iter() {
                ToolCallEntry { call: call.clone() }
            }
        }
    }
}

#[component]
fn ToolCallEntry(call: ToolCall) -> Element {
    let mut expanded = use_signal(|| false);
    rsx! {
        div {
            style: "border-top: 1px solid var(--border, #333); padding: 8px 12px; cursor: pointer;",
            onclick: move |_| expanded.set(!expanded()),
            div {
                style: "display: flex; align-items: center; gap: 8px;",
                span {
                    match &call.status {
                        ToolStatus::Pending => rsx! { Loader { size: 14 } },
                        ToolStatus::Running => rsx! { Settings { size: 14 } },
                        ToolStatus::Completed => rsx! { CircleCheck { size: 14 } },
                        ToolStatus::Failed => rsx! { CircleX { size: 14 } },
                    }
                }
                span {
                    style: "font-family: monospace; font-size: 0.85em; font-weight: 500;",
                    "{call.name}"
                }
                span {
                    style: "margin-left: auto; font-size: 0.75em; color: var(--text-muted, #666);",
                    if expanded() { ChevronDown { size: 14 } } else { ChevronRight { size: 14 } }
                }
            }
            if expanded() {
                div {
                    style: "margin-top: 8px; padding: 8px; background: var(--surface, #111); border-radius: 4px; font-family: monospace; font-size: 0.8em; white-space: pre-wrap; max-height: 200px; overflow-y: auto;",
                    if let Some(ref input) = call.input_preview {
                        div {
                            style: "color: var(--text-secondary, #aaa); margin-bottom: 4px;",
                            "Input: {input}"
                        }
                    }
                    if let Some(ref output) = call.output_preview {
                        div {
                            style: "color: var(--text-primary, #ddd);",
                            "Output: {output}"
                        }
                    }
                }
            }
        }
    }
}
