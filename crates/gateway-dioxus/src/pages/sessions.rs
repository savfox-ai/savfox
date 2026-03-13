use std::collections::HashMap;

use dioxus::prelude::*;
use savfox_gateway_shared::{derive_session_label, is_internal_session_message};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;

use crate::api::client::stream_chat;
use crate::api::types::{
    AgentEntry, AgentsResponse, ChatAttachment, ChatMessage, SessionEntry, SessionsResponse,
};
use crate::api::ws::WsRpc;
use crate::components::chat_input::ChatInput;
use crate::components::chat_message::ChatMessageBubble;
use crate::components::copy_button::CopyButton;
use crate::components::markdown_renderer::MarkdownRenderer;
use crate::components::search_input::SearchInput;
use crate::components::skeleton::*;

const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];
const REASONING_MODES: [&str; 3] = ["off", "on", "stream"];
const VERBOSE_MODES: [&str; 3] = ["off", "on", "full"];
const PENDING_MODEL_STORAGE_KEY: &str = "savfox_pending_session_model";

fn normalize_reasoning_mode(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if REASONING_MODES.iter().any(|mode| *mode == normalized) {
        normalized
    } else {
        "on".to_string()
    }
}

fn normalize_verbose_mode(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if VERBOSE_MODES.iter().any(|mode| *mode == normalized) {
        normalized
    } else {
        "on".to_string()
    }
}

fn sort_sessions(mut entries: Vec<SessionEntry>) -> Vec<SessionEntry> {
    entries.sort_by(|a, b| {
        let a_activity = a.last_activity.as_deref().unwrap_or("");
        let b_activity = b.last_activity.as_deref().unwrap_or("");
        b_activity.cmp(a_activity)
    });
    entries
}

fn should_show_thinking_for_message(mode: &str, is_streaming: bool, is_last: bool) -> bool {
    match mode {
        "off" => false,
        "on" => !(is_streaming && is_last),
        "stream" => true,
        _ => true,
    }
}

fn find_agent_entry<'a>(agents: &'a [AgentEntry], selected: &str) -> Option<&'a AgentEntry> {
    agents
        .iter()
        .find(|entry| entry.id.as_deref() == Some(selected) || entry.name == selected)
}

fn primary_model_for_agent(entry: &AgentEntry) -> Option<String> {
    entry
        .models
        .as_ref()
        .and_then(|models| models.primary.clone())
        .or_else(|| entry.model.clone())
}

fn parse_history_messages(payload: &Value) -> Vec<ChatMessage> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(parse_history_message)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn parse_history_message(item: &Value) -> Option<ChatMessage> {
    let role = item
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("assistant")
        .to_string();
    let content = item
        .get("text")
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() || is_internal_session_message(&content) {
        return None;
    }

    Some(ChatMessage {
        role,
        content,
        attachments: vec![],
        timestamp: None,
        thinking: None,
    })
}

fn load_pending_session_model() -> Option<String> {
    web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
        .and_then(|storage| storage.get_item(PENDING_MODEL_STORAGE_KEY).ok())
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn save_pending_session_model(value: Option<&str>) {
    let Some(storage) = web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
    else {
        return;
    };

    if let Some(model) = value {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            let _ = storage.remove_item(PENDING_MODEL_STORAGE_KEY);
        } else {
            let _ = storage.set_item(PENDING_MODEL_STORAGE_KEY, trimmed);
        }
    } else {
        let _ = storage.remove_item(PENDING_MODEL_STORAGE_KEY);
    }
}

fn build_openai_chat_message_payload(prompt: &str, attachments: &[ChatAttachment]) -> Value {
    if attachments.is_empty() {
        return json!({
            "role": "user",
            "content": prompt,
        });
    }

    let mut parts = Vec::new();
    if !prompt.is_empty() {
        parts.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }

    for attachment in attachments {
        if !attachment
            .mime_type
            .to_ascii_lowercase()
            .starts_with("image/")
        {
            continue;
        }
        let data_url = format!(
            "data:{};base64,{}",
            attachment.mime_type, attachment.data_base64
        );
        parts.push(json!({
            "type": "image_url",
            "image_url": { "url": data_url },
        }));
    }

    if parts.is_empty() {
        json!({
            "role": "user",
            "content": prompt,
        })
    } else {
        json!({
            "role": "user",
            "content": parts,
        })
    }
}

async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[component]
pub fn Sessions() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();

    let mut messages = use_signal(Vec::<ChatMessage>::new);
    let mut session_buffers = use_signal(HashMap::<String, Vec<ChatMessage>>::new);
    let mut streaming = use_signal(|| false);

    let initial_pending_model = load_pending_session_model();
    let initial_selected_model = initial_pending_model
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut selected_model = use_signal(move || initial_selected_model.clone());
    let mut pending_session_model = use_signal(move || initial_pending_model.clone());
    let mut thinking_level = use_signal(|| "medium".to_string());
    let mut reasoning_mode = use_signal(|| "on".to_string());
    let mut verbose_mode = use_signal(|| "on".to_string());
    let mut selected_agent = use_signal(|| "default".to_string());

    let mut current_session_id = use_signal(|| Option::<String>::None);
    let mut session_refresh_tick = use_signal(|| 0u32);
    let mut session_list_cache = use_signal(Vec::<SessionEntry>::new);
    let mut session_actions_open_for = use_signal(|| Option::<String>::None);
    let mut confirm_delete_id = use_signal(|| Option::<String>::None);
    let mut sessions_loaded_once = use_signal(|| false);
    let mut sessions_retry_worker_running = use_signal(|| false);
    let mut initial_sessions_refresh_done = use_signal(|| false);
    let mut abort_ctl = use_signal(|| Option::<web_sys::AbortController>::None);
    let mut loading_session = use_signal(|| false);

    let mut sidebar_content = use_signal(|| Option::<String>::None);
    let mut session_search_query = use_signal(String::new);

    let ws_sessions = ws.clone();
    let sessions_data = use_resource(move || {
        let _connected = ws_connected();
        let _tick = session_refresh_tick();
        let ws = ws_sessions.clone();
        async move {
            ws.call::<SessionsResponse>("sessions.list", None)
                .await
                .map(|resp| resp.entries)
        }
    });

    let ws_agents = ws.clone();
    let agents_data = use_resource(move || {
        let _connected = ws_connected();
        let ws = ws_agents.clone();
        async move {
            ws.call::<AgentsResponse>("agents.list", None)
                .await
                .map(|resp| resp.agents)
                .unwrap_or_default()
        }
    });

    use_effect(move || {
        let agents_snapshot = agents_data.read().as_ref().cloned().unwrap_or_default();
        let preserve_draft_model =
            current_session_id().is_none() && pending_session_model().is_some();
        if let Some(agent) = find_agent_entry(&agents_snapshot, "default") {
            if let Some(level) = agent.thinking.as_deref() {
                if THINKING_LEVELS.iter().any(|candidate| *candidate == level) {
                    thinking_level.set(level.to_string());
                }
            }
            if let Some(mode) = agent.verbose.as_deref() {
                verbose_mode.set(normalize_verbose_mode(mode));
            }
            if !preserve_draft_model {
                if let Some(primary) = primary_model_for_agent(agent) {
                    let model_name = primary.clone();
                    selected_model.set(primary);
                    web_sys::console::log_1(&format!("Model set to: {}", model_name).into());
                } else {
                    selected_model.set("default".to_string());
                }
            }
        } else if !preserve_draft_model {
            selected_model.set("default".to_string());
            web_sys::console::log_1(&"Model set to: default".into());
        }
    });

    // Keep the sidebar list stable across transient WS-RPC failures.
    use_effect(move || {
        if let Some(Ok(entries)) = sessions_data.read().as_ref() {
            session_list_cache.set(entries.clone());
            sessions_loaded_once.set(true);
        }
    });

    // Ensure sessions.list is retried once after WS reaches OPEN.
    use_effect(move || {
        let connected = ws_connected();
        web_sys::console::log_1(
            &format!(
                "WebSocket status: {}, initial_refresh_done: {}",
                connected,
                initial_sessions_refresh_done()
            )
            .into(),
        );
        if connected && !initial_sessions_refresh_done() {
            initial_sessions_refresh_done.set(true);
            session_refresh_tick += 1;
        } else if !connected && initial_sessions_refresh_done() {
            initial_sessions_refresh_done.set(false);
        }
    });

    // Retry initial sessions fetch a few times after WS is connected.
    // This avoids a startup race where the first RPC call can fail before
    // the socket is fully ready.
    use_effect(move || {
        if !ws_connected() {
            sessions_loaded_once.set(false);
            return;
        }
        if sessions_loaded_once() || sessions_retry_worker_running() {
            return;
        }
        sessions_retry_worker_running.set(true);
        spawn(async move {
            web_sys::console::log_1(&"Starting session loading retry loop".into());
            for i in 0..12 {
                if sessions_loaded_once() || !ws_connected() {
                    break;
                }
                web_sys::console::log_1(
                    &format!("Retrying session load, attempt: {}", i + 1).into(),
                );
                session_refresh_tick += 1;
                sleep_ms(400).await;
            }
            sessions_retry_worker_running.set(false);
            web_sys::console::log_1(&"Session loading retry loop completed".into());
        });
    });

    use_effect(move || {
        let _len = messages.read().len();
        if let Some(doc) = web_sys::window().and_then(|window| window.document()) {
            if let Some(el) = doc.get_element_by_id("chat-messages") {
                el.set_scroll_top(el.scroll_height());
            }
        }
    });

    let on_new_session = move |_| {
        if let Some(controller) = abort_ctl.write().take() {
            controller.abort();
        }
        streaming.set(false);
        loading_session.set(false);
        current_session_id.set(None);
        messages.write().clear();
        sidebar_content.set(None);
        if let Some(pending) = pending_session_model() {
            selected_model.set(pending);
        }
    };

    let on_clear = move |_| {
        messages.write().clear();
        if let Some(session_id) = current_session_id() {
            session_buffers.write().insert(session_id, vec![]);
        }
    };

    let on_expand_tool = move |content: String| {
        sidebar_content.set(Some(content));
    };

    let ws_for_send = ws.clone();
    let on_send = move |(text, attachments): (String, Vec<ChatAttachment>)| {
        if streaming() {
            return;
        }

        let prompt = text.trim().to_string();
        if prompt.is_empty() && attachments.is_empty() {
            return;
        }

        // Log user message
        web_sys::console::log_1(
            &format!(
                "User message: '{}', attachments: {}",
                prompt,
                attachments.len()
            )
            .into(),
        );

        let active_model = pending_session_model().unwrap_or_else(|| selected_model());
        let active_thinking = thinking_level();
        let active_reasoning = normalize_reasoning_mode(&reasoning_mode());
        let active_verbose = normalize_verbose_mode(&verbose_mode());
        let ws_send = ws_for_send.clone();

        spawn(async move {
            let mut session_id = current_session_id();

            if session_id.is_none() {
                let session_label = derive_session_label(Some(&prompt), None, None)
                    .unwrap_or_else(|| "New Session".to_string());
                let patch_model = active_model.clone();
                let patch_reasoning = active_reasoning.clone();
                let patch_verbose = active_verbose.clone();
                let patch_model_override = patch_model.clone();
                let patch_result = ws_send
                    .call::<Value>(
                        "sessions.patch",
                        Some(json!({
                            "label": session_label,
                            "model": patch_model,
                            "overrides": {
                                "model": patch_model_override,
                                "thinking": active_thinking,
                                "reasoning": patch_reasoning,
                                "verbose": patch_verbose,
                            }
                        })),
                    )
                    .await;
                if let Ok(payload) = patch_result {
                    if let Some(new_session_id) = payload
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                    {
                        current_session_id.set(Some(new_session_id.clone()));
                        session_id = Some(new_session_id);
                        pending_session_model.set(None);
                        save_pending_session_model(None);
                    }
                }
                session_refresh_tick += 1;
            } else if let Some(existing_session_id) = session_id.clone() {
                let patch_model = active_model.clone();
                let patch_reasoning = active_reasoning.clone();
                let patch_verbose = active_verbose.clone();
                let patch_model_override = patch_model.clone();
                let _ = ws_send
                    .call::<Value>(
                        "sessions.patch",
                        Some(json!({
                            "session_id": existing_session_id,
                            "model": patch_model,
                            "overrides": {
                                "model": patch_model_override,
                                "thinking": active_thinking,
                                "reasoning": patch_reasoning,
                                "verbose": patch_verbose,
                            }
                        })),
                    )
                    .await;
            }

            messages.write().push(ChatMessage {
                role: "user".to_string(),
                content: prompt.clone(),
                attachments: attachments.clone(),
                timestamp: None,
                thinking: None,
            });
            messages.write().push(ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
                attachments: vec![],
                timestamp: None,
                thinking: None,
            });
            streaming.set(true);

            let controller = web_sys::AbortController::new().ok();
            let signal = controller.as_ref().map(|ctl| ctl.signal());
            abort_ctl.set(controller);

            let capture_reasoning = active_reasoning != "off";
            let mut reasoning_messages = messages;
            let mut reasoning_cb = move |chunk: &str| {
                web_sys::console::log_1(&format!("Reasoning chunk: '{}'", chunk).into());
                if !capture_reasoning {
                    return;
                }
                if let Some(last) = reasoning_messages.write().last_mut() {
                    let existing = last.thinking.clone().unwrap_or_default();
                    last.thinking = Some(existing + chunk);
                }
            };

            let mut content_messages = messages;
            web_sys::console::log_1(
                &format!(
                    "Starting stream with model: {}, session: {:?}",
                    active_model, session_id
                )
                .into(),
            );

            let outbound_message = build_openai_chat_message_payload(&prompt, &attachments);
            let stream_result = stream_chat(
                outbound_message,
                active_model.clone(),
                session_id.clone(),
                |chunk: &str| {
                    web_sys::console::log_1(&format!("Assistant chunk: '{}'", chunk).into());
                    if let Some(last) = content_messages.write().last_mut() {
                        last.content.push_str(chunk);
                    }
                },
                Some(&mut reasoning_cb),
                signal,
            )
            .await;

            web_sys::console::log_1(
                &format!("Stream completed with result: {:?}", stream_result).into(),
            );

            if let Err(err) = stream_result {
                web_sys::console::log_1(&format!("Stream error: {}", err).into());
                if !err.to_lowercase().contains("abort") {
                    if let Some(last) = messages.write().last_mut() {
                        if last.content.is_empty() {
                            last.content = format!("Error: {err}");
                        }
                    }
                }
            } else {
                web_sys::console::log_1(&"Stream completed successfully".into());
            }

            streaming.set(false);
            abort_ctl.set(None);

            if let Some(key) = session_id {
                session_buffers.write().insert(key, messages.read().clone());
                session_refresh_tick += 1;
            }
        });
    };

    let on_abort = move |_: ()| {
        if let Some(controller) = abort_ctl.write().take() {
            controller.abort();
        }
        streaming.set(false);
    };

    let sessions = sort_sessions(session_list_cache());
    let active_session_id = current_session_id();
    let active_session_label = active_session_id
        .as_deref()
        .and_then(|session_id| {
            sessions
                .iter()
                .find(|entry| {
                    entry.session_id.as_deref() == Some(session_id)
                        || entry.id.as_deref() == Some(session_id)
                })
                .map(SessionEntry::display_label)
        })
        .unwrap_or_else(|| "Draft Session".to_string());

    let msgs: Vec<ChatMessage> = messages.read().clone();
    let message_count = msgs.len();
    let has_sidebar = sidebar_content().is_some();
    let agents_list: Vec<AgentEntry> = agents_data.read().as_ref().cloned().unwrap_or_default();

    rsx! {
        div { class: "session-page",
            div { class: "session-shell",
                aside { class: "session-list-pane",
                    div { class: "session-list-head",
                        h3 { class: "session-list-title", "Sessions" }
                        button {
                            class: "session-list-new-btn",
                            onclick: on_new_session,
                            "+ New Session"
                        }
                    }
                    div { style: "padding:8px 12px;border-bottom:1px solid color-mix(in srgb, var(--border) 40%, transparent);",
                        SearchInput {
                            value: session_search_query(),
                            on_change: move |v: String| session_search_query.set(v),
                            placeholder: "Filter sessions...".to_string(),
                        }
                    }
                    div { class: "session-list-scroll",
                        {
                            let search_q = session_search_query().trim().to_ascii_lowercase();
                            let filtered_sessions: Vec<&SessionEntry> = sessions.iter().filter(|entry| {
                                if search_q.is_empty() {
                                    true
                                } else {
                                    entry.display_label().to_ascii_lowercase().contains(&search_q)
                                }
                            }).collect();
                            rsx! {
                        if filtered_sessions.is_empty() {
                            div { class: "session-list-empty",
                                if sessions.is_empty() {
                                    "No saved sessions yet"
                                } else {
                                    "No matching sessions"
                                }
                            }
                        } else {
                            for entry in filtered_sessions.into_iter() {
                                {
                                    let sid = entry.display_id();
                                    let sid_for_click = sid.clone();
                                    let sid_for_delete = sid.clone();
                                    let item_title = entry.display_label();
                                    let item_preview = {
                                        let label = entry.display_label();
                                        let preview_text: String = label.chars().take(50).collect();
                                        let count = entry.display_count();
                                        if count > 0 {
                                            format!("{} msg{}", count, if count == 1 { "" } else { "s" })
                                        } else if preview_text.len() < label.len() {
                                            format!("{}...", preview_text)
                                        } else {
                                            String::new()
                                        }
                                    };
                                    let item_model = entry.model.as_deref().unwrap_or("").to_string();
                                    let item_meta = entry.last_activity.as_deref().unwrap_or("-").to_string();
                                    let item_active = active_session_id.as_deref() == Some(sid.as_str());
                                    let item_class = if item_active {
                                        "session-list-item session-list-item--active"
                                    } else {
                                        "session-list-item"
                                    };
                                    let ws_history = ws.clone();
                                    let ws_delete = ws.clone();
                                    let entry_model = entry.model.clone();
                                    let entry_provider = entry.provider.clone();
                                    let entry_thread_id = entry.thread_id.clone();
                                    let on_delete_session = {
                                        let sid_for_delete = sid_for_delete.clone();
                                        let ws_delete = ws_delete.clone();
                                        move |_| {
                                            let sid = sid_for_delete.clone();
                                            let ws = ws_delete.clone();
                                            confirm_delete_id.set(None);
                                            spawn(async move {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "sessions.delete",
                                                    Some(json!({ "session_id": sid })),
                                                ).await;
                                                session_refresh_tick += 1;
                                                if current_session_id() == Some(sid.clone()) {
                                                    current_session_id.set(None);
                                                    messages.write().clear();
                                                }
                                                session_buffers.write().remove(&sid);
                                            });
                                        }
                                    };
                                    rsx! {
                                        div {
                                            key: "{sid}",
                                            class: "session-list-item-wrapper",
                                            button {
                                                class: "{item_class}",
                                                onclick: move |_| {
                                                    println!("[DEBUG] Session menu item clicked: session_id={}", sid_for_click);
                                                    current_session_id.set(Some(sid_for_click.clone()));
                                                    sidebar_content.set(None);
                                                    pending_session_model.set(None);
                                                    save_pending_session_model(None);

                                                    // Update selected_model from session entry if available
                                                    if let Some(model) = entry_model.clone() {
                                                        selected_model.set(model);
                                                    } else {
                                                        selected_model.set("default".to_string());
                                                    }

                                                    if let Some(cached) = session_buffers.read().get(&sid_for_click).cloned() {
                                                        println!("[DEBUG] Loading session from cache: session_id={}, messages={}", sid_for_click, cached.len());
                                                        loading_session.set(false);
                                                        messages.set(cached);
                                                        return;
                                                    }

                                                    println!("[DEBUG] Fetching session history from server: session_id={}", sid_for_click);
                                                    loading_session.set(true);
                                                    messages.write().clear();
                                                    // Use thread_id for history lookup if available, otherwise use session_id
                                                    let lookup_key = entry_thread_id.clone().unwrap_or_else(|| sid_for_click.clone());
                                                    let session_for_fetch = sid_for_click.clone();
                                                    let session_for_payload = lookup_key.clone();
                                                    let ws = ws_history.clone();
                                                    spawn(async move {
                                                        if let Ok(payload) = ws
                                                            .call::<Value>(
                                                                "chat.history",
                                                                Some(json!({
                                                                    "session_id": session_for_payload,
                                                                    "limit": 200,
                                                                })),
                                                            )
                                                            .await
                                                        {
                                                            // Debug: Print rollout_path from response
                                                            if let Some(rollout_path) = payload.get("rollout_path").and_then(|v| v.as_str()) {
                                                                println!("[DEBUG] Session history loaded: session_id={}, rollout_path={}", session_for_fetch, rollout_path);
                                                            }
                                                            let parsed = parse_history_messages(&payload);
                                                            println!("[DEBUG] Parsed {} messages from history for session_id={}", parsed.len(), session_for_fetch);
                                                            session_buffers
                                                                .write()
                                                                .insert(session_for_fetch, parsed.clone());
                                                            messages.set(parsed);
                                                            loading_session.set(false);
                                                        } else {
                                                            println!("[DEBUG] Failed to load session history: session_id={}", session_for_fetch);
                                                            loading_session.set(false);
                                                        }
                                                    });
                                                },
                                                div { class: "session-list-item__title", "{item_title}" }
                                                if !item_preview.is_empty() || !item_model.is_empty() {
                                                    div { class: "session-list-item__preview",
                                                        if !item_model.is_empty() {
                                                            span { class: "session-list-item__model", "{item_model}" }
                                                        }
                                                        if !item_preview.is_empty() {
                                                            span { "{item_preview}" }
                                                        }
                                                    }
                                                }
                                                div { class: "session-list-item__meta", "{item_meta}" }
                                            }
                                            if confirm_delete_id() == Some(sid.clone()) {
                                                div { class: "session-delete-confirm",
                                                    span { class: "session-delete-confirm__label", "Delete?" }
                                                    button {
                                                        class: "session-delete-confirm__yes",
                                                        onclick: on_delete_session,
                                                        "Yes"
                                                    }
                                                    button {
                                                        class: "session-delete-confirm__no",
                                                        onclick: move |_| confirm_delete_id.set(None),
                                                        "No"
                                                    }
                                                }
                                            } else {
                                                {
                                                    let sid_for_confirm = sid.clone();
                                                    rsx! {
                                                        button {
                                                            class: "session-list-item__delete",
                                                            onclick: move |_| confirm_delete_id.set(Some(sid_for_confirm.clone())),
                                                            title: "Delete session",
                                                            aria_label: "Delete session",
                                                            "\u{00d7}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                            }
                        }
                    }
                }

                section { class: "session-main",
                    div { class: "session-main-header",
                        div { class: "session-main-header__left",
                            h2 { class: "session-main-title", "Session" }
                            span { class: "session-main-subtitle", "{active_session_label}" }
                            if message_count > 0 {
                                span { class: "session-main-badge", "{message_count} msgs" }
                            }
                        }
                        div { class: "session-main-header__right",
                            if message_count > 0 {
                                button {
                                    class: "session-pill-btn",
                                    onclick: {
                                        let export_msgs = msgs.clone();
                                        let export_session_id = active_session_id.clone();
                                        move |_| {
                                            let msgs = export_msgs.clone();
                                            let session_id = export_session_id.clone();
                                            // Build markdown content
                                            let mut md = String::new();
                                            for msg in msgs.iter() {
                                                let role_header = match msg.role.as_str() {
                                                    "user" => "## User",
                                                    "assistant" => "## Assistant",
                                                    other => {
                                                        let capitalized: String = other.chars().enumerate().map(|(i, c)| {
                                                            if i == 0 { c.to_ascii_uppercase() } else { c }
                                                        }).collect();
                                                        // Leak avoided: use format then push
                                                        md.push_str(&format!("## {}\n\n", capitalized));
                                                        if let Some(ref ts) = msg.timestamp {
                                                            md.push_str(&format!("<!-- {} -->\n\n", ts));
                                                        }
                                                        md.push_str(&msg.content);
                                                        md.push_str("\n\n---\n\n");
                                                        continue;
                                                    }
                                                };
                                                md.push_str(role_header);
                                                md.push_str("\n\n");
                                                if let Some(ref ts) = msg.timestamp {
                                                    md.push_str(&format!("<!-- {} -->\n\n", ts));
                                                }
                                                md.push_str(&msg.content);
                                                md.push_str("\n\n---\n\n");
                                            }

                                            // Trigger browser download
                                            let filename = if let Some(ref sid) = session_id {
                                                format!("session-{}.md", sid)
                                            } else {
                                                "session-draft.md".to_string()
                                            };

                                            if let Some(window) = web_sys::window() {
                                                if let Some(document) = window.document() {
                                                    let blob_parts = js_sys::Array::new();
                                                    blob_parts.push(&wasm_bindgen::JsValue::from_str(&md));
                                                    let mut opts = web_sys::BlobPropertyBag::new();
                                                    opts.set_type("text/markdown;charset=utf-8");
                                                    if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&blob_parts, &opts) {
                                                        if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                                                            if let Ok(el) = document.create_element("a") {
                                                                let anchor: web_sys::HtmlAnchorElement = el.unchecked_into();
                                                                anchor.set_href(&url);
                                                                anchor.set_download(&filename);
                                                                anchor.style().set_property("display", "none").ok();
                                                                if let Some(body) = document.body() {
                                                                    body.append_child(&anchor).ok();
                                                                    anchor.click();
                                                                    body.remove_child(&anchor).ok();
                                                                }
                                                                web_sys::Url::revoke_object_url(&url).ok();
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    },
                                    "Export"
                                }
                            }
                            button {
                                class: "session-pill-btn",
                                onclick: on_new_session,
                                "New"
                            }
                            button {
                                class: "session-pill-btn",
                                onclick: on_clear,
                                "Clear"
                            }
                        }
                    }

                    div {
                        class: if has_sidebar { "chat-body chat-body--split" } else { "chat-body" },
                        div {
                            id: "chat-messages",
                            class: "chat-messages",
                            role: "log",
                            aria_live: "polite",
                            aria_label: "Session messages",

                            if loading_session() {
                                div { class: "chat-empty",
                                    SkeletonLines { count: 3 }
                                }
                            } else if msgs.is_empty() {
                                div { class: "chat-empty",
                                    "Start a new session with your first message"
                                }
                            }

                            for (i, msg) in msgs.iter().enumerate() {
                                {
                                    let prev_same = if i > 0 {
                                        msgs[i - 1].role == msg.role
                                    } else {
                                        false
                                    };
                                    let is_last = i == message_count.saturating_sub(1);
                                    let show_thinking = should_show_thinking_for_message(
                                        &reasoning_mode(),
                                        streaming(),
                                        is_last,
                                    );
                                    rsx! {
                                        ChatMessageBubble {
                                            key: "{i}",
                                            message: msg.clone(),
                                            show_thinking: show_thinking,
                                            verbose_mode: verbose_mode(),
                                            is_last: is_last,
                                            is_streaming: streaming(),
                                            prev_same_role: prev_same,
                                            on_expand_tool: on_expand_tool,
                                        }
                                    }
                                }
                            }
                        }

                        if has_sidebar {
                            div { class: "chat-sidebar",
                                div { class: "chat-sidebar__header",
                                    span { class: "chat-sidebar__title", "Content" }
                                    if let Some(content) = sidebar_content() {
                                        CopyButton { text: content }
                                    }
                                    button {
                                        class: "chat-sidebar__close",
                                        onclick: move |_| sidebar_content.set(None),
                                        "Close"
                                    }
                                }
                                div { class: "chat-sidebar__body",
                                    div { class: "chat-sidebar__content",
                                        MarkdownRenderer { content: sidebar_content().unwrap_or_default() }
                                    }
                                }
                            }
                        }
                    }

                    ChatInput {
                        on_send: on_send,
                        on_abort: on_abort,
                        streaming: streaming(),
                        model_value: selected_model(),
                        on_model_change: move |model: String| {
                            selected_model.set(model.clone());
                            if current_session_id().is_none() {
                                pending_session_model.set(Some(model.clone()));
                                save_pending_session_model(Some(&model));
                            }
                        },
                        agent_value: selected_agent(),
                        on_agent_change: move |agent: String| {
                            selected_agent.set(agent.clone());
                            // Update model to match the selected agent's primary model
                            let agents_snapshot = agents_data.read().as_ref().cloned().unwrap_or_default();
                            if let Some(entry) = find_agent_entry(&agents_snapshot, &agent) {
                                if let Some(primary) = primary_model_for_agent(entry) {
                                    selected_model.set(primary);
                                }
                            }
                        },
                        agents: agents_list.clone(),
                    }
                }
            }
        }
        style { {SESSION_STYLES} }
    }
}

const SESSION_STYLES: &str = r#"
    .session-page {
        display: flex;
        flex-direction: column;
        flex: 1 1 0;
        min-height: 0;
        overflow: hidden;
        background: var(--bg-primary);
    }

    .session-shell {
        display: flex;
        flex: 1 1 0;
        min-height: 0;
        overflow: hidden;
    }

    .session-list-pane {
        width: 320px;
        min-width: 260px;
        max-width: 400px;
        min-height: 0;
        border-right: 1px solid color-mix(in srgb, var(--border) 40%, transparent);
        background: color-mix(in srgb, var(--bg-secondary) 40%, var(--bg-primary) 60%);
        display: flex;
        flex-direction: column;
        flex-shrink: 0;
        overflow: hidden;
    }

    .session-list-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 12px;
        padding: 16px 20px;
        border-bottom: 1px solid color-mix(in srgb, var(--border) 40%, transparent);
    }

    .session-list-title {
        font-size: 14px;
        font-weight: 600;
        letter-spacing: -0.01em;
        color: var(--text-primary);
        margin: 0;
    }

    .session-list-new-btn {
        padding: 7px 16px;
        border: none;
        border-radius: var(--radius);
        background: var(--accent);
        color: #fff;
        font-size: 13px;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
        box-shadow: 0 1px 4px rgba(0, 0, 0, 0.12);
        letter-spacing: 0.01em;
    }

    .session-list-new-btn:hover {
        background: color-mix(in srgb, var(--accent) 85%, #000 15%);
        color: #fff;
        transform: translateY(-0.5px);
        box-shadow: 0 3px 8px rgba(0, 0, 0, 0.18);
    }

    .session-list-scroll {
        flex: 1;
        overflow: auto;
        padding: 12px 12px;
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .session-list-empty {
        color: var(--text-muted);
        font-size: 13px;
        text-align: center;
        padding: 32px 16px;
        background: color-mix(in srgb, var(--bg-tertiary) 40%, transparent);
        border: 1px dashed var(--border);
        border-radius: var(--radius);
        margin: 8px;
    }

    .session-list-item-wrapper {
        position: relative;
        display: flex;
        align-items: stretch;
        width: 100%;
        border-radius: var(--radius);
        overflow: hidden;
        flex-shrink: 0;
    }

    .session-list-item {
        flex: 1;
        text-align: left;
        border: none;
        border-radius: var(--radius);
        background: transparent;
        color: var(--text-secondary);
        padding: 10px 14px;
        padding-right: 32px;
        cursor: pointer;
        display: flex;
        flex-direction: column;
        gap: 4px;
        transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
    }

    .session-list-item__delete {
        position: absolute;
        right: 8px;
        top: 50%;
        transform: translateY(-50%) scale(0.95);
        width: 24px;
        height: 24px;
        border: none;
        border-radius: 6px;
        background: var(--bg-tertiary);
        color: var(--text-muted);
        font-size: 18px;
        line-height: 1;
        cursor: pointer;
        display: flex;
        align-items: center;
        justify-content: center;
        opacity: 0;
        transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
        box-shadow: 0 1px 3px rgba(0,0,0,0.1);
    }

    .session-list-item-wrapper:hover .session-list-item__delete {
        opacity: 1;
        transform: translateY(-50%) scale(1);
    }

    .session-list-item__delete:hover {
        background: var(--danger);
        color: #fff;
    }

    .session-delete-confirm {
        position: absolute;
        right: 4px;
        top: 50%;
        transform: translateY(-50%);
        display: flex;
        align-items: center;
        gap: 4px;
        z-index: 2;
        background: var(--bg-tertiary);
        border-radius: 6px;
        padding: 2px 6px;
        box-shadow: 0 1px 4px rgba(0,0,0,0.15);
    }

    .session-delete-confirm__label {
        font-size: 11px;
        color: var(--text-secondary);
        white-space: nowrap;
    }

    .session-delete-confirm__yes,
    .session-delete-confirm__no {
        border: none;
        border-radius: 4px;
        font-size: 11px;
        font-weight: 600;
        padding: 2px 8px;
        cursor: pointer;
        line-height: 1.4;
    }

    .session-delete-confirm__yes {
        background: var(--danger);
        color: #fff;
    }

    .session-delete-confirm__yes:hover {
        filter: brightness(1.1);
    }

    .session-delete-confirm__no {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .session-delete-confirm__no:hover {
        background: var(--border);
    }

    .session-list-item-wrapper:hover .session-list-item {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .session-list-item--active {
        background: var(--bg-tertiary) !important;
        color: var(--text-primary) !important;
        box-shadow: inset 2px 0 0 var(--accent);
    }

    .session-list-item__title {
        font-size: 13px;
        font-weight: 500;
        line-height: 1.4;
        word-break: break-word;
    }

    .session-list-item__preview {
        font-size: 11px;
        color: var(--text-muted);
        line-height: 1.4;
        display: flex;
        align-items: center;
        gap: 6px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .session-list-item__model {
        font-size: 10px;
        font-weight: 500;
        color: var(--accent);
        background: color-mix(in srgb, var(--accent) 10%, transparent);
        padding: 1px 6px;
        border-radius: 4px;
        white-space: nowrap;
        flex-shrink: 0;
    }

    .session-list-item__meta {
        font-size: 11px;
        color: var(--text-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        opacity: 0.8;
    }

    .session-main {
        flex: 1 1 0;
        min-width: 0;
        min-height: 0;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        background: var(--bg-primary);
        position: relative;
    }

    .session-main:before {
        content: '';
        position: absolute;
        top: 0;
        left: 0;
        right: 0;
        height: 120px;
        background: linear-gradient(180deg, color-mix(in srgb, var(--bg-secondary) 80%, transparent) 0%, transparent 100%);
        pointer-events: none;
        z-index: 10;
        opacity: 0.5;
    }

    .session-main-header {
        position: sticky;
        top: 0;
        z-index: 20;
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 16px;
        flex-wrap: wrap;
        padding: 16px 24px;
        border-bottom: 1px solid color-mix(in srgb, var(--border) 30%, transparent);
        background: color-mix(in srgb, var(--bg-primary) 85%, transparent);
        backdrop-filter: blur(12px);
        -webkit-backdrop-filter: blur(12px);
        flex-shrink: 0;
    }

    .session-main-header__left {
        display: flex;
        align-items: center;
        gap: 12px;
        min-width: 0;
    }

    .session-main-header__right {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
        justify-content: flex-end;
    }

    .session-main-title {
        margin: 0;
        font-size: 15px;
        font-weight: 600;
        letter-spacing: -0.01em;
        color: var(--text-primary);
    }

    .session-main-subtitle {
        font-size: 13px;
        color: var(--text-secondary);
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: 12px;
        padding: 4px 12px;
        max-width: 260px;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        box-shadow: 0 1px 2px rgba(0,0,0,0.02);
    }

    .session-main-badge {
        font-size: 11px;
        font-weight: 600;
        color: var(--accent);
        background: color-mix(in srgb, var(--accent) 15%, transparent);
        border-radius: 12px;
        padding: 4px 10px;
    }

    .session-agent-select {
        padding: 6px 12px;
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        font-size: 13px;
        font-weight: 500;
        outline: none;
        min-width: 140px;
        cursor: pointer;
        box-shadow: 0 1px 2px rgba(0,0,0,0.02);
        transition: all 0.2s ease;
    }
    
    .session-agent-select:hover {
        border-color: color-mix(in srgb, var(--border) 70%, var(--accent) 30%);
    }

    .session-pill-btn {
        padding: 6px 14px;
        background: var(--bg-secondary);
        color: var(--text-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 13px;
        font-weight: 500;
        cursor: pointer;
        transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
        box-shadow: 0 1px 2px rgba(0,0,0,0.02);
    }

    .session-pill-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
        border-color: color-mix(in srgb, var(--border) 70%, var(--accent) 30%);
    }

    .session-pill-btn--active {
        background: color-mix(in srgb, var(--accent) 10%, var(--bg-secondary) 90%);
        border-color: color-mix(in srgb, var(--accent) 50%, transparent);
        color: var(--accent);
    }

    .chat-body {
        flex: 1 1 0;
        min-height: 0;
        display: flex;
        overflow: hidden;
        position: relative;
        z-index: 1;
    }

    .chat-body--split .chat-messages {
        border-right: 1px solid var(--border);
    }

    .chat-messages {
        flex: 1;
        overflow: auto;
        padding: 24px 0 32px 0;
        scroll-behavior: smooth;
    }
    
    .chat-messages > * {
        max-width: 800px;
        margin-left: auto;
        margin-right: auto;
    }

    .chat-empty {
        display: flex;
        align-items: center;
        justify-content: center;
        height: 100%;
        color: var(--text-muted);
        font-size: 15px;
        font-weight: 500;
        opacity: 0.7;
    }

    .chat-sidebar {
        width: min(36vw, 420px);
        min-width: 280px;
        max-width: 520px;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        background: var(--bg-secondary);
        box-shadow: -4px 0 24px rgba(0,0,0,0.05);
        z-index: 30;
    }

    .chat-sidebar__header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 12px;
        padding: 16px 20px;
        border-bottom: 1px solid var(--border);
        background: var(--bg-secondary);
    }

    .chat-sidebar__title {
        font-size: 13px;
        font-weight: 600;
        color: var(--text-secondary);
    }

    .chat-sidebar__close {
        border: none;
        border-radius: 6px;
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        padding: 6px 12px;
        font-size: 12px;
        font-weight: 500;
        cursor: pointer;
        transition: all 0.2s ease;
    }

    .chat-sidebar__close:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .chat-sidebar__body {
        flex: 1;
        overflow: auto;
        padding: 16px 20px;
    }

    .chat-sidebar__content {
        font-size: 13px;
        color: var(--text-secondary);
        line-height: 1.6;
    }

    .stop-generating-bar {
        display: flex;
        justify-content: center;
        padding: 8px 24px;
        flex-shrink: 0;
    }

    .stop-generating-btn {
        padding: 8px 24px;
        border: 1px solid var(--border);
        border-radius: 20px;
        background: var(--bg-secondary);
        color: var(--text-primary);
        font-size: 13px;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
        box-shadow: 0 2px 8px rgba(0, 0, 0, 0.06);
    }

    .stop-generating-btn:hover {
        background: var(--danger);
        color: #fff;
        border-color: var(--danger);
        box-shadow: 0 4px 12px rgba(0, 0, 0, 0.12);
        transform: translateY(-1px);
    }

    @media (max-width: 900px) {
        .session-shell {
            flex-direction: column;
        }

        .session-list-pane {
            width: 100%;
            max-width: none;
            min-width: 0;
            flex: 0 0 200px;
            border-right: none;
            border-bottom: 1px solid var(--border);
        }

        .chat-sidebar {
            width: 100%;
            max-width: none;
            min-width: 0;
            box-shadow: none;
            border-top: 1px solid var(--border);
        }
    }

    @media (max-width: 640px) {
        .session-main-header {
            padding: 12px 16px;
        }

        .session-main-header__left,
        .session-main-header__right {
            width: 100%;
            justify-content: space-between;
        }

        .session-agent-select {
            min-width: 120px;
            flex: 1;
        }

        .chat-messages {
            padding: 16px 0;
        }
    }
"#;
