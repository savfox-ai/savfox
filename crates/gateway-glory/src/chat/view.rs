//! Session workspace. Browser transport remains independent from widget state.
use std::cell::RefCell;
use std::rc::Rc;
use std::result::Result;

use base64::Engine;
use glory::prelude::*;
use serde_json::{Value, json};

use super::helpers::{
    build_openai_chat_message_payload, parse_history_messages, parse_terminal_events,
};
use crate::api::client::stream_chat;
use crate::api::types::{AgentEntry, ChatAttachment, ChatMessage, SessionsResponse};
use crate::components::markdown_renderer::markdown;
use crate::pages::common::{self, Query};
use crate::ui::{AppContext, action, error_text, field, json_text, set};

#[derive(Clone)]
struct Conversation {
    session: Cage<String>,
    messages: Cage<Vec<ChatMessage>>,
    prompt: Cage<String>,
    agent: Cage<String>,
    model: Cage<String>,
    thinking: Cage<String>,
    reasoning: Cage<String>,
    verbose: Cage<String>,
    busy: Cage<bool>,
    error: Cage<String>,
    request: Cage<String>,
    abort: Rc<RefCell<Option<web_sys::AbortController>>>,
    show_thinking: Cage<bool>,
    terminal_input: Cage<String>,
    details: Cage<Value>,
}

pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let scope = &ctx.scope;
    let state = Conversation {
        session: scope.cage(session_from_path(path)),
        messages: scope.cage(Vec::new()),
        prompt: scope.cage(String::new()),
        agent: scope.cage(query_value(path, "agent").unwrap_or_else(|| "default".into())),
        model: scope.cage(
            query_value(path, "model")
                .unwrap_or_else(|| preference("savfox_default_model", "default")),
        ),
        thinking: scope.cage("medium".into()),
        reasoning: scope.cage("on".into()),
        verbose: scope.cage("on".into()),
        busy: scope.cage(false),
        error: scope.cage(String::new()),
        request: scope.cage(String::new()),
        abort: Rc::new(RefCell::new(None)),
        show_thinking: scope.cage(true),
        terminal_input: scope.cage(String::new()),
        details: scope.cage(Value::Null),
    };
    let sessions = common::query(ctx, "sessions.list", None);
    let agents = common::query(ctx, "agents.list", None);
    let models = common::query(ctx, "models.list", None);
    let selected = state.session;
    let agent = state.agent;
    let model = state.model;
    let catalog = sessions.value;
    ctx.scope.effect(move || {
        let selected = selected.get();
        if let Ok(sessions) = serde_json::from_value::<SessionsResponse>(catalog.get())
            && let Some(entry) = sessions
                .entries
                .into_iter()
                .find(|entry| entry.display_id() == selected)
        {
            if let Some(next) = entry.agent_id
                && agent.get_untracked() != next
            {
                set(agent, next);
            }
            if let Some(next) = entry.model
                && model.get_untracked() != next
            {
                set(model, next);
            }
        }
    });
    history_effect(ctx, &state);
    stream_notifications(ctx, &state);
    let abort = state.abort.clone();
    let _ = ctx.scope.on_cleanup(move || {
        if let Some(controller) = abort.borrow_mut().take() {
            controller.abort();
        }
    });

    let entries = sessions.value;
    let filtered = scope.cage(String::new());
    let session_items = scope.bond(move || {
        serde_json::from_value::<SessionsResponse>(entries.get())
            .map(|value| {
                let filter = filtered.get().to_lowercase();
                let mut entries = value.entries;
                entries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
                entries
                    .into_iter()
                    .filter(|entry| {
                        filter.is_empty()
                            || entry.display_label().to_lowercase().contains(&filter)
                            || entry.display_id().to_lowercase().contains(&filter)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let list_ctx = ctx.clone();
    let list_state = state.clone();
    let list_query = sessions.clone();
    let new_ctx = ctx.clone();
    let new_state = state.clone();
    let composer = composer(ctx, &state, &agents, &sessions);
    let indexed = state.messages;
    let message_items =
        scope.bond(move || indexed.get().into_iter().enumerate().collect::<Vec<_>>());
    let show = state.show_thinking;
    let metadata = state.details;
    let model_value = models.value;
    let agent_value = agents.value;
    let model_state = state.model;
    let agent_state = state.agent;
    let selected_session = state.session;
    let preferences = crate::utils::model_visibility::load_model_preferences();
    let content = Box::new(
        row().class("sessions-workspace").fill((
            col().class("sessions-sidebar").fill((
                action("New session", move || {
                    if new_state.busy.get() {
                        set(
                            new_state.error,
                            "Stop the current response before starting a new session".into(),
                        );
                        return;
                    }
                    set(new_state.session, String::new());
                    set(new_state.messages, Vec::new());
                    set(new_state.prompt, String::new());
                    set(new_state.details, Value::Null);
                    new_ctx.navigate("/sessions");
                }),
                field("Filter sessions", filtered, false),
                common::refresh(ctx, &sessions),
                common::status(ctx, &sessions),
                each(
                    session_items,
                    |entry| entry.display_id(),
                    move |entry, _| {
                        let id = entry.get_untracked().display_id();
                        let state = list_state.clone();
                        let ctx = list_ctx.clone();
                        let query = list_query.clone();
                        let delete_ctx = ctx.clone();
                        let delete_id = id.clone();
                        let session_id = state.session;
                        let label = entry.map(|value| {
                            format!("{} · {}", value.display_label(), value.display_count())
                        });
                        let target = id.clone();
                        let select_ctx = ctx.clone();
                        col()
                            .class(ctx.scope.bond(move || {
                                if session_id.get() == id {
                                    "session-row active".into()
                                } else {
                                    "session-row".into()
                                }
                            }))
                            .fill((
                                sapid::button()
                                    .class("session-choice")
                                    .label(label)
                                    .on_press(move || {
                                        select_ctx.navigate(&format!(
                                            "/sessions?session_id={}",
                                            encode(&target)
                                        ));
                                    }),
                                sapid::button().label("Delete").on_press(move || {
                                    if confirm("Delete this session and its history?") {
                                        common::mutate(
                                            &delete_ctx,
                                            &query,
                                            "sessions.delete",
                                            json!({"session_id":delete_id}),
                                        );
                                    }
                                }),
                            ))
                    },
                ),
            )),
            col().class("conversation").fill((
                row().class("conversation-options").fill((
                    Box::new(
                        col().class("form-field").fill((
                            text().class("field-label").content("Agent"),
                            sapid::text_field()
                                .name("Agent")
                                .value(state.agent)
                                .enabled(selected_session.map(|session| session.is_empty()))
                                .on_value_change(move |value| set(agent_state, value)),
                        )),
                    ) as Box<dyn Widget>,
                    field("Model", state.model, false),
                    common::choice(
                        "Thinking",
                        state.thinking,
                        &[
                            ("off", "Off"),
                            ("minimal", "Minimal"),
                            ("low", "Low"),
                            ("medium", "Medium"),
                            ("high", "High"),
                            ("xhigh", "Extra high"),
                        ],
                    ),
                    common::choice(
                        "Reasoning",
                        state.reasoning,
                        &[("off", "Off"), ("on", "On"), ("stream", "Stream")],
                    ),
                    common::choice(
                        "Verbose",
                        state.verbose,
                        &[("off", "Off"), ("on", "On"), ("full", "Full")],
                    ),
                    common::toggle("Show reasoning", state.show_thinking),
                )),
                crate::ui::details(
                    "available-chat-models",
                    "Available agents and models",
                    Box::new(col().fill((
                        each(
                            scope.bond(move || {
                                common::array(&agent_value.get(), &["agents", "entries"])
                            }),
                            common::item_id,
                            move |item, _| {
                                let data = item.get_untracked();
                                let id = common::item_id(&data);
                                sapid::button()
                                    .label(item.map(|value| common::string(value, &["name", "id"])))
                                    .enabled(selected_session.map(|session| session.is_empty()))
                                    .on_press(move || {
                                        let model = item.get_untracked()["native"]["model"]
                                            .as_str()
                                            .unwrap_or("default")
                                            .to_owned();
                                        set(agent_state, id.clone());
                                        set(model_state, model);
                                    })
                            },
                        ),
                        each(
                            scope.bond(move || {
                                common::array(&model_value.get(), &["models", "entries"])
                                    .into_iter()
                                    .filter(|model| {
                                        crate::utils::model_visibility::is_model_visible(
                                            &preferences,
                                            &crate::pages::models::visibility_key(model),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                            common::item_id,
                            move |item, _| {
                                let data = item.get_untracked();
                                let id = common::item_id(&data);
                                sapid::button()
                                    .label(item.map(|value| common::string(value, &["name", "id"])))
                                    .on_press(move || set(model_state, id.clone()))
                            },
                        ),
                    ))),
                ),
                error_text(ctx, state.error),
                col().class("chat-messages").fill(each(
                    message_items,
                    |value| value.0,
                    move |item, _| {
                        widget(move |scope| {
                            let message = item.map(|(_, message)| message.clone());
                            let source = scope.bond(move || vec![message.get().content]);
                            let thinking = scope.bond(move || {
                                if show.get() {
                                    message.get().thinking.unwrap_or_default()
                                } else {
                                    String::new()
                                }
                            });
                            let role = scope.bond(move || message.get().role);
                            col()
                                .class(
                                    scope.bond(move || {
                                        format!("chat-message {}", message.get().role)
                                    }),
                                )
                                .fill((
                                    text().class("message-author").content(role),
                                    text().class("message-thinking").content(thinking),
                                    each(
                                        source,
                                        |value| value.clone(),
                                        move |content, _| markdown(&content.get_untracked()),
                                    ),
                                    text().class("terminal-events").content(scope.bond(
                                        move || {
                                            message
                                                .get()
                                                .terminal_events
                                                .iter()
                                                .filter_map(|event| {
                                                    event
                                                        .text
                                                        .as_deref()
                                                        .or(event.message.as_deref())
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n")
                                        },
                                    )),
                                ))
                                .build(scope)?;
                            Ok(())
                        })
                    },
                )),
                composer,
                terminal_controls(ctx, &state),
                delivery_controls(ctx, &state),
                json_text(ctx, metadata),
            )),
        )),
    ) as Box<dyn Widget>;
    crate::ui::localized_page(
        ctx,
        "nav.sessions",
        "Continue a conversation or start a new session.",
        vec![content],
    )
}

fn history_effect(ctx: &AppContext, state: &Conversation) {
    let ctx_effect = ctx.clone();
    let state = state.clone();
    ctx.scope.effect(move || {
        let connected = ctx_effect.connected.get();
        let session = state.session.get();
        if !connected || session.is_empty() || state.busy.get_untracked() {
            return;
        }
        let ctx = ctx_effect.clone();
        let state = state.clone();
        ctx_effect.spawn(async move {
            match ctx
                .rpc
                .call::<Value>(
                    "chat.history",
                    Some(json!({"session_id":session,"limit":200})),
                )
                .await
            {
                Ok(value) => set(state.messages, parse_history_messages(&value)),
                Err(error) => set(state.error, error),
            }
        });
    });
}

fn stream_notifications(ctx: &AppContext, state: &Conversation) {
    let state = state.clone();
    ctx.on_notification("agent.stream", move |payload| {
        if !matches_request(&payload, &state) {
            return;
        }
        if let Some(delta) = payload["delta"].as_str() {
            let kind = payload["kind"].as_str().unwrap_or("text");
            let _ = state.messages.try_revise(|messages| {
                if let Some(message) = messages.last_mut() {
                    if kind == "reasoning" || kind == "thinking" {
                        message.thinking.get_or_insert_default().push_str(delta);
                    } else if kind == "text" {
                        message.content.push_str(delta);
                    }
                }
            });
            crate::ui::flush();
        }
    });
}

fn matches_request(payload: &Value, state: &Conversation) -> bool {
    let request = state.request.try_get().unwrap_or_default();
    !request.is_empty() && payload["request_id"].as_str() == Some(request.as_str())
}

fn composer(
    ctx: &AppContext,
    state: &Conversation,
    agents: &Query,
    sessions: &Query,
) -> Box<dyn Widget> {
    let send_ctx = ctx.clone();
    let send_state = state.clone();
    let send_agents = agents.value;
    let send_sessions = sessions.clone();
    let cancel_ctx = ctx.clone();
    let cancel_state = state.clone();
    let error = state.error;
    Box::new(
        sapid::prompt_input("savfox-chat")
            .class("chat-composer")
            .label("Message")
            .value(state.prompt)
            .busy(state.busy)
            .enabled(ctx.connected)
            .submit_mode(sapid::PromptSubmitMode::EnterToSend)
            .attachments(glory::render::FileConfig {
                multiple: true,
                max_files: 8,
                filters: vec![glory::render::FileFilter::Mime("image/*".into())],
                max_file_bytes: 10 * 1024 * 1024,
                max_total_bytes: 32 * 1024 * 1024,
                ..Default::default()
            })
            .on_value_change({
                let prompt = state.prompt;
                move |value| set(prompt, value)
            })
            .on_reject(move |reason| set(error, format!("Message could not be sent: {reason:?}")))
            .on_send(move |submission| {
                if send_state.busy.get() {
                    return;
                }
                set(send_state.busy, true);
                set(send_state.error, String::new());
                let ctx = send_ctx.clone();
                let state = send_state.clone();
                let agents = send_agents.get();
                let sessions = send_sessions.clone();
                send_ctx.spawn(async move {
                    let result = send(&ctx, &state, submission, agents).await;
                    if result.is_ok() {
                        crate::utils::notifications::notify_if_enabled(
                            "Savfox response ready",
                            "Your conversation has a new response.",
                        );
                    }
                    if let Err(error) = result {
                        set(state.error, error);
                    }
                    set(state.busy, false);
                    set(state.request, String::new());
                    state.abort.borrow_mut().take();
                    sessions.refresh();
                });
            })
            .on_cancel(move || {
                if let Some(controller) = cancel_state.abort.borrow().as_ref() {
                    controller.abort();
                }
                let ctx = cancel_ctx.clone();
                let session = cancel_state.session.get();
                let error = cancel_state.error;
                cancel_ctx.spawn(async move {
                    if !session.is_empty()
                        && let Err(message) = ctx
                            .rpc
                            .call::<Value>("chat.abort", Some(json!({"session_id":session})))
                            .await
                    {
                        set(error, message);
                    }
                });
                set(cancel_state.request, String::new());
            }),
    )
}

async fn send(
    ctx: &AppContext,
    state: &Conversation,
    submission: sapid::PromptSubmission,
    agents: Value,
) -> Result<(), String> {
    let picker = submission.file_controller.clone();
    let attachments = read_attachments(submission.files, submission.file_controller).await?;
    if let Some(picker) = picker {
        let _ = picker.clear();
        crate::ui::flush();
    }
    let prompt = submission.text.trim().to_owned();
    let session = state.session.get();
    let agent = state.agent.get();
    let model = state.model.get();
    let label = savfox_gateway_shared::derive_session_label(Some(&prompt), None, None)
        .unwrap_or_else(|| "New session".into());
    let mut patch = json!({"label":label,"model":model,"overrides":{"model":model,"thinking":state.thinking.get(),"reasoning":state.reasoning.get(),"verbose":state.verbose.get()}});
    if session.is_empty() {
        patch["agent"] = json!(agent);
    } else {
        patch["session_id"] = json!(session);
    }
    let patched = ctx.rpc.call::<Value>("sessions.patch", Some(patch)).await?;
    let session = patched["session_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(&session)
        .to_owned();
    if session.is_empty() {
        return Err("Gateway did not return a session ID".into());
    }
    // Keep the current page mounted while giving this draft a shareable URL.
    crate::utils::deep_link::replace_url(&format!("/sessions?session_id={}", encode(&session)));
    if state.session.get_untracked() != session {
        let _ = state.session.try_revise(|value| *value = session.clone());
    }
    set(state.prompt, String::new());
    let _ = state.messages.try_revise(|messages| {
        messages.push(ChatMessage {
            role: "user".into(),
            content: prompt.clone(),
            attachments: attachments.clone(),
            timestamp: None,
            thinking: None,
            terminal_events: Vec::new(),
        });
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            attachments: Vec::new(),
            timestamp: None,
            thinking: None,
            terminal_events: Vec::new(),
        });
    });
    crate::ui::flush();
    let entries = common::array(&agents, &["agents", "entries"]);
    let terminal = entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<AgentEntry>(entry).ok())
        .any(|entry| {
            (entry.id.as_deref() == Some(&agent) || entry.name == agent)
                && entry.kind.as_str() == "terminal"
        });
    if terminal {
        let request = uuid::Uuid::new_v4().to_string();
        set(state.request, request.clone());
        let result=ctx.rpc.call::<Value>("chat.send",Some(json!({"message":prompt,"agent":agent,"session_id":session,"request_id":request,"attachments":attachments}))).await?;
        let _ = state.messages.try_revise(|messages| {
            if let Some(message) = messages.last_mut() {
                if let Some(content) = result
                    .get("raw_response")
                    .or_else(|| result.get("response"))
                    .and_then(Value::as_str)
                {
                    message.content = content.into();
                }
                message.terminal_events = parse_terminal_events(
                    result
                        .get("terminal")
                        .and_then(|terminal| terminal.get("events")),
                );
            }
        });
        crate::ui::flush();
    } else {
        let controller = web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?;
        *state.abort.borrow_mut() = Some(controller.clone());
        let messages = state.messages;
        let reasoning_mode = state.reasoning.get();
        let mut reasoning = move |chunk: &str| {
            if reasoning_mode == "off" {
                return;
            }
            let _ = messages.try_revise(|items| {
                if let Some(message) = items.last_mut() {
                    message.thinking.get_or_insert_default().push_str(chunk);
                }
            });
            crate::ui::flush();
        };
        let result = stream_chat(
            build_openai_chat_message_payload(&prompt, &attachments),
            model,
            Some(session),
            move |chunk| {
                let _ = messages.try_revise(|items| {
                    if let Some(message) = items.last_mut() {
                        message.content.push_str(chunk);
                    }
                });
                crate::ui::flush();
            },
            Some(&mut reasoning),
            Some(controller.signal()),
        )
        .await;
        if let Err(error) = result
            && !controller.signal().aborted()
        {
            return Err(error);
        }
    }
    Ok(())
}

async fn read_attachments(
    files: Vec<glory::render::FileInfo>,
    controller: Option<sapid::FileController>,
) -> Result<Vec<ChatAttachment>, String> {
    let mut attachments = Vec::new();
    if files.is_empty() {
        return Ok(attachments);
    }
    let controller = controller.ok_or("File access is unavailable")?;
    for file in files {
        if !file.media_type.starts_with("image/") {
            return Err("Only image attachments are supported".into());
        }
        let mut bytes = Vec::new();
        let mut offset = 0;
        while offset < file.size {
            let length = (file.size - offset).min(65_536) as u32;
            let (send, receive) = futures::channel::oneshot::channel();
            controller
                .read(
                    glory::render::FileRead {
                        file: file.token,
                        offset,
                        length,
                    },
                    move |result| {
                        let _ = send.send(result);
                    },
                )
                .map_err(|error| error.to_string())?;
            crate::ui::flush();
            bytes.extend(
                receive
                    .await
                    .map_err(|_| "File read cancelled")?
                    .map_err(|error| error.to_string())?,
            );
            offset += u64::from(length);
        }
        attachments.push(ChatAttachment {
            filename: file.name,
            mime_type: file.media_type,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    Ok(attachments)
}

fn terminal_controls(ctx: &AppContext, state: &Conversation) -> Box<dyn Widget> {
    let mut controls = vec![field("Terminal input", state.terminal_input, true)];
    for (label, method, kind) in [
        ("Read output", "agent.terminal.pty.read", "read"),
        ("Send line", "agent.terminal.pty.write", "line"),
        ("Interrupt", "agent.terminal.pty.write", "interrupt"),
        ("Close terminal", "agent.terminal.pty.close", "close"),
    ] {
        let ctx = ctx.clone();
        let state = state.clone();
        controls.push(action(label,move ||{
            let session=state.session.get();if session.is_empty(){set(state.error,"Choose a session first".into());return;}
            let task_ctx=ctx.clone();let state=state.clone();
            ctx.spawn(async move {
                match task_ctx.rpc.call::<Value>(method,Some(json!({"agent":state.agent.get(),"session_id":session,"kind":kind,"text":state.terminal_input.get()}))).await {
                    Ok(value)=>{set(state.details,value);if kind=="line"{set(state.terminal_input,String::new());}}
                    Err(error)=>set(state.error,error),
                }
            });
        }));
    }
    crate::ui::details(
        "session-terminal",
        "Terminal controls",
        Box::new(col().class("terminal-controls").fill(controls)),
    )
}

fn delivery_controls(ctx: &AppContext, state: &Conversation) -> Box<dyn Widget> {
    let summary = ctx.scope.cage(String::new());
    let preview = ctx.scope.cage(Value::Null);
    let mut controls = vec![field("Public checkpoint summary", summary, true)];
    for (label, method) in [
        ("Ambient context", "sessions.ambient.get"),
        ("Idle fallback", "sessions.idle_reply.get"),
        (
            "Preview Arkret checkpoint",
            "sessions.arkret.delivery.preview",
        ),
        (
            "Publish Arkret checkpoint",
            "sessions.arkret.delivery.publish",
        ),
    ] {
        let ctx = ctx.clone();
        let state = state.clone();
        controls.push(action(label,move ||{
            let session=state.session.get();if session.is_empty(){set(state.error,"Choose a session first".into());return;}
            let ctx_task=ctx.clone();let state=state.clone();
            let params=json!({"session_id":session,"kind":"progress","title":"Progress update","summary":summary.get(),"verification":[],"next_actions":[]});
            if method.ends_with(".publish") {
                if preview.get()["params"]!=params {set(state.error,"Preview the current checkpoint summary before publishing".into());return;}
                if !confirm("Publish the reviewed checkpoint to Arkret?"){return;}
            }
            ctx.spawn(async move {
                match ctx_task.rpc.call::<Value>(method,Some(params.clone())).await {
                    Ok(value)=>{if method.ends_with(".preview"){set(preview,json!({"params":params,"result":value}));}set(state.details,value);}
                    Err(error)=>set(state.error,error),
                }
            });
        }));
    }
    crate::ui::details(
        "session-delivery",
        "Session context and Arkret delivery",
        Box::new(col().class("delivery-controls").fill(controls)),
    )
}

fn query_value(path: &str, key: &str) -> Option<String> {
    path.split_once('?')?.1.split('&').find_map(|entry| {
        let (name, value) = entry.split_once('=')?;
        (name == key).then(|| js_sys_decode(value))
    })
}
fn js_sys_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            )
        {
            decoded.push((high * 16 + low) as u8);
            index += 3;
            continue;
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
fn session_from_path(path: &str) -> String {
    query_value(path, "session_id")
        .or_else(|| query_value(path, "session"))
        .or_else(|| {
            path.strip_prefix("/sessions/")
                .map(|value| value.split('?').next().unwrap_or_default().into())
        })
        .unwrap_or_default()
}
fn encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            output.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}
fn confirm(message: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.confirm_with_message(message).ok())
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = message;
        false
    }
}

fn preference(key: &str, fallback: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(key).ok().flatten())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| fallback.into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        fallback.into()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_deep_links_preserve_encoded_query_values() {
        assert_eq!(
            super::session_from_path("/sessions?session_id=a%2Fb"),
            "a/b"
        );
        assert_eq!(super::session_from_path("/sessions?session=older"), "older");
        assert_eq!(super::session_from_path("/sessions/a"), "a");
        assert_eq!(super::session_from_path("/sessions?session=%你"), "%你");
    }
}
