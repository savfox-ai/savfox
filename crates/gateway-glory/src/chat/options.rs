//! Session tools keep their own results, prerequisites and request lifecycle.
use super::*;
use crate::i18n::Locale;

fn words(locale: Locale, en: &str, zh: &str) -> String {
    if locale == Locale::ZhCn {
        zh.into()
    } else {
        en.into()
    }
}

fn note(ctx: &AppContext, en: &'static str, zh: &'static str) -> Box<dyn Widget> {
    Box::new(
        text()
            .class("session-option-note")
            .content(workspace::label(ctx, en, zh)),
    )
}

pub(super) fn reasoning_visible(mode: &str, responding: bool, is_latest: bool) -> bool {
    mode != "off" && (mode == "stream" || !responding || !is_latest)
}

fn section(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    class: &str,
    children: Vec<Box<dyn Widget>>,
) -> Box<dyn Widget> {
    Box::new(
        col().class(format!("session-option-card {class}")).fill((
            text()
                .kind(TextKind::Heading(3))
                .class("session-option-title")
                .content(workspace::label(ctx, en, zh)),
            col().class("session-option-body").fill(children),
        )),
    )
}

fn button(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    enabled: Bond<bool>,
    press: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        sapid::button()
            .class("session-option-button")
            .label(workspace::label(ctx, en, zh))
            .enabled(enabled)
            .on_press(press),
    )
}

fn input(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    value: Cage<String>,
    enabled: Bond<bool>,
) -> Box<dyn Widget> {
    Box::new(
        col().class("session-option-field").fill((
            text()
                .class("session-option-label")
                .content(workspace::label(ctx, en, zh)),
            sapid::textarea()
                .class("session-option-input")
                .name(workspace::label(ctx, en, zh))
                .value(value)
                .enabled(enabled)
                .on_value_change(move |next| set(value, next)),
        )),
    )
}

pub(super) fn render(
    ctx: &AppContext,
    state: &Conversation,
    agents: Cage<Value>,
    sessions: Cage<Value>,
) -> Box<dyn Widget> {
    Box::new(col().class("session-options-panel").fill((
        col().class("session-display-options").fill((
            text().class("session-option-label")
                .content(workspace::label(ctx, "Reasoning display", "思考过程")),
            workspace::localized_choice(ctx, "Reasoning display", "思考过程", state.reasoning, &[
                ("off", "Hidden", "隐藏"),
                ("on", "After reply", "回复完成后显示"),
                ("stream", "Live", "实时显示"),
            ]),
            note(ctx, "Controls the display of reasoning provided by the model; reasoning effort is set in the composer.",
                "控制模型返回的思考过程如何显示；推理力度在消息输入框下方设置。"),
        )),
        col().class("session-options-grid").fill(vec![
            terminal(ctx, state, agents),
            context(ctx, state),
            delivery(ctx, state, sessions),
        ]),
    )))
}

#[derive(Clone, Copy)]
struct Terminal {
    metadata: Cage<Value>,
    output: Cage<String>,
    sequence: Cage<u64>,
    error: Cage<String>,
    busy: Cage<bool>,
    generation: Cage<u64>,
    revision: Cage<u64>,
}

fn terminal_unavailable(session: &str, agent: &str, agents: &Value, locale: Locale) -> String {
    if session.is_empty() {
        return words(
            locale,
            "Choose an existing session or send its first message to use terminal controls.",
            "选择已有会话，或发送第一条消息创建会话后，才能控制终端。",
        );
    }
    let entry = common::array(agents, &["agents"])
        .into_iter()
        .find(|entry| {
            common::item_id(entry) == agent || (agent == "default" && entry["is_default"] == true)
        });
    let Some(entry) = entry else {
        return words(
            locale,
            "Waiting for the session's agent configuration.",
            "正在读取会话的智能体配置。",
        );
    };
    if entry["kind"] != "terminal" {
        return words(
            locale,
            "This session uses a native agent and has no interactive terminal.",
            "当前会话使用原生智能体，没有可交互的终端。",
        );
    }
    if entry.pointer("/terminal/mode").and_then(Value::as_str) != Some("managed_pty")
        && entry
            .pointer("/terminal/interactive_command")
            .and_then(Value::as_str)
            .is_none_or(|command| command.trim().is_empty())
    {
        return words(
            locale,
            "This agent needs managed_pty mode or an explicit interactive command for terminal controls.",
            "此智能体需要配置 managed_pty 模式或交互式启动命令，才能使用终端控制。",
        );
    }
    String::new()
}

fn terminal_running(metadata: &Value) -> bool {
    metadata["completion"] == "running"
}

fn terminal_output(value: &Value, since: u64) -> (String, u64) {
    let entries = value
        .get("entries")
        .or_else(|| value.get("transcript"))
        .and_then(Value::as_array);
    let mut output = String::new();
    let mut sequence = since;
    for entry in entries.into_iter().flatten() {
        let next = entry["sequence"].as_u64().unwrap_or(0);
        if next > since {
            output.push_str(entry["text"].as_str().unwrap_or(""));
            sequence = sequence.max(next);
        }
    }
    // This is a text transcript, rather than a terminal emulator.
    if let Ok(ansi) = regex_lite::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]") {
        output = ansi.replace_all(&output, "").into_owned();
    }
    (output, sequence)
}

fn accept_terminal(terminal: Terminal, value: &Value) {
    if let Some(metadata) = value.get("metadata") {
        set(terminal.metadata, metadata.clone());
    }
    let (output, sequence) = terminal_output(value, terminal.sequence.get_untracked());
    if !output.is_empty() {
        let mut transcript = terminal.output.get_untracked();
        transcript.push_str(&output);
        let tail: String = transcript.chars().rev().take(65_536).collect();
        set(terminal.output, tail.chars().rev().collect());
    }
    set(terminal.sequence, sequence);
}

fn terminal(ctx: &AppContext, state: &Conversation, agents: Cage<Value>) -> Box<dyn Widget> {
    let terminal = Terminal {
        metadata: ctx.scope.cage(Value::Null),
        output: ctx.scope.cage(String::new()),
        sequence: ctx.scope.cage(0),
        error: ctx.scope.cage(String::new()),
        busy: ctx.scope.cage(false),
        generation: ctx.scope.cage(0),
        revision: ctx.scope.cage(0),
    };
    let session = state.session;
    let agent = state.agent;
    let locale = ctx.locale;
    let connected = ctx.connected;
    let unavailable = ctx.scope.bond(move || {
        terminal_unavailable(&session.get(), &agent.get(), &agents.get(), locale.get())
    });
    let allowed = ctx
        .scope
        .bond(move || unavailable.get().is_empty() && connected.get());
    let context = ctx.clone();
    ctx.scope.effect(move || {
        let generation = terminal.generation.get_untracked().wrapping_add(1);
        set(terminal.generation, generation);
        if !allowed.get() {
            return;
        }
        set(terminal.busy, true);
        let context = context.clone();
        let selected_session = session.get_untracked();
        let selected_agent = agent.get_untracked();
        let task = context.clone();
        context.spawn(async move {
            let params = json!({"agent":selected_agent,"session_id":selected_session});
            let response = task
                .rpc
                .call::<Value>("agent.terminal.pty.list", None)
                .await;
            if terminal.generation.get() != generation {
                return;
            }
            match response {
                Ok(value) => {
                    let metadata = common::array(&value, &["sessions"])
                        .into_iter()
                        .find(|metadata| {
                            metadata.pointer("/key/agent_id").and_then(Value::as_str)
                                == Some(selected_agent.as_str())
                                && metadata.pointer("/key/session_id").and_then(Value::as_str)
                                    == Some(selected_session.as_str())
                        })
                        .unwrap_or(Value::Null);
                    set(terminal.metadata, metadata);
                }
                Err(message) => set(terminal.error, message),
            }
            set(terminal.busy, false);
            loop {
                if terminal.generation.get() != generation
                    || session.get() != selected_session
                    || agent.get() != selected_agent
                {
                    break;
                }
                if connected.get()
                    && !terminal.busy.get()
                    && terminal_running(&terminal.metadata.get())
                {
                    let revision = terminal.revision.get_untracked();
                    let mut read = params.clone();
                    read["since_sequence"] = json!(terminal.sequence.get_untracked());
                    let response = task
                        .rpc
                        .call::<Value>("agent.terminal.pty.read", Some(read))
                        .await;
                    if terminal.generation.get() != generation
                        || terminal.revision.get() != revision
                    {
                        continue;
                    }
                    match response {
                        Ok(value) => accept_terminal(terminal, &value),
                        Err(message) => {
                            set(terminal.error, message);
                            set(terminal.metadata, Value::Null);
                        }
                    }
                }
                crate::utils::sleep_ms(1000).await;
            }
        });
    });
    let mut actions = Vec::new();
    for (en, zh, method, kind) in [
        (
            "Start terminal",
            "启动终端",
            "agent.terminal.pty.start",
            "start",
        ),
        (
            "Refresh output",
            "刷新输出",
            "agent.terminal.pty.read",
            "read",
        ),
        ("Send line", "发送一行", "agent.terminal.pty.write", "line"),
        ("Interrupt", "中断", "agent.terminal.pty.write", "interrupt"),
        (
            "Close terminal",
            "关闭终端",
            "agent.terminal.pty.close",
            "close",
        ),
    ] {
        let input = state.terminal_input;
        let ready = ctx.scope.bond(move || {
            allowed.get()
                && !terminal.busy.get()
                && if kind == "start" {
                    !terminal_running(&terminal.metadata.get())
                } else {
                    terminal_running(&terminal.metadata.get())
                        && (kind != "line" || !input.get().trim().is_empty())
                }
        });
        let context = ctx.clone();
        let button = button(ctx, en, zh, ready, move || {
            if !ready.get() {
                return;
            }
            if kind == "close"
                && !confirm(&words(
                    locale.get(),
                    "Close this session's terminal process?",
                    "关闭当前会话的终端进程？",
                ))
            {
                return;
            }
            let params = json!({"agent":agent.get(),"session_id":session.get(),"kind":kind,"text":if kind=="line"{input.get()}else{String::new()},"since_sequence":terminal.sequence.get()});
            set(
                terminal.revision,
                terminal.revision.get_untracked().wrapping_add(1),
            );
            set(terminal.busy, true);
            set(terminal.error, String::new());
            if kind == "start" {
                set(terminal.output, String::new());
                set(terminal.sequence, 0);
            }
            let task = context.clone();
            context.spawn(async move {
                match task.rpc.call::<Value>(method, Some(params)).await {
                    Ok(value) => {
                        accept_terminal(terminal, &value);
                        if kind == "line" {
                            set(input, String::new());
                        }
                    }
                    Err(message) => set(terminal.error, message),
                }
                set(terminal.busy, false);
            });
        });
        actions.push(button);
    }
    let metadata = terminal.metadata;
    section(
        ctx,
        "Interactive terminal",
        "交互式终端",
        "session-option-card--terminal",
        vec![
            note(
                ctx,
                "Input goes directly to the agent's configured interactive process; it is not a general shell command box.",
                "输入会直接发送给智能体配置的交互式进程，这里不是通用的系统命令执行框。",
            ),
            Box::new(
                text()
                    .class("session-option-note session-terminal-prerequisite")
                    .content(unavailable),
            ),
            Box::new(
                col()
                    .class(ctx.scope.bond(move || {
                        if unavailable.get().is_empty() {
                            "session-terminal-body"
                        } else {
                            "session-terminal-body is-unavailable"
                        }
                        .to_owned()
                    }))
                    .fill(vec![Box::new(
                text()
                    .class("session-terminal-status")
                    .content(ctx.scope.bond(move || {
                        if terminal.busy.get() {
                            words(locale.get(), "Working…", "正在执行…")
                        } else if terminal_running(&metadata.get()) {
                            words(locale.get(), "Running", "运行中")
                        } else {
                            words(locale.get(), "Not running", "未启动")
                        }
                    })),
            ) as Box<dyn Widget>,
            Box::new(
                text()
                    .class("session-terminal-command")
                    .content(metadata.map(|v| common::string(v, &["command"]))),
            ),
            input(
                ctx,
                "Terminal input",
                "终端输入",
                state.terminal_input,
                ctx.scope.bond(move || {
                    allowed.get() && !terminal.busy.get() && terminal_running(&metadata.get())
                }),
            ),
            Box::new(row().class("session-option-actions").fill(actions)),
            error_text(ctx, terminal.error),
            Box::new(
                text()
                    .class("session-terminal-output")
                    .content(ctx.scope.bond(move || {
                        let output = terminal.output.get();
                        if output.is_empty() {
                            words(
                                locale.get(),
                                "Terminal output will appear here after the process starts.",
                                "进程启动后，终端输出会显示在这里。",
                            )
                        } else {
                            output
                        }
                    })),
            ),
            ]),
            ),
        ],
    )
}

fn context_result(value: &Value, locale: Locale) -> String {
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.is_empty() {
            return words(
                locale,
                "No buffered context messages.",
                "没有缓存的上下文消息。",
            );
        }
        return messages
            .iter()
            .map(|message| {
                format!(
                    "{}: {}",
                    common::string(message, &["sender_name", "sender_id", "sender_kind"]),
                    common::string(message, &["text"])
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    let count = value["recent_sent_count"].as_u64().unwrap_or(0);
    let pending = value
        .pointer("/pending/message_preview")
        .and_then(Value::as_str);
    let status = match pending {
        Some(preview) => format!(
            "{}\n{preview}",
            words(locale, "Pending fallback reply:", "等待发送的空闲回复：")
        ),
        None => words(locale, "No pending idle reply.", "没有等待发送的空闲回复。"),
    };
    format!(
        "{status}\n\n{}: {count}",
        words(locale, "Replies in the last hour", "最近一小时回复数")
    )
}

fn context(ctx: &AppContext, state: &Conversation) -> Box<dyn Widget> {
    let busy = ctx.scope.cage(false);
    let result = ctx.scope.cage(String::new());
    let error = ctx.scope.cage(String::new());
    let session = state.session;
    let connected = ctx.connected;
    let locale = ctx.locale;
    let ready = ctx
        .scope
        .bond(move || !session.get().is_empty() && connected.get() && !busy.get());
    let mut actions = Vec::new();
    for (en, zh, method) in [
        (
            "View ambient context",
            "查看环境上下文",
            "sessions.ambient.get",
        ),
        ("View idle reply", "查看空闲回复", "sessions.idle_reply.get"),
    ] {
        let context = ctx.clone();
        actions.push(button(ctx, en, zh, ready, move || {
            if !ready.get() {
                return;
            }
            let params = json!({"session_id":session.get()});
            set(busy, true);
            set(error, String::new());
            let task = context.clone();
            context.spawn(async move {
                match task.rpc.call::<Value>(method, Some(params)).await {
                    Ok(value) => set(result, context_result(&value, locale.get())),
                    Err(message) => set(error, message),
                }
                set(busy, false);
            });
        }));
    }
    section(
        ctx,
        "Session context",
        "会话上下文",
        "",
        vec![
            note(
                ctx,
                "Inspect buffered context and pending idle replies. An existing session is required.",
                "查看缓存上下文和待发送的空闲回复，需要先选择或创建会话。",
            ),
            Box::new(row().class("session-option-actions").fill(actions)),
            error_text(ctx, error),
            Box::new(text().class("session-option-output").content(result)),
        ],
    )
}

fn delivery_params(session: &str, summary: &str) -> Value {
    json!({"session_id":session,"kind":"progress","title":"Progress update","summary":summary.trim(),"verification":[],"next_actions":[]})
}

fn delivery(ctx: &AppContext, state: &Conversation, sessions: Cage<Value>) -> Box<dyn Widget> {
    let summary = state.checkpoint_summary;
    let preview = ctx.scope.cage(Value::Null);
    let busy = ctx.scope.cage(false);
    let error = ctx.scope.cage(String::new());
    let result = ctx.scope.cage(String::new());
    let session = state.session;
    let connected = ctx.connected;
    let locale = ctx.locale;
    let bound = ctx.scope.bond(move || {
        common::array(&sessions.get(), &["entries"])
            .iter()
            .any(|entry| {
                common::string(entry, &["session_id", "id"]) == session.get()
                    && entry
                        .pointer("/arkret_delivery/mode")
                        .and_then(Value::as_str)
                        == Some("task_delivery")
            })
    });
    let available = ctx
        .scope
        .bond(move || bound.get() && connected.get() && !busy.get());
    let can_preview = ctx
        .scope
        .bond(move || available.get() && !summary.get().trim().is_empty());
    let can_publish = ctx.scope.bond(move || {
        can_preview.get()
            && preview.get()["params"] == delivery_params(&session.get(), &summary.get())
    });
    let mut actions = Vec::new();
    for (en, zh, method, publishing) in [
        (
            "Preview checkpoint",
            "预览检查点",
            "sessions.arkret.delivery.preview",
            false,
        ),
        (
            "Publish checkpoint",
            "发布检查点",
            "sessions.arkret.delivery.publish",
            true,
        ),
    ] {
        let ready = if publishing { can_publish } else { can_preview };
        let context = ctx.clone();
        actions.push(button(ctx, en, zh, ready, move || {
            if !ready.get() {
                return;
            }
            if publishing
                && !confirm(&words(
                    locale.get(),
                    "Publish this reviewed checkpoint to Arkret?",
                    "将已审核的检查点发布到 Arkret？",
                ))
            {
                return;
            }
            let params = delivery_params(&session.get(), &summary.get());
            set(busy, true);
            set(error, String::new());
            set(result, String::new());
            let task = context.clone();
            context.spawn(async move {
                match task.rpc.call::<Value>(method, Some(params.clone())).await {
                    Ok(value) => {
                        if publishing {
                            set(preview, Value::Null);
                            set(
                                result,
                                words(locale.get(), "Checkpoint published.", "检查点已发布。"),
                            );
                        } else {
                            set(preview, json!({"params":params,"result":value}));
                        }
                    }
                    Err(message) => set(error, message),
                }
                set(busy, false);
            });
        }));
    }
    section(ctx, "Arkret checkpoint", "Arkret 检查点", "", vec![
        Box::new(text().class("session-option-note").content(ctx.scope.bond(move || {
            if bound.get() { words(locale.get(), "Only this public summary is published. Preview and review it before publishing; edits require a new preview.", "仅发布这里填写的公开摘要。发布前先预览并审核；修改摘要后需要重新预览。") }
            else { words(locale.get(), "Publishing is available only for sessions bound to an Arkret task delivery.", "仅绑定 Arkret 任务投递的会话支持发布检查点。") }
        }))),
        Box::new(col().class(ctx.scope.bond(move || {
            if bound.get() { "session-delivery-body" } else { "session-delivery-body is-unavailable" }.to_owned()
        })).fill(vec![input(ctx, "Public checkpoint summary", "公开检查点摘要", summary, available),
        Box::new(row().class("session-option-actions").fill(actions)),
        error_text(ctx, error),
        Box::new(text().class("session-option-output session-delivery-preview").content(preview.map(|value| {
            value.pointer("/result/rendered").and_then(Value::as_str).unwrap_or("").to_owned()
        }))),
        ])),
        Box::new(text().class("session-option-note").content(ctx.scope.bond(move || {
            if !preview.get().is_null() && !can_publish.get() && !busy.get() { words(locale.get(), "Summary changed. Preview again before publishing.", "摘要已修改，请重新预览后再发布。") } else { result.get() }
        }))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_display_distinguishes_live_completed_and_hidden() {
        assert!(reasoning_visible("stream", true, true));
        assert!(!reasoning_visible("on", true, true));
        assert!(reasoning_visible("on", false, true));
        assert!(reasoning_visible("on", true, false));
        assert!(!reasoning_visible("off", false, false));
    }

    #[test]
    fn terminal_transcript_uses_sequences_and_strips_terminal_colors() {
        let (text, sequence) = terminal_output(
            &json!({"entries":[
                {"sequence":1,"text":"previous"}, {"sequence":2,"text":"\u{1b}[32mready\u{1b}[0m\n"},
                {"sequence":3,"text":"next\n"}
            ]}),
            1,
        );
        assert_eq!(text, "ready\nnext\n");
        assert_eq!(sequence, 3);
    }

    #[test]
    fn native_and_protocol_agents_do_not_offer_shell_input() {
        let agents = json!({"agents":[
            {"id":"native","kind":"native"},
            {"id":"protocol","kind":"terminal","terminal":{"mode":"app_server"}},
            {"id":"pty","kind":"terminal","terminal":{"mode":"managed_pty"}}
        ]});
        assert!(!terminal_unavailable("", "pty", &agents, Locale::En).is_empty());
        assert!(!terminal_unavailable("session", "native", &agents, Locale::En).is_empty());
        assert!(!terminal_unavailable("session", "protocol", &agents, Locale::En).is_empty());
        assert!(terminal_unavailable("session", "pty", &agents, Locale::En).is_empty());
    }
}
