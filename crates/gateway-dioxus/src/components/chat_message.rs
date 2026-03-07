use dioxus::prelude::*;

use crate::api::types::ChatMessage;
use crate::components::canvas_renderer::{
    Artifact, CanvasRenderer, parse_artifacts, strip_artifacts,
};
use crate::components::copy_button::CopyButton;
use crate::components::markdown_renderer::MarkdownRenderer;
use crate::utils::text::dir_attr_for_text;

/// Parsed tool call from message content.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub content: String,
    pub status: ToolStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolStatus {
    Success,
    Error,
    Running,
}

/// Extract tool call blocks from content.
pub fn parse_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();

    // <tool_call>...</tool_call> blocks
    let mut pos = 0;
    while let Some(start) = content[pos..].find("<tool_call>") {
        let abs_start = pos + start;
        if let Some(end) = content[abs_start..].find("</tool_call>") {
            let inner = &content[abs_start + 11..abs_start + end];
            let name = inner
                .lines()
                .next()
                .unwrap_or("tool_call")
                .trim()
                .to_string();
            let status = if inner.contains("error") || inner.contains("Error") {
                ToolStatus::Error
            } else {
                ToolStatus::Success
            };
            calls.push(ToolCall {
                name,
                content: inner.to_string(),
                status,
            });
            pos = abs_start + end + 12;
        } else {
            break;
        }
    }

    // ```tool blocks
    pos = 0;
    while let Some(start) = content[pos..].find("```tool") {
        let abs_start = pos + start;
        let after = abs_start + 7;
        if let Some(end) = content[after..].find("```") {
            let inner = &content[after..after + end];
            let name = inner.lines().next().unwrap_or("tool").trim().to_string();
            calls.push(ToolCall {
                name,
                content: inner.to_string(),
                status: ToolStatus::Success,
            });
            pos = after + end + 3;
        } else {
            break;
        }
    }

    calls
}

/// Strip tool call blocks from content for clean display.
fn strip_tool_blocks(content: &str) -> String {
    let mut result = content.to_string();

    // Remove <tool_call>...</tool_call>
    while let Some(start) = result.find("<tool_call>") {
        if let Some(end) = result[start..].find("</tool_call>") {
            result.replace_range(start..start + end + 12, "");
        } else {
            break;
        }
    }

    // Remove ```tool...```
    while let Some(start) = result.find("```tool") {
        let after = start + 7;
        if let Some(end) = result[after..].find("```") {
            result.replace_range(start..after + end + 3, "");
        } else {
            break;
        }
    }

    result.trim().to_string()
}

fn normalize_verbose_mode(value: Option<&str>) -> &'static str {
    match value.unwrap_or("on").trim().to_ascii_lowercase().as_str() {
        "off" => "off",
        "full" => "full",
        _ => "on",
    }
}

fn brief_tool_output(content: &str, max_chars: usize) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no output)");
    let mut out: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[component]
pub fn ChatMessageBubble(
    message: ChatMessage,
    show_thinking: Option<bool>,
    verbose_mode: Option<String>,
    is_last: Option<bool>,
    is_streaming: Option<bool>,
    prev_same_role: Option<bool>,
    on_expand_tool: Option<EventHandler<String>>,
) -> Element {
    let is_user = message.role == "user";
    let is_streaming = is_streaming.unwrap_or(false);
    let is_last = is_last.unwrap_or(false);
    let show_thinking = show_thinking.unwrap_or(false);
    let verbose_mode = normalize_verbose_mode(verbose_mode.as_deref());
    let prev_same = prev_same_role.unwrap_or(false);

    let bubble_class = if is_user {
        "chat-bubble chat-bubble-user"
    } else {
        "chat-bubble chat-bubble-assistant"
    };

    let message_class = if prev_same {
        "chat-message chat-message--grouped"
    } else {
        "chat-message"
    };

    let has_thinking = show_thinking && message.thinking.is_some();
    let has_attachments = !message.attachments.is_empty();

    // Parse and strip tool calls for assistant messages
    let tool_calls = if !is_user {
        parse_tool_calls(&message.content)
    } else {
        vec![]
    };
    let content_after_tools = if !is_user && !tool_calls.is_empty() {
        strip_tool_blocks(&message.content)
    } else {
        message.content.clone()
    };

    // Parse and strip artifacts (canvas/artifact blocks)
    let artifacts: Vec<Artifact> = if !is_user {
        parse_artifacts(&content_after_tools)
    } else {
        vec![]
    };
    let display_content = if !artifacts.is_empty() {
        strip_artifacts(&content_after_tools)
    } else {
        content_after_tools
    };
    let message_dir = dir_attr_for_text(&display_content);

    rsx! {
        div { class: "{message_class}",
            div { class: "{bubble_class}", dir: "{message_dir}",
                // Role label for non-grouped messages
                if !prev_same && !is_user {
                    div { class: "chat-bubble__role", "Assistant" }
                }

                // Timestamp for non-grouped messages
                if !prev_same {
                    if let Some(ref ts) = message.timestamp {
                        div { class: "chat-bubble__timestamp", "{ts}" }
                    }
                }

                // Image attachments
                if has_attachments {
                    div { class: "chat-bubble__attachments",
                        for (i, att) in message.attachments.iter().enumerate() {
                            if att.mime_type.starts_with("image/") {
                                img {
                                    key: "{i}",
                                    src: "data:{att.mime_type};base64,{att.data_base64}",
                                    alt: "{att.filename}",
                                    class: "chat-bubble__attachment-img",
                                }
                            }
                        }
                    }
                }

                // Thinking section (enhanced)
                if has_thinking {
                    {
                        let thinking_text = message.thinking.as_deref().unwrap_or("");
                        let thinking_dir = dir_attr_for_text(thinking_text);
                        rsx! {
                            details { class: "chat-thinking",
                                summary { class: "chat-thinking__summary",
                                    span { "Thinking" }
                                }
                                pre { class: "chat-thinking__content", dir: "{thinking_dir}", "{thinking_text}" }
                            }
                        }
                    }
                }

                // Tool call cards
                if verbose_mode != "off" {
                    for (idx, tc) in tool_calls.iter().enumerate() {
                        {
                            let status_class = match tc.status {
                                ToolStatus::Success => "tool-card--success",
                                ToolStatus::Error => "tool-card--error",
                                ToolStatus::Running => "tool-card--running",
                            };
                            let status_icon = match tc.status {
                                ToolStatus::Success => "OK",
                                ToolStatus::Error => "!!",
                                ToolStatus::Running => "..",
                            };
                            let is_short = tc.content.lines().count() <= 5;
                            let tool_content = tc.content.clone();
                            let tool_preview = brief_tool_output(&tc.content, 180);
                            let expand_content = tc.content.clone();
                            let handler = on_expand_tool.clone();
                            let card_handler = on_expand_tool.clone();
                            let card_content = tc.content.clone();
                            let clickable_class = if on_expand_tool.is_some() {
                                "tool-card--clickable"
                            } else {
                                ""
                            };
                            rsx! {
                                div {
                                    key: "{idx}",
                                    class: "tool-card {status_class} {clickable_class}",
                                    onclick: move |_| {
                                        if let Some(h) = card_handler.clone() {
                                            h.call(card_content.clone());
                                        }
                                    },
                                    div { class: "tool-card__header",
                                        span { class: "tool-card__status", "{status_icon}" }
                                        span { class: "tool-card__name", "{tc.name}" }
                                        if let Some(ref h) = handler {
                                            {
                                                let h = h.clone();
                                                let expand_content = expand_content.clone();
                                                rsx! {
                                                    button {
                                                        class: "tool-card__expand",
                                                        onclick: move |_| h.call(expand_content.clone()),
                                                        "Expand"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if verbose_mode == "full" {
                                        if is_short {
                                            pre { class: "tool-card__body tool-card__body--inline",
                                                "{tool_content}"
                                            }
                                        } else {
                                            details { class: "tool-card__details",
                                                summary { "Show output ({tc.content.lines().count()} lines)" }
                                                pre { class: "tool-card__body", "{tool_content}" }
                                            }
                                        }
                                    } else {
                                        pre { class: "tool-card__body tool-card__body--inline",
                                            "{tool_preview}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Canvas artifacts
                for (aidx, art) in artifacts.iter().enumerate() {
                    {
                        let expand_handler = on_expand_tool.clone();
                        rsx! {
                            CanvasRenderer {
                                key: "{aidx}",
                                artifact: art.clone(),
                                on_expand: expand_handler,
                            }
                        }
                    }
                }

                // Main content
                // Show loading indicator only when the original message has no content at all
                if message.content.is_empty() && is_last && is_streaming {
                    div { class: "reading-indicator",
                        span { class: "reading-indicator__dot" }
                        span { class: "reading-indicator__dot" }
                        span { class: "reading-indicator__dot" }
                    }
                }
                // Always render content div if there's any text to display
                if !display_content.is_empty() {
                    div { class: "chat-bubble__content", dir: "{message_dir}",
                        MarkdownRenderer { content: display_content.clone() }
                    }
                }

                // Copy button for assistant messages
                if !is_user && !message.content.is_empty() {
                    div { class: "chat-bubble__actions",
                        CopyButton { text: message.content.clone() }
                    }
                }
            }
        }
        style { {CHAT_BUBBLE_STYLES} }
    }
}

const CHAT_BUBBLE_STYLES: &str = r#"
    .chat-message {
        display: flex;
        padding: 24px 16px;
        width: 100%;
        position: relative;
    }

    .chat-message--grouped {
        display: flex;
        padding: 8px 16px;
        width: 100%;
        position: relative;
    }

    .chat-bubble {
        width: 100%;
        max-width: 800px;
        margin: 0 auto;
        padding: 0 16px;
        white-space: pre-wrap;
        word-break: break-word;
        font-size: 15px;
        line-height: 1.65;
        position: relative;
    }

    .chat-bubble-user {
        background: transparent;
        color: var(--text-primary);
        font-weight: 400;
        border-radius: var(--radius);
        padding: 16px 24px;
        background: color-mix(in srgb, var(--bg-tertiary) 60%, transparent);
        border: 1px solid color-mix(in srgb, var(--border) 40%, transparent);
        box-shadow: 0 2px 12px rgba(0,0,0,0.02);
    }

    .chat-bubble-user .chat-bubble__content {
        font-size: 15px;
        color: var(--text-primary);
    }

    .chat-bubble-assistant {
        background: transparent;
        color: var(--text-primary);
    }

    .chat-bubble__role {
        font-size: 12px;
        font-weight: 600;
        color: var(--accent);
        text-transform: uppercase;
        letter-spacing: 0.06em;
        margin-bottom: 8px;
        display: flex;
        align-items: center;
        gap: 8px;
    }
    
    .chat-bubble__role::before {
        content: '';
        display: inline-block;
        width: 16px;
        height: 16px;
        background: var(--accent);
        border-radius: 4px;
        opacity: 0.85;
    }

    .chat-bubble__timestamp {
        position: absolute;
        top: 24px;
        right: 32px;
        font-size: 11px;
        color: var(--text-muted);
        opacity: 0;
        transition: opacity 0.2s ease;
    }
    
    .chat-message:hover .chat-bubble__timestamp {
        opacity: 1;
    }

    .chat-bubble__content {
        white-space: normal;
        word-break: break-word;
        unicode-bidi: plaintext;
    }

    .chat-bubble[dir="rtl"] {
        text-align: right;
    }

    .chat-bubble[dir="ltr"] {
        text-align: left;
    }

    .chat-bubble__actions {
        position: absolute;
        bottom: -16px;
        right: 16px;
        opacity: 0;
        transition: opacity 0.2s ease, transform 0.2s ease;
        transform: translateY(4px);
    }
    
    .chat-message:hover .chat-bubble__actions {
        opacity: 1;
        transform: translateY(0);
    }

    /* ── Thinking ── */
    .chat-thinking {
        margin-bottom: 16px;
        background: transparent;
        border-left: 2px solid color-mix(in srgb, var(--accent) 50%, transparent);
        padding: 0 0 0 16px;
    }

    .chat-thinking__summary {
        cursor: pointer;
        padding: 8px 0;
        font-size: 13px;
        font-weight: 500;
        color: var(--text-secondary);
        display: flex;
        align-items: center;
        gap: 8px;
        user-select: none;
        transition: color 0.2s ease;
    }
    
    .chat-thinking__summary:hover {
        color: var(--text-primary);
    }

    .chat-thinking__content {
        margin: 0;
        padding: 12px 0 16px 0;
        font-size: 13px;
        max-height: 400px;
        overflow: auto;
        white-space: pre-wrap;
        color: var(--text-muted);
        font-family: var(--font-mono, monospace);
        unicode-bidi: plaintext;
        line-height: 1.6;
    }

    /* ── Tool Cards ── */
    .tool-card {
        margin: 12px 0;
        border: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
        border-radius: 12px;
        overflow: hidden;
        background: color-mix(in srgb, var(--bg-secondary) 50%, transparent);
        box-shadow: 0 2px 8px rgba(0,0,0,0.02);
        transition: all 0.2s ease;
    }

    .tool-card--clickable {
        cursor: pointer;
    }

    .tool-card--clickable:hover {
        border-color: color-mix(in srgb, var(--accent) 50%, transparent);
        box-shadow: 0 4px 12px rgba(0,0,0,0.05);
        transform: translateY(-1px);
    }

    .tool-card--success { border-left: 3px solid var(--success); }
    .tool-card--error   { border-left: 3px solid var(--danger); }
    .tool-card--running { border-left: 3px solid var(--warning); }

    .tool-card__header {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 10px 14px;
        font-size: 13px;
        background: color-mix(in srgb, var(--bg-tertiary) 30%, transparent);
        border-bottom: 1px solid color-mix(in srgb, var(--border) 30%, transparent);
    }

    .tool-card__status {
        font-weight: 700;
        font-size: 11px;
        padding: 2px 6px;
        border-radius: 4px;
        line-height: 1;
    }

    .tool-card--success .tool-card__status { background: color-mix(in srgb, var(--success) 15%, transparent); color: var(--success); }
    .tool-card--error .tool-card__status { background: color-mix(in srgb, var(--danger) 15%, transparent); color: var(--danger); }
    .tool-card--running .tool-card__status { background: color-mix(in srgb, var(--warning) 15%, transparent); color: var(--warning); }

    .tool-card__name {
        font-weight: 600;
        color: var(--text-primary);
        flex: 1;
        font-family: var(--font-mono, monospace);
    }

    .tool-card__expand {
        background: transparent;
        border: 1px solid var(--border);
        border-radius: 6px;
        color: var(--text-secondary);
        font-size: 11px;
        font-weight: 500;
        padding: 4px 10px;
        cursor: pointer;
        transition: all 0.2s ease;
    }

    .tool-card__expand:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        border-color: color-mix(in srgb, var(--border) 80%, var(--accent) 20%);
    }

    .tool-card__body {
        padding: 12px 14px;
        font-size: 12px;
        font-family: var(--font-mono, monospace);
        white-space: pre-wrap;
        word-break: break-word;
        color: var(--text-secondary);
        margin: 0;
        max-height: 350px;
        overflow: auto;
        line-height: 1.5;
    }

    .tool-card__body--inline {
        max-height: none;
    }

    .tool-card__details > summary {
        padding: 8px 14px;
        font-size: 12px;
        font-weight: 500;
        color: var(--text-muted);
        cursor: pointer;
        transition: color 0.2s ease;
    }
    
    .tool-card__details > summary:hover {
        color: var(--text-primary);
    }

    /* ── Attachments ── */
    .chat-bubble__attachments {
        display: flex;
        gap: 12px;
        margin-bottom: 12px;
        flex-wrap: wrap;
    }

    .chat-bubble__attachment-img {
        max-width: 240px;
        max-height: 180px;
        border-radius: 8px;
        object-fit: cover;
        box-shadow: 0 4px 12px rgba(0,0,0,0.1);
        border: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
    }

    .chat-bubble code {
        background: color-mix(in srgb, var(--bg-tertiary) 80%, transparent);
        padding: 2px 6px;
        border-radius: 4px;
        font-size: 13.5px;
        font-family: var(--font-mono, monospace);
        color: var(--accent);
    }

    .chat-bubble pre {
        background: var(--bg-secondary);
        padding: 16px;
        border-radius: 12px;
        overflow-x: auto;
        margin: 16px 0;
        border: 1px solid color-mix(in srgb, var(--border) 30%, transparent);
        box-shadow: 0 4px 12px rgba(0,0,0,0.02);
    }

    .chat-bubble pre code {
        background: transparent;
        padding: 0;
        color: var(--text-primary);
        font-size: 13px;
        line-height: 1.5;
    }

    @media screen and (max-width: 768px) {
        .chat-message {
            padding: 16px 12px;
        }
        .chat-bubble {
            padding: 0;
        }
    }

    /* ── Canvas Artifacts ── */
    .canvas-artifact {
        margin: 16px 0;
        border: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
        border-radius: 12px;
        overflow: hidden;
        background: var(--bg-primary);
        box-shadow: 0 4px 16px rgba(0,0,0,0.04);
        transition: transform 0.2s ease, box-shadow 0.2s ease;
    }
    
    .canvas-artifact:hover {
        box-shadow: 0 8px 24px rgba(0,0,0,0.08);
        transform: translateY(-2px);
    }

    .canvas-artifact__toolbar {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 12px 16px;
        background: color-mix(in srgb, var(--bg-secondary) 50%, transparent);
        border-bottom: 1px solid color-mix(in srgb, var(--border) 30%, transparent);
        gap: 12px;
    }

    .canvas-artifact__info {
        display: flex;
        align-items: center;
        gap: 10px;
        min-width: 0;
    }

    .canvas-artifact__type-badge {
        font-size: 10px;
        font-weight: 700;
        padding: 2px 8px;
        background: color-mix(in srgb, var(--accent) 15%, transparent);
        color: var(--accent);
        border-radius: 4px;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        white-space: nowrap;
    }

    .canvas-artifact__title {
        font-size: 13px;
        font-weight: 500;
        color: var(--text-primary);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .canvas-artifact__actions {
        display: flex;
        gap: 6px;
        flex-shrink: 0;
    }

    .canvas-artifact__btn {
        background: var(--bg-primary);
        border: 1px solid var(--border);
        border-radius: 6px;
        color: var(--text-secondary);
        font-size: 12px;
        font-weight: 500;
        padding: 4px 10px;
        cursor: pointer;
        transition: all 0.2s ease;
    }

    .canvas-artifact__btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        border-color: color-mix(in srgb, var(--border) 80%, var(--accent) 20%);
    }

    .canvas-artifact__frame {
        width: 100%;
        min-height: 300px;
        max-height: 600px;
        border: none;
        background: var(--bg-secondary);
    }

    @media screen and (max-width: 480px) {
        .canvas-artifact__frame {
            min-height: 200px;
        }
    }
"#;
