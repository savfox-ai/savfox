use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{AgentEntry, ChatAttachment};
use crate::components::image_attachment::AttachmentPreview;
use crate::components::model_selector::ModelSelector;

/// ID used to locate the textarea in the DOM for height adjustment and IME listeners.
const TEXTAREA_ID: &str = "chat-input-textarea";

/// Maximum number of visible lines before the textarea scrolls.
const MAX_LINES: u32 = 10;
const TEXTAREA_MIN_HEIGHT_PX: f64 = 88.0;
const TEXTAREA_LINE_HEIGHT_PX: f64 = 22.0;
const TEXTAREA_VERTICAL_PADDING_PX: f64 = 24.0;
const MAX_IMAGE_DIMENSION: u32 = 1024;
const DEFAULT_IMAGE_QUALITY: f64 = 0.82;

/// Resets the textarea height to a single line.
fn reset_textarea_height() {
    if let Some(el) = get_textarea_element() {
        let _ = el.style().set_property("height", "auto");
        let _ = el
            .style()
            .set_property("height", &format!("{TEXTAREA_MIN_HEIGHT_PX}px"));
    }
}

/// Adjusts the textarea height based on its scroll content, clamped to MAX_LINES.
fn adjust_textarea_height() {
    if let Some(el) = get_textarea_element() {
        // Reset to auto so scrollHeight reflects actual content height
        let _ = el.style().set_property("height", "auto");
        let scroll_height = el.scroll_height();
        let max_height = TEXTAREA_LINE_HEIGHT_PX * MAX_LINES as f64 + TEXTAREA_VERTICAL_PADDING_PX;
        let clamped = (scroll_height as f64)
            .max(TEXTAREA_MIN_HEIGHT_PX)
            .min(max_height);
        let _ = el.style().set_property("height", &format!("{clamped}px"));
    }
}

/// Helper to fetch the textarea DOM element by ID.
fn get_textarea_element() -> Option<web_sys::HtmlTextAreaElement> {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(TEXTAREA_ID))
        .and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
}

fn parse_data_url(data_url: &str) -> Option<(String, String)> {
    let (meta, payload) = data_url.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    let mime = meta
        .strip_prefix("data:")
        .and_then(|prefix| prefix.strip_suffix(";base64"))
        .unwrap_or("application/octet-stream")
        .to_string();
    Some((mime, payload.to_string()))
}

fn compressed_filename(original_name: &str, mime_type: &str) -> String {
    let stem = original_name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(original_name);
    let ext = if mime_type == "image/png" {
        "png"
    } else {
        "jpg"
    };
    format!("{stem}.{ext}")
}

fn choose_output_mime(input_mime: &str) -> &'static str {
    match input_mime {
        "image/png" => "image/png",
        "image/webp" | "image/heic" | "image/heif" => "image/jpeg",
        _ => "image/jpeg",
    }
}

fn append_image_file(
    file: web_sys::File,
    mut attachments: Signal<Vec<ChatAttachment>>,
    image_quality: f64,
) {
    let input_mime = file.type_().to_ascii_lowercase();
    if !input_mime.starts_with("image/") {
        return;
    }

    let file_name = file.name();
    let fallback_mime = input_mime.clone();
    let reader = match web_sys::FileReader::new() {
        Ok(reader) => reader,
        Err(_) => return,
    };
    let reader_clone = reader.clone();
    let output_mime = choose_output_mime(&input_mime).to_string();

    let onload = Closure::wrap(Box::new(move |_: web_sys::Event| {
        if let Ok(result) = reader_clone.result() {
            if let Some(data_url) = result.as_string() {
                let Some(window) = web_sys::window() else {
                    return;
                };
                let Some(document) = window.document() else {
                    return;
                };
                let Ok(image) = web_sys::HtmlImageElement::new() else {
                    return;
                };
                let file_name_inner = file_name.clone();
                let fallback_mime_inner = fallback_mime.clone();
                let output_mime_inner = output_mime.clone();
                let mut attachments_inner = attachments;

                let image_for_onload = image.clone();
                let on_image_load = Closure::wrap(Box::new(move |_: web_sys::Event| {
                    let width = image_for_onload.natural_width();
                    let height = image_for_onload.natural_height();
                    if width == 0 || height == 0 {
                        return;
                    }

                    let max_dim = width.max(height);
                    let scale = if max_dim > MAX_IMAGE_DIMENSION {
                        MAX_IMAGE_DIMENSION as f64 / max_dim as f64
                    } else {
                        1.0
                    };
                    let target_w = ((width as f64) * scale).round().max(1.0) as u32;
                    let target_h = ((height as f64) * scale).round().max(1.0) as u32;

                    let Ok(canvas_el) = document.create_element("canvas") else {
                        return;
                    };
                    let Ok(canvas) = canvas_el.dyn_into::<web_sys::HtmlCanvasElement>() else {
                        return;
                    };
                    canvas.set_width(target_w);
                    canvas.set_height(target_h);

                    let Ok(Some(ctx)) = canvas.get_context("2d") else {
                        return;
                    };
                    let Ok(ctx2d) = ctx.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
                        return;
                    };
                    let _ = ctx2d.draw_image_with_html_image_element_and_dw_and_dh(
                        &image_for_onload,
                        0.0,
                        0.0,
                        target_w as f64,
                        target_h as f64,
                    );

                    let encoded = if output_mime_inner == "image/png" {
                        canvas.to_data_url_with_type("image/png").ok()
                    } else {
                        canvas
                            .to_data_url_with_type_and_encoder_options(
                                "image/jpeg",
                                &JsValue::from_f64(image_quality),
                            )
                            .ok()
                    };
                    let data_url_out = encoded.unwrap_or_default();
                    if data_url_out.is_empty() {
                        return;
                    }
                    if let Some((mime, data_base64)) = parse_data_url(&data_url_out) {
                        let mime_type = if mime.is_empty() {
                            fallback_mime_inner.clone()
                        } else {
                            mime
                        };
                        attachments_inner.write().push(ChatAttachment {
                            filename: compressed_filename(&file_name_inner, &mime_type),
                            mime_type,
                            data_base64,
                        });
                    }
                })
                    as Box<dyn FnMut(web_sys::Event)>);

                image.set_onload(Some(on_image_load.as_ref().unchecked_ref()));
                on_image_load.forget();
                let _ = image
                    .style()
                    .set_property("image-orientation", "from-image");
                image.set_src(&data_url);
            }
        }
    }) as Box<dyn FnMut(web_sys::Event)>);

    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
    onload.forget();
    let _ = reader.read_as_data_url(&file);
}

fn append_images_from_file_list(
    files: &web_sys::FileList,
    attachments: Signal<Vec<ChatAttachment>>,
    image_quality: f64,
) {
    for idx in 0..files.length() {
        if let Some(file) = files.get(idx) {
            append_image_file(file, attachments, image_quality);
        }
    }
}

fn open_image_picker(attachments: Signal<Vec<ChatAttachment>>, image_quality: f64) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else {
        return;
    };
    let Ok(input_el) = doc.create_element("input") else {
        return;
    };
    let Ok(input) = input_el.dyn_into::<web_sys::HtmlInputElement>() else {
        return;
    };
    input.set_type("file");
    input.set_multiple(true);
    input.set_accept("image/*");
    let _ = input.set_attribute("style", "display:none");

    let on_change = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let target = match event.target() {
            Some(target) => target,
            None => return,
        };
        let input: web_sys::HtmlInputElement = match target.dyn_into() {
            Ok(input) => input,
            Err(_) => return,
        };
        if let Some(files) = input.files() {
            append_images_from_file_list(&files, attachments, image_quality);
        }
        if let Some(parent) = input.parent_node() {
            let _ = parent.remove_child(&input);
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref());
    on_change.forget();

    if let Some(body) = doc.body() {
        let _ = body.append_child(&input);
        input.click();
    }
}

#[component]
pub fn ChatInput(
    on_send: EventHandler<(String, Vec<ChatAttachment>)>,
    on_abort: EventHandler<()>,
    streaming: bool,
    model_value: String,
    on_model_change: EventHandler<String>,
    agent_value: String,
    on_agent_change: EventHandler<String>,
    agents: Vec<AgentEntry>,
) -> Element {
    let mut text = use_signal(String::new);
    let mut attachments = use_signal(Vec::<ChatAttachment>::new);
    let mut composing = use_signal(|| false);
    let mut image_quality = use_signal(|| DEFAULT_IMAGE_QUALITY);

    // Attach compositionstart / compositionend listeners to the textarea via JS interop.
    // These fire during CJK / Korean IME input and let us avoid submitting on Enter
    // while the user is still composing characters.
    use_effect(move || {
        let Some(el) = get_textarea_element() else {
            return;
        };
        let target: &web_sys::EventTarget = el.as_ref();

        let comp_start = Closure::wrap(Box::new(move |_: web_sys::CompositionEvent| {
            composing.set(true);
        }) as Box<dyn FnMut(web_sys::CompositionEvent)>);

        let comp_end = Closure::wrap(Box::new(move |_: web_sys::CompositionEvent| {
            composing.set(false);
        }) as Box<dyn FnMut(web_sys::CompositionEvent)>);

        let _ = target.add_event_listener_with_callback(
            "compositionstart",
            comp_start.as_ref().unchecked_ref(),
        );
        let _ = target
            .add_event_listener_with_callback("compositionend", comp_end.as_ref().unchecked_ref());

        // Prevent the closures from being dropped while the element lives.
        comp_start.forget();
        comp_end.forget();
    });

    // Attach image paste + drag/drop handlers.
    use_effect(move || {
        let Some(el) = get_textarea_element() else {
            return;
        };
        let target: &web_sys::EventTarget = el.as_ref();

        let on_paste = Closure::wrap(Box::new(move |event: web_sys::ClipboardEvent| {
            let Some(clipboard) = event.clipboard_data() else {
                return;
            };
            let Some(files) = clipboard.files() else {
                return;
            };
            if files.length() == 0 {
                return;
            }
            append_images_from_file_list(&files, attachments, image_quality());
            event.prevent_default();
        }) as Box<dyn FnMut(web_sys::ClipboardEvent)>);

        let on_drag_over = Closure::wrap(Box::new(move |event: web_sys::DragEvent| {
            event.prevent_default();
        }) as Box<dyn FnMut(web_sys::DragEvent)>);

        let on_drop = Closure::wrap(Box::new(move |event: web_sys::DragEvent| {
            event.prevent_default();
            let Some(data_transfer) = event.data_transfer() else {
                return;
            };
            let Some(files) = data_transfer.files() else {
                return;
            };
            if files.length() == 0 {
                return;
            }
            append_images_from_file_list(&files, attachments, image_quality());
        }) as Box<dyn FnMut(web_sys::DragEvent)>);

        let _ = target.add_event_listener_with_callback("paste", on_paste.as_ref().unchecked_ref());
        let _ = target
            .add_event_listener_with_callback("dragover", on_drag_over.as_ref().unchecked_ref());
        let _ = target.add_event_listener_with_callback("drop", on_drop.as_ref().unchecked_ref());

        on_paste.forget();
        on_drag_over.forget();
        on_drop.forget();
    });

    let handle_submit = move |_| {
        let val = text().trim().to_string();
        if val.is_empty() && attachments.read().is_empty() || streaming {
            return;
        }
        let atts = attachments.read().clone();
        on_send((val, atts));
        text.set(String::new());
        attachments.write().clear();
        reset_textarea_height();
    };

    let handle_keydown = move |e: Event<KeyboardData>| {
        if e.key() == Key::Enter && !e.modifiers().shift() {
            e.prevent_default();
            // Don't submit while an IME composition is in progress.
            if composing() {
                return;
            }
            let val = text().trim().to_string();
            if val.is_empty() && attachments.read().is_empty() || streaming {
                return;
            }
            let atts = attachments.read().clone();
            on_send((val, atts));
            text.set(String::new());
            attachments.write().clear();
            reset_textarea_height();
        }
    };

    let handle_input = move |e: Event<FormData>| {
        text.set(e.value());
        adjust_textarea_height();
    };

    let handle_remove_attachment = move |idx: usize| {
        attachments.write().remove(idx);
    };
    let handle_pick_images = move |_| {
        open_image_picker(attachments, image_quality());
    };
    let model_display_name = {
        let raw = model_value.trim().to_string();
        if raw.is_empty() || raw == "default" {
            "Default model".to_string()
        } else {
            raw
        }
    };
    let agent_display_name = {
        let raw = agent_value.trim().to_string();
        agents
            .iter()
            .find(|a| a.id.as_deref().filter(|id| !id.is_empty()) == Some(&raw) || a.name == raw)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| {
                if raw.is_empty() || raw == "default" {
                    "Default".to_string()
                } else {
                    raw
                }
            })
    };

    rsx! {
        div { class: "chat-input-area",
            // Attachment preview
            AttachmentPreview {
                attachments: attachments.read().clone(),
                on_remove: handle_remove_attachment,
            }

            div { class: "chat-input",
                div { class: "chat-input-model-badge",
                    span { class: "chat-input-model-badge__label", "Agent:" }
                    span { class: "chat-input-model-badge__name", "{agent_display_name}" }
                    span { class: "chat-input-model-badge__sep", "|" }
                    span { class: "chat-input-model-badge__label", "Model:" }
                    span { class: "chat-input-model-badge__name", "{model_display_name}" }
                }
                textarea {
                    id: TEXTAREA_ID,
                    value: "{text}",
                    oninput: handle_input,
                    onkeydown: handle_keydown,
                    placeholder: "Type your message... (Enter to send, Shift+Enter for newline)",
                    rows: 3,
                    class: "chat-textarea",
                    aria_label: "Session message input",
                }
                div { class: "chat-input-actions",
                    div { class: "chat-input-actions-left",
                        button {
                            onclick: handle_pick_images,
                            class: "chat-icon-btn chat-icon-btn-attach",
                            title: "Attach image",
                            aria_label: "Attach image",
                            "+"
                        }
                        div { class: "chat-agent-select-wrap",
                            select {
                                class: "chat-agent-select",
                                value: "{agent_value}",
                                aria_label: "Select agent",
                                onchange: move |e: Event<FormData>| on_agent_change(e.value()),
                                for agent in agents.iter() {
                                    {
                                        let ref_id = agent.id.as_deref()
                                            .filter(|id| !id.trim().is_empty())
                                            .unwrap_or(&agent.name);
                                        let label = if agent.is_default.unwrap_or(false) {
                                            format!("{} (default)", agent.name)
                                        } else {
                                            agent.name.clone()
                                        };
                                        rsx! {
                                            option {
                                                value: "{ref_id}",
                                                selected: ref_id == agent_value,
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "chat-model-select-wrap",
                            ModelSelector {
                                value: model_value,
                                on_change: move |model: String| on_model_change(model),
                            }
                        }
                        button {
                            class: "chat-effort-btn",
                            title: "Reasoning effort",
                            aria_label: "Reasoning effort",
                            "Reasoning: Default"
                        }
                    }
                    div { class: "chat-input-actions-right",
                        div { class: "chat-input-hint", "Enter to send • Shift+Enter for new line" }
                        if streaming {
                            button {
                                onclick: move |_| on_abort(()),
                                class: "chat-icon-btn chat-icon-btn-stop",
                                aria_label: "Stop generation",
                                "■"
                            }
                        } else {
                            {
                                let disabled = text().trim().is_empty() && attachments.read().is_empty();
                                let class = if disabled {
                                    "chat-icon-btn chat-icon-btn-send disabled"
                                } else {
                                    "chat-icon-btn chat-icon-btn-send"
                                };
                                rsx! {
                                    button {
                                        onclick: handle_submit,
                                        disabled: "{disabled}",
                                        class: "{class}",
                                        aria_label: "Send message",
                                        title: "Send message",
                                        "↑"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        style { {CHAT_INPUT_STYLES} }
    }
}

const CHAT_INPUT_STYLES: &str = r#"
    .chat-input-area {
        border-top: 1px solid var(--border);
        background: linear-gradient(
            180deg,
            color-mix(in srgb, var(--bg-secondary) 92%, var(--accent) 8%) 0%,
            var(--bg-secondary) 100%
        );
        padding: 12px;
        flex-shrink: 0;
    }

    .chat-input {
        display: flex;
        flex-direction: column;
        gap: 10px;
        padding: 12px 12px 10px;
        border: 1px solid color-mix(in srgb, var(--border) 70%, var(--accent) 30%);
        border-radius: 16px;
        background: linear-gradient(
            180deg,
            color-mix(in srgb, var(--bg-tertiary) 92%, white 8%) 0%,
            var(--bg-tertiary) 100%
        );
        box-shadow: 0 8px 28px rgba(0, 0, 0, 0.18);
    }

    .chat-input:focus-within {
        border-color: color-mix(in srgb, var(--accent) 78%, var(--border) 22%);
        box-shadow:
            0 0 0 2px color-mix(in srgb, var(--accent) 28%, transparent),
            0 10px 32px rgba(0, 0, 0, 0.2);
    }

    .chat-input-model-badge {
        display: flex;
        align-items: center;
        gap: 5px;
        padding: 0 6px 4px;
        font-size: 11px;
        line-height: 1;
    }

    .chat-input-model-badge__label {
        color: var(--text-muted);
        font-weight: 500;
    }

    .chat-input-model-badge__name {
        color: var(--text-secondary);
        font-weight: 600;
        max-width: 260px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .chat-input-model-badge__sep {
        color: var(--border);
        margin: 0 2px;
    }

    .chat-textarea {
        width: 100%;
        padding: 10px 6px;
        background: transparent;
        border: none;
        color: var(--text-primary);
        outline: none;
        resize: none;
        min-height: 88px;
        max-height: 260px;
        font-size: 15px;
        line-height: 1.45;
        overflow-y: auto;
    }

    .chat-textarea::placeholder {
        color: var(--text-muted);
    }

    .chat-input-actions {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 8px;
    }

    .chat-input-actions-left {
        display: flex;
        align-items: center;
        gap: 8px;
    }

    .chat-agent-select-wrap {
        min-width: 140px;
    }

    .chat-agent-select {
        height: 38px;
        min-width: 140px;
        padding: 0 12px;
        border: 1px solid var(--border);
        border-radius: 10px;
        background: color-mix(in srgb, var(--bg-tertiary) 90%, var(--bg-secondary) 10%);
        color: var(--text-primary);
        font-size: 12px;
        font-weight: 500;
        cursor: pointer;
        outline: none;
        transition: border-color 0.2s ease;
        appearance: auto;
    }

    .chat-agent-select:hover {
        border-color: color-mix(in srgb, var(--border) 60%, var(--accent) 40%);
    }

    .chat-agent-select:focus {
        border-color: color-mix(in srgb, var(--accent) 60%, var(--border) 40%);
        box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 20%, transparent);
    }

    .chat-model-select-wrap {
        min-width: 210px;
    }

    .chat-model-select-wrap .model-selector__trigger {
        min-width: 210px;
        height: 38px;
    }

    .chat-effort-btn {
        height: 38px;
        border-radius: 10px;
        border: 1px solid var(--border);
        background: color-mix(in srgb, var(--bg-secondary) 88%, white 12%);
        color: var(--text-primary);
        padding: 0 12px;
        font-size: 12px;
        font-weight: 500;
        white-space: nowrap;
        cursor: pointer;
    }

    .chat-effort-btn:hover {
        background: var(--bg-hover);
    }

    .chat-input-actions-right {
        margin-left: auto;
        display: flex;
        align-items: center;
        gap: 8px;
    }

    .chat-input-hint {
        font-size: 11px;
        color: var(--text-muted);
        line-height: 1;
    }

    .chat-icon-btn {
        width: 38px;
        height: 38px;
        border-radius: 10px;
        border: 1px solid var(--border);
        display: inline-flex;
        align-items: center;
        justify-content: center;
        font-size: 20px;
        line-height: 1;
        transition: transform 0.14s ease, opacity 0.14s ease, border-color 0.14s ease;
        user-select: none;
    }

    .chat-icon-btn:active {
        transform: scale(0.96);
    }

    .chat-icon-btn-attach {
        background: color-mix(in srgb, var(--bg-secondary) 88%, white 12%);
        color: var(--text-primary);
    }

    .chat-icon-btn-send {
        background: var(--accent);
        border-color: var(--accent);
        color: #fff;
    }

    .chat-icon-btn-stop {
        background: var(--danger);
        border-color: var(--danger);
        color: #fff;
        font-size: 14px;
    }

    .chat-icon-btn-send.disabled {
        opacity: 0.42;
    }

    @media screen and (max-width: 640px) {
        .chat-input-area {
            padding: 10px;
        }

        .chat-input {
            padding: 10px 10px 8px;
            border-radius: 14px;
        }

        .chat-textarea {
            min-height: 78px;
            font-size: 16px;
            padding: 8px 4px;
        }

        .chat-icon-btn {
            width: 36px;
            height: 36px;
        }

        .chat-agent-select-wrap {
            min-width: 110px;
        }

        .chat-agent-select {
            height: 36px;
            min-width: 110px;
            font-size: 11px;
        }

        .chat-model-select-wrap {
            min-width: 160px;
        }

        .chat-model-select-wrap .model-selector__trigger {
            min-width: 160px;
            height: 36px;
        }

        .chat-effort-btn {
            height: 36px;
            font-size: 11px;
            padding: 0 10px;
        }

        .chat-input-hint {
            display: none;
        }
    }

    @media (hover: none) and (pointer: coarse) {
        .chat-textarea {
            font-size: 16px;
        }
    }
"#;
