use crate::components::icon::Icon;
use crate::components::markdown_renderer::MarkdownRenderer;
use dioxus::prelude::*;

#[component]
pub fn MarkdownSidebar(
    content: Option<String>,
    error: Option<String>,
    title: String,
    on_close: EventHandler<()>,
    on_view_raw: Option<EventHandler<()>>,
) -> Element {
    rsx! {
        div {
            class: "sidebar-panel fixed right-0 top-0 h-full w-96 bg-white shadow-lg border-l flex flex-col z-50",

            div { class: "sidebar-header flex items-center justify-between px-4 py-3 border-b bg-gray-50",
                h3 { class: "sidebar-title font-medium text-gray-900", "{title}" }
                button {
                    class: "p-1 text-gray-500 hover:text-gray-700 hover:bg-gray-100 rounded",
                    r#type: "button",
                    title: "Close",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x".to_string(), class: Some("w-5 h-5".to_string()) }
                }
            }

            div { class: "sidebar-content flex-1 overflow-auto p-4",
                if let Some(err) = error {
                    ErrorContent { error: err, on_view_raw: on_view_raw }
                } else if let Some(text) = content {
                    div { class: "sidebar-markdown",
                        MarkdownRenderer { content: text }
                    }
                } else {
                    div { class: "muted text-gray-500 text-center py-8",
                        "No content available"
                    }
                }
            }
        }
        style { {SIDEBAR_STYLES} }
    }
}

#[component]
fn ErrorContent(error: String, on_view_raw: Option<EventHandler<()>>) -> Element {
    rsx! {
        div { class: "callout-danger bg-red-50 border border-red-200 rounded-md p-3 mb-3",
            p { class: "text-red-700 text-sm", "{error}" }
        }
        if let Some(ref handler) = on_view_raw {
            {
                let handler = handler.clone();
                rsx! {
                    button {
                        class: "btn-secondary px-3 py-1.5 text-sm border rounded hover:bg-gray-50",
                        r#type: "button",
                        onclick: move |_| handler.call(()),
                        "View Raw Text"
                    }
                }
            }
        }
    }
}

#[component]
pub fn ResizableSidebar(
    content: Option<String>,
    error: Option<String>,
    title: String,
    default_width: Option<u32>,
    min_width: Option<u32>,
    max_width: Option<u32>,
    on_close: EventHandler<()>,
    on_view_raw: Option<EventHandler<()>>,
    on_resize: Option<EventHandler<u32>>,
) -> Element {
    let width = use_signal(|| default_width.unwrap_or(384));

    rsx! {
        div {
            class: "sidebar-panel resizable fixed right-0 top-0 h-full bg-white shadow-lg border-l flex flex-col z-50",
            style: "width: {width()}px",

            div {
                class: "resize-handle absolute left-0 top-0 w-1 h-full cursor-ew-resize hover:bg-blue-400 transition-colors",
            }

            div { class: "sidebar-header flex items-center justify-between px-4 py-3 border-b bg-gray-50",
                h3 { class: "sidebar-title font-medium text-gray-900", "{title}" }
                button {
                    class: "p-1 text-gray-500 hover:text-gray-700 hover:bg-gray-100 rounded",
                    r#type: "button",
                    title: "Close",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x".to_string(), class: Some("w-5 h-5".to_string()) }
                }
            }

            div { class: "sidebar-content flex-1 overflow-auto p-4",
                if let Some(err) = error {
                    ErrorContent { error: err, on_view_raw: on_view_raw }
                } else if let Some(text) = content {
                    div { class: "sidebar-markdown",
                        MarkdownRenderer { content: text }
                    }
                } else {
                    div { class: "muted text-gray-500 text-center py-8",
                        "No content available"
                    }
                }
            }
        }
        style { {SIDEBAR_STYLES} }
    }
}

const SIDEBAR_STYLES: &str = r#"
    .sidebar-panel {
        font-family: system-ui, -apple-system, sans-serif;
    }
    
    .sidebar-panel .sidebar-markdown {
        font-size: 14px;
        line-height: 1.6;
    }
    
    .sidebar-panel .sidebar-markdown h1,
    .sidebar-panel .sidebar-markdown h2,
    .sidebar-panel .sidebar-markdown h3 {
        margin-top: 1em;
        margin-bottom: 0.5em;
        font-weight: 600;
    }
    
    .sidebar-panel .sidebar-markdown h1 { font-size: 1.5em; }
    .sidebar-panel .sidebar-markdown h2 { font-size: 1.25em; }
    .sidebar-panel .sidebar-markdown h3 { font-size: 1.1em; }
    
    .sidebar-panel .sidebar-markdown pre {
        background: #f6f8fa;
        padding: 12px;
        border-radius: 6px;
        overflow-x: auto;
        font-size: 13px;
    }
    
    .sidebar-panel .sidebar-markdown code {
        background: #f6f8fa;
        padding: 2px 4px;
        border-radius: 3px;
        font-size: 13px;
    }
    
    .sidebar-panel .sidebar-markdown pre code {
        background: transparent;
        padding: 0;
    }
    
    .sidebar-panel .sidebar-markdown blockquote {
        border-left: 3px solid #e5e7eb;
        margin: 0.5em 0;
        padding-left: 1em;
        color: #6b7280;
    }
    
    .sidebar-panel .sidebar-markdown ul,
    .sidebar-panel .sidebar-markdown ol {
        margin: 0.5em 0;
        padding-left: 1.5em;
    }
    
    .sidebar-panel .sidebar-markdown li {
        margin: 0.25em 0;
    }
    
    .sidebar-panel .resize-handle {
        z-index: 10;
    }
    
    .sidebar-panel.resizable {
        position: relative;
    }
"#;
