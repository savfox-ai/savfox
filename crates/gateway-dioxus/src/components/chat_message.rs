use std::sync::Once;

use dioxus::prelude::*;

use crate::api::types::{ChatMessage, ChatTerminalEvent};
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

fn terminal_event_title(event: &ChatTerminalEvent) -> String {
    match event.event.as_str() {
        "log" => event
            .stream
            .as_deref()
            .map(|stream| format!("log:{stream}"))
            .unwrap_or_else(|| "log".to_string()),
        "status" => "status".to_string(),
        "message" => "message".to_string(),
        "error" => "error".to_string(),
        "completed" => "completed".to_string(),
        other => other.to_string(),
    }
}

fn terminal_event_body(event: &ChatTerminalEvent) -> String {
    event
        .text
        .as_deref()
        .or(event.status.as_deref())
        .or(event.message.as_deref())
        .map(str::to_string)
        .or_else(|| event.exit_code.map(|code| format!("exit code {code}")))
        .unwrap_or_default()
}

#[component]
pub fn ChatMessageBubble(
    message: ReadSignal<ChatMessage>,
    show_thinking: Option<bool>,
    verbose_mode: Option<String>,
    is_last: Option<bool>,
    is_streaming: Option<bool>,
    prev_same_role: Option<bool>,
    /// Display name shown above assistant messages (the active agent's name).
    /// Falls back to "Assistant" when empty/None.
    agent_name: Option<String>,
    on_expand_tool: Option<EventHandler<String>>,
) -> Element {
    inject_chat_bubble_styles_once();
    let message = message();
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

    let role_modifier = if is_user {
        "chat-message--user"
    } else {
        "chat-message--assistant"
    };
    let message_class = if prev_same {
        format!("chat-message chat-message--grouped {role_modifier}")
    } else {
        format!("chat-message {role_modifier}")
    };

    // Label above assistant messages — the active agent's name.
    let agent_label = agent_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Assistant")
        .to_string();

    let has_thinking = show_thinking && message.thinking.is_some();
    let has_attachments = !message.attachments.is_empty();
    let has_terminal_events = !message.terminal_events.is_empty();

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
                // Role label for non-grouped messages — shows the active agent name
                if !prev_same && !is_user {
                    div { class: "chat-bubble__role", "{agent_label}" }
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

                if has_terminal_events {
                    details { class: "terminal-timeline", open: true,
                        summary { class: "terminal-timeline__summary",
                            span { "Terminal timeline" }
                            span { class: "terminal-timeline__count", "{message.terminal_events.len()}" }
                        }
                        div { class: "terminal-timeline__list",
                            for (idx, event) in message.terminal_events.iter().enumerate() {
                                {
                                    let title = terminal_event_title(event);
                                    let body = terminal_event_body(event);
                                    let item_class = format!("terminal-timeline__item terminal-timeline__item--{}", event.event);
                                    rsx! {
                                        div { key: "{idx}", class: "{item_class}",
                                            div { class: "terminal-timeline__item-head",
                                                span { class: "terminal-timeline__dot" }
                                                span { class: "terminal-timeline__title", "{title}" }
                                            }
                                            if !body.trim().is_empty() {
                                                pre { class: "terminal-timeline__body", "{body}" }
                                            }
                                        }
                                    }
                                }
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
                            let line_count = tc.content.lines().count();
                            let is_short = line_count <= 5;
                            let tool_content = tc.content.clone();
                            let tool_preview = brief_tool_output(&tc.content, 180);
                            let expand_content = tool_content.clone();
                            let handler = on_expand_tool.clone();
                            let card_handler = on_expand_tool.clone();
                            let card_content = tool_content.clone();
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
                                                summary { "Show output ({line_count} lines)" }
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
    }
}

/// Injects the chat bubble stylesheet into the document head exactly once,
/// rather than emitting a `<style>` node per rendered bubble.
fn inject_chat_bubble_styles_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(el) = doc.create_element("style") {
                el.set_inner_html(CHAT_BUBBLE_STYLES);
                if let Some(head) = doc.head() {
                    let _ = head.append_child(&el);
                }
            }
        }
    });
}

const CHAT_BUBBLE_STYLES: &str = r#"
    .chat-message {
        display: flex;
        padding: 12px 24px;
        width: 100%;
        position: relative;
    }

    .chat-message--grouped {
        display: flex;
        padding: 4px 24px;
        width: 100%;
        position: relative;
    }

    /* User on the right, assistant on the left. */
    .chat-message--user { justify-content: flex-end; }
    .chat-message--assistant { justify-content: flex-start; }

    .chat-bubble {
        width: fit-content;
        max-width: min(76%, 720px);
        padding: 0 18px;
        white-space: pre-wrap;
        word-break: break-word;
        font-size: 15px;
        line-height: 1.65;
        position: relative;
    }

    .chat-bubble-user {
        color: var(--text-primary);
        font-weight: 400;
        border-radius: 12px 12px 4px 12px;
        padding: 14px 18px;
        background:
            linear-gradient(180deg,
                color-mix(in srgb, var(--accent) 10%, var(--surface-flat-strong) 90%) 0%,
                color-mix(in srgb, var(--ornament) 4%, var(--surface-flat) 96%) 100%);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 68%, var(--accent) 32%);
        box-shadow: var(--surface-inner), var(--surface-shadow-soft), 0 0 0 1px rgba(255,255,255,0.03);
        backdrop-filter: blur(20px) saturate(145%);
        -webkit-backdrop-filter: blur(20px) saturate(145%);
    }

    .chat-bubble-user .chat-bubble__content {
        font-size: 15px;
        color: var(--text-primary);
    }

    .chat-bubble-assistant {
        background: color-mix(in srgb, var(--surface-flat-soft) 58%, transparent);
        color: var(--text-primary);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 42%, transparent);
        border-radius: 12px 12px 12px 4px;
        padding: 14px 18px;
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.05);
        backdrop-filter: blur(14px) saturate(135%);
        -webkit-backdrop-filter: blur(14px) saturate(135%);
    }

    .chat-bubble__role {
        font-size: 12px;
        font-weight: 600;
        color: var(--text-secondary);
        margin-bottom: 6px;
        display: inline-flex;
        align-items: center;
        gap: 6px;
        font-family: var(--font-display);
        letter-spacing: 0.01em;
        text-transform: none;
    }

    .chat-bubble__role::before {
        content: '';
        display: inline-block;
        width: 6px;
        height: 6px;
        background: var(--accent);
        border-radius: 999px;
        box-shadow: 0 0 8px color-mix(in srgb, var(--accent) 42%, transparent);
    }

    .chat-bubble__timestamp {
        position: absolute;
        top: 24px;
        right: 32px;
        font-size: 11px;
        color: color-mix(in srgb, var(--text-muted) 84%, var(--ornament) 16%);
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
        background: color-mix(in srgb, var(--surface-flat-soft) 88%, transparent);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
        border-radius: 18px;
        padding: 0 14px 0 16px;
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), var(--surface-shadow-soft);
    }

    .chat-thinking__summary {
        cursor: pointer;
        padding: 12px 0;
        font-size: 13px;
        font-weight: 500;
        color: var(--text-secondary);
        display: flex;
        align-items: center;
        gap: 8px;
        user-select: none;
        transition: color 0.2s ease;
        list-style: none;
    }

    .chat-thinking__summary::-webkit-details-marker {
        display: none;
    }

    .chat-thinking__summary::before {
        content: '\25b6';
        font-size: 10px;
        transition: transform 0.2s ease;
        display: inline-block;
    }

    .chat-thinking[open] > .chat-thinking__summary::before {
        transform: rotate(90deg);
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

    .terminal-timeline {
        margin: 10px 0 12px;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 62%, transparent);
        border-radius: 8px;
        background: color-mix(in srgb, var(--surface-flat-soft) 72%, transparent);
        overflow: hidden;
    }

    .terminal-timeline__summary {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 8px;
        padding: 9px 11px;
        cursor: pointer;
        color: var(--text-secondary);
        font-size: 12px;
        font-weight: 650;
    }

    .terminal-timeline__count {
        min-width: 22px;
        height: 20px;
        padding: 0 6px;
        border-radius: 999px;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        background: color-mix(in srgb, var(--accent) 18%, transparent);
        color: var(--text-primary);
        font-size: 11px;
    }

    .terminal-timeline__list {
        padding: 2px 11px 10px;
        display: grid;
        gap: 8px;
    }

    .terminal-timeline__item {
        display: grid;
        gap: 5px;
        padding-left: 2px;
    }

    .terminal-timeline__item-head {
        display: flex;
        align-items: center;
        gap: 7px;
        min-height: 18px;
        color: var(--text-secondary);
        font-size: 12px;
        font-weight: 600;
    }

    .terminal-timeline__dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: color-mix(in srgb, var(--accent) 76%, var(--text-muted) 24%);
        flex: 0 0 auto;
    }

    .terminal-timeline__item--error .terminal-timeline__dot {
        background: var(--danger);
    }

    .terminal-timeline__item--completed .terminal-timeline__dot {
        background: var(--success);
    }

    .terminal-timeline__body {
        margin: 0 0 0 15px;
        padding: 8px 10px;
        border-radius: 7px;
        background: color-mix(in srgb, var(--bg-primary) 82%, transparent);
        color: var(--text-primary);
        font-family: var(--font-mono, monospace);
        font-size: 11px;
        line-height: 1.45;
        white-space: pre-wrap;
        overflow-x: auto;
        max-height: 220px;
    }

    /* ── Tool Cards ── */
    .tool-card {
        margin: 12px 0;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
        border-radius: 18px;
        overflow: hidden;
        background: color-mix(in srgb, var(--surface-flat-soft) 90%, transparent);
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), var(--surface-shadow-soft);
        transition: all 0.2s ease;
        backdrop-filter: blur(18px) saturate(140%);
        -webkit-backdrop-filter: blur(18px) saturate(140%);
    }

    .tool-card--clickable {
        cursor: pointer;
    }

    .tool-card--clickable:hover {
        border-color: color-mix(in srgb, var(--surface-stroke) 54%, var(--accent) 46%);
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.06), var(--surface-glow);
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
        background: color-mix(in srgb, var(--accent) 5%, var(--surface-flat-soft) 95%);
        border-bottom: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
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
        border-radius: 14px;
        object-fit: cover;
        box-shadow: 0 14px 28px rgba(0,0,0,0.16);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 70%, transparent);
    }

    .chat-bubble code {
        background: color-mix(in srgb, var(--surface-flat-soft) 92%, transparent);
        padding: 2px 6px;
        border-radius: 4px;
        font-size: 13.5px;
        font-family: var(--font-mono, monospace);
        color: var(--accent);
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 64%, transparent);
    }

    .chat-bubble pre {
        background: var(--surface-panel-strong);
        padding: 16px;
        border-radius: 18px;
        overflow-x: auto;
        margin: 16px 0;
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.06), var(--surface-shadow-soft);
        backdrop-filter: blur(16px) saturate(140%);
        -webkit-backdrop-filter: blur(16px) saturate(140%);
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
        border: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
        border-radius: 18px;
        overflow: hidden;
        background: var(--surface-panel);
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.06), var(--surface-shadow-soft);
        transition: transform 0.2s ease, box-shadow 0.2s ease;
        backdrop-filter: blur(18px) saturate(140%);
        -webkit-backdrop-filter: blur(18px) saturate(140%);
    }
    
    .canvas-artifact:hover {
        box-shadow: inset 0 1px 0 rgba(255,255,255,0.06), var(--surface-glow);
        transform: translateY(-2px);
    }

    .canvas-artifact__toolbar {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 12px 16px;
        background: color-mix(in srgb, var(--accent) 5%, var(--surface-flat-soft) 95%);
        border-bottom: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, transparent);
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
        padding: 4px 10px;
        background: color-mix(in srgb, var(--accent) 16%, transparent);
        color: var(--accent);
        border-radius: 999px;
        white-space: nowrap;
        letter-spacing: 0.08em;
        text-transform: uppercase;
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
        background: var(--button-surface);
        border: 1px solid var(--border);
        border-radius: 6px;
        color: var(--text-secondary);
        font-size: 12px;
        font-weight: 500;
        padding: 4px 10px;
        cursor: pointer;
        box-shadow: inset 0 1px 0 var(--button-highlight), var(--button-soft-shadow);
        transition: all 0.2s ease;
    }

    .canvas-artifact__btn:hover {
        background: var(--button-surface-hover);
        color: var(--text-primary);
        border-color: color-mix(in srgb, var(--border) 72%, var(--accent) 28%);
        box-shadow: inset 0 1px 0 var(--button-highlight), var(--button-soft-shadow-hover);
        transform: translateY(-1px);
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

#[cfg(test)]
mod tests {
    use super::{terminal_event_body, terminal_event_title};
    use crate::api::types::ChatTerminalEvent;

    #[test]
    fn terminal_event_helpers_format_timeline_rows() {
        let log = ChatTerminalEvent {
            event: "log".to_string(),
            stream: Some("stderr".to_string()),
            text: Some("warning".to_string()),
            ..Default::default()
        };
        assert_eq!(terminal_event_title(&log), "log:stderr");
        assert_eq!(terminal_event_body(&log), "warning");

        let completed = ChatTerminalEvent {
            event: "completed".to_string(),
            exit_code: Some(0),
            ..Default::default()
        };
        assert_eq!(terminal_event_title(&completed), "completed");
        assert_eq!(terminal_event_body(&completed), "exit code 0");
    }
}
