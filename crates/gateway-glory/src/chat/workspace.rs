//! The session workspace follows the pre-migration Dioxus split-pane layout.
use super::*;
use crate::i18n::Locale;

fn words(locale: Locale, en: &str, zh: &str) -> String {
    if locale == Locale::ZhCn {
        zh.into()
    } else {
        en.into()
    }
}

pub(super) fn label(ctx: &AppContext, en: &'static str, zh: &'static str) -> Bond<String> {
    let locale = ctx.locale;
    ctx.scope.bond(move || words(locale.get(), en, zh))
}

fn icon(source: &'static str) -> impl Widget {
    let uri = format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(source)
    );
    image().source(uri).alt("").class("session-icon")
}

fn tool(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    source: &'static str,
    action: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        sapid::button()
            .unstyled()
            .class("session-pill-btn")
            .name(label(ctx, en, zh))
            .fill((icon(source), text().content(label(ctx, en, zh))))
            .on_press(action),
    )
}

pub(super) fn render(
    ctx: &AppContext,
    state: &Conversation,
    sessions: &Query,
    agents: &Query,
    models: &Query,
) -> Box<dyn Widget> {
    let scope = &ctx.scope;
    let search = scope.cage(String::new());
    let agent_filter = scope.cage(String::new());
    let list_open = scope.cage(false);
    let inspector = scope.cage(false);
    let source = sessions.value;
    let selected = state.session;
    let locale = ctx.locale;
    let entries = scope.bond(move || {
        let query = search.get().to_lowercase();
        let filter = agent_filter.get();
        let mut entries = serde_json::from_value::<SessionsResponse>(source.get())
            .map(|value| value.entries)
            .unwrap_or_default();
        entries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        entries
            .into_iter()
            .filter(|entry| {
                (filter.is_empty() || entry.agent_id.as_deref() == Some(filter.as_str()))
                    && (query.is_empty()
                        || entry.display_label().to_lowercase().contains(&query)
                        || entry.display_id().to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>()
    });
    let header_title = scope.bond(move || {
        let id = selected.get();
        serde_json::from_value::<SessionsResponse>(source.get())
            .ok()
            .and_then(|data| {
                data.entries
                    .into_iter()
                    .find(|entry| entry.display_id() == id)
            })
            .map(|entry| entry.display_label())
            .unwrap_or_else(|| words(locale.get(), "Draft session", "新会话"))
    });
    let message_source = state.messages;
    let message_items = scope.bond(move || {
        message_source
            .get()
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>()
    });
    let count = scope.bond(move || {
        let count = message_source.get().len();
        if count == 0 {
            String::new()
        } else {
            words(
                locale.get(),
                &format!("{count} messages"),
                &format!("{count} 条消息"),
            )
        }
    });
    let new_ctx = ctx.clone();
    let new_state = state.clone();
    let new_session = move || {
        if new_state.busy.get() {
            return;
        }
        set(new_state.session, String::new());
        set(new_state.messages, Vec::new());
        set(new_state.prompt, String::new());
        set(new_state.details, Value::Null);
        new_ctx.navigate("/sessions");
        set(list_open, false);
    };
    let list_ctx = ctx.clone();
    let list_state = state.clone();
    let list_query = sessions.clone();
    let sidebar = col()
        .class(scope.bond(move || {
            if list_open.get() {
                "session-list-pane session-list-pane--open".into()
            } else {
                "session-list-pane".into()
            }
        }))
        .fill((
            row().class("session-list-head").fill((
                text()
                    .kind(TextKind::Heading(2))
                    .class("session-list-title")
                    .content(label(ctx, "Sessions", "会话")),
                sapid::button()
                    .unstyled()
                    .class("session-list-new-btn")
                    .name(label(ctx, "New session", "新建会话"))
                    .fill((
                        icon(crate::utils::icons::ICON_PLUS),
                        text().content(label(ctx, "New session", "新建会话")),
                    ))
                    .on_press(new_session),
            )),
            row().class("session-list-filter").fill((
                text()
                    .class("session-list-filter__label")
                    .content(label(ctx, "Agent", "智能体")),
                catalog_picker(ctx, agents.value, agent_filter, PickerKind::Filter, None),
                {
                    let refresh = sessions.clone();
                    sapid::button()
                        .unstyled()
                        .class("session-list-refresh")
                        .name(label(ctx, "Refresh sessions", "刷新会话"))
                        .fill(icon(crate::utils::icons::ICON_REFRESH_CW))
                        .on_press(move || refresh.refresh())
                },
            )),
            row().class("session-list-search").fill((
                icon(crate::utils::icons::ICON_SEARCH),
                sapid::text_field()
                    .unstyled()
                    .class("session-search-input")
                    .name(label(ctx, "Filter sessions", "搜索会话"))
                    .placeholder(label(ctx, "Filter sessions…", "搜索会话…"))
                    .value(search)
                    .on_value_change(move |value| set(search, value)),
            )),
            error_text(ctx, sessions.error),
            col().class("session-list-scroll").fill((
                {
                    let empty = scope.bond(move || entries.get().is_empty());
                    let busy = sessions.busy;
                    when(empty, move || {
                        text().class("session-list-empty").content(scope_text(
                            locale,
                            busy,
                            "No saved sessions yet",
                            "暂无保存的会话",
                        ))
                    })
                },
                each(
                    entries,
                    |entry| entry.display_id(),
                    move |entry, _| {
                        let id = entry.get_untracked().display_id();
                        let ctx = list_ctx.clone();
                        let state = list_state.clone();
                        let query = list_query.clone();
                        let active = state.session;
                        let selected_id = id.clone();
                        let click_id = id.clone();
                        let delete_id = id.clone();
                        let click_ctx = ctx.clone();
                        let delete_ctx = ctx.clone();
                        let locale = ctx.locale;
                        row().class("session-list-item-wrapper").fill((
                            sapid::button()
                                .unstyled()
                                .class(ctx.scope.bond(move || {
                                    if active.get() == selected_id {
                                        "session-list-item session-list-item--active".into()
                                    } else {
                                        "session-list-item".into()
                                    }
                                }))
                                .name(entry.map(|entry| entry.display_label()))
                                .fill((
                                    text()
                                        .class("session-list-item__title")
                                        .content(entry.map(|entry| entry.display_label())),
                                    group().fill((
                                        text().class("session-list-item__model").content(
                                            entry.map(|entry| {
                                                entry.model.clone().unwrap_or_default()
                                            }),
                                        ),
                                        text().content(ctx.scope.bond(move || {
                                            let count = entry.get().display_count();
                                            if count == 0 {
                                                String::new()
                                            } else {
                                                words(
                                                    locale.get(),
                                                    &format!("{count} messages"),
                                                    &format!("{count} 条消息"),
                                                )
                                            }
                                        })),
                                    )),
                                    text().class("session-list-item__meta").content(
                                        ctx.scope.bond(move || {
                                            entry
                                                .get()
                                                .last_activity
                                                .as_deref()
                                                .and_then(|value| {
                                                    chrono::DateTime::parse_from_rfc3339(value).ok()
                                                })
                                                .map(|time| {
                                                    crate::utils::time::format_relative_timestamp(
                                                        time.timestamp(),
                                                        locale.get(),
                                                    )
                                                })
                                                .unwrap_or_default()
                                        }),
                                    ),
                                ))
                                .on_press(move || {
                                    click_ctx.navigate(&format!(
                                        "/sessions?session_id={}",
                                        encode(&click_id)
                                    ));
                                    set(list_open, false);
                                }),
                            sapid::button()
                                .unstyled()
                                .class("session-list-item__delete")
                                .name(label(&ctx, "Delete session", "删除会话"))
                                .fill(icon(crate::utils::icons::ICON_X))
                                .on_press(move || {
                                    if confirm(&words(
                                        locale.get_untracked(),
                                        "Delete this session and its history?",
                                        "删除此会话及其历史记录？",
                                    )) {
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
        ));
    let show = state.show_thinking;
    let assistant_agents = agents.value;
    let assistant_id = state.agent;
    let messages = col().class("chat-messages").fill((
        {
            let busy = state.history_busy;
            let messages = state.messages;
            when(scope.bond(move || messages.get().is_empty()), move || {
                col().class("chat-empty").fill((
                    icon(crate::utils::icons::ICON_MESSAGE_SQUARE),
                    text().class("chat-empty-title").content(scope_text(
                        locale,
                        busy,
                        "Start a new session",
                        "开始新的会话",
                    )),
                    text().class("chat-empty-description").content(scope_text(
                        locale,
                        busy,
                        "Send your first message to begin",
                        "发送第一条消息，开始对话",
                    )),
                ))
            })
        },
        each(
            message_items,
            |value| value.0,
            move |item, _| {
                widget(move |scope| {
                    let message = item.map(|(_, message)| message.clone());
                    let content = scope.bond(move || vec![message.get().content]);
                    let role = scope.bond(move || {
                        if message.get().role == "user" {
                            words(locale.get(), "You", "你")
                        } else {
                            common::array(&assistant_agents.get(), &["agents"])
                                .into_iter()
                                .find(|entry| common::item_id(entry) == assistant_id.get())
                                .map(|entry| common::string(&entry, &["name"]))
                                .unwrap_or_else(|| words(locale.get(), "Assistant", "助手"))
                        }
                    });
                    col()
                        .class(scope.bond(move || format!("chat-message {}", message.get().role)))
                        .fill((
                            text().class("message-author").content(role),
                            text()
                                .class("message-thinking")
                                .content(scope.bond(move || {
                                    if show.get() {
                                        message.get().thinking.unwrap_or_default()
                                    } else {
                                        String::new()
                                    }
                                })),
                            each(
                                content,
                                |value| value.clone(),
                                move |content, _| markdown(&content.get_untracked()),
                            ),
                            each(
                                scope.bond(move || {
                                    message
                                        .get()
                                        .attachments
                                        .into_iter()
                                        .filter(|attachment| {
                                            attachment.mime_type.starts_with("image/")
                                        })
                                        .enumerate()
                                        .collect::<Vec<_>>()
                                }),
                                |entry| entry.0,
                                move |entry, _| {
                                    let attachment = entry.get_untracked().1;
                                    let source = format!(
                                        "data:{};base64,{}",
                                        attachment.mime_type, attachment.data_base64
                                    );
                                    image()
                                        .source(source)
                                        .alt(attachment.filename)
                                        .class("chat-message-attachment")
                                },
                            ),
                            text().class("terminal-events").content(scope.bond(move || {
                                message
                                    .get()
                                    .terminal_events
                                    .iter()
                                    .filter_map(|event| {
                                        event.text.as_deref().or(event.message.as_deref())
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })),
                        ))
                        .build(scope)
                })
            },
        ),
    ));
    let export_state = state.clone();
    let clear_state = state.clone();
    let reset_ctx = ctx.clone();
    let reset_state = state.clone();
    let header = row().class("session-main-header").fill((
        row().class("session-main-header__left").fill((
            Box::new(
                sapid::button()
                    .unstyled()
                    .class("session-mobile-list-toggle")
                    .name(label(ctx, "Show sessions", "显示会话列表"))
                    .fill(icon(crate::utils::icons::ICON_MENU))
                    .on_press(move || set(list_open, !list_open.get())),
            ) as Box<dyn Widget>,
            Box::new(
                text()
                    .kind(TextKind::Heading(1))
                    .class("session-main-title page-title")
                    .content(label(ctx, "Session", "会话")),
            ),
            Box::new(text().class("session-main-subtitle").content(header_title)),
            Box::new(text().class("session-main-badge").content(count)),
        )),
        row().class("session-main-header__right").fill((
            tool(
                ctx,
                "Export",
                "导出",
                crate::utils::icons::ICON_FILE_TEXT,
                move || {
                    let mut output = String::new();
                    for message in export_state.messages.get() {
                        output.push_str(&format!("## {}\n\n{}\n\n", message.role, message.content));
                    }
                    crate::utils::download::trigger_download(
                        "savfox-session.md",
                        &output,
                        "text/markdown",
                    );
                },
            ),
            tool(
                ctx,
                "New",
                "新建",
                crate::utils::icons::ICON_PLUS,
                move || {
                    if !reset_state.busy.get() {
                        set(reset_state.session, String::new());
                        set(reset_state.messages, Vec::new());
                        set(reset_state.prompt, String::new());
                        reset_ctx.navigate("/sessions");
                    }
                },
            ),
            tool(
                ctx,
                "Clear",
                "清空",
                crate::utils::icons::ICON_TRASH,
                move || {
                    if !clear_state.busy.get() {
                        set(clear_state.messages, Vec::new());
                    }
                },
            ),
            tool(
                ctx,
                "Options",
                "选项",
                crate::utils::icons::ICON_SETTINGS,
                move || set(inspector, !inspector.get()),
            ),
        )),
    ));
    let options_ctx = ctx.clone();
    let options_state = state.clone();
    let options = when(inspector, move || {
        let terminal = options_state.clone();
        let delivery = options_state.clone();
        let ctx = options_ctx.clone();
        col().class("session-options-panel").fill((
            row().class("session-display-options").fill((
                localized_choice(
                    &ctx,
                    "Reasoning",
                    "推理显示",
                    options_state.reasoning,
                    &[
                        ("off", "Off", "关闭"),
                        ("on", "On", "开启"),
                        ("stream", "Stream", "流式"),
                    ],
                ),
                localized_choice(
                    &ctx,
                    "Verbose",
                    "详细程度",
                    options_state.verbose,
                    &[
                        ("off", "Off", "关闭"),
                        ("on", "On", "开启"),
                        ("full", "Full", "完整"),
                    ],
                ),
                Box::new(
                    sapid::checkbox()
                        .unstyled()
                        .label(label(&ctx, "Show reasoning", "显示思考过程"))
                        .checked(options_state.show_thinking)
                        .on_checked_change(move |value| set(show, value)),
                ) as Box<dyn Widget>,
            )),
            terminal_controls(&ctx, &terminal),
            delivery_controls(&ctx, &delivery),
            json_text(&ctx, options_state.details),
        ))
    });
    let composer_ctx = ctx.clone();
    let composer_state = state.clone();
    let composer_agents = agents.clone();
    let composer_sessions = sessions.clone();
    let composer_locales = scope.bond(move || vec![locale.get().code().to_owned()]);
    let prompt = each(
        composer_locales,
        |value| value.clone(),
        move |_, _| {
            composer(
                &composer_ctx,
                &composer_state,
                &composer_agents,
                &composer_sessions,
            )
        },
    );
    let agent_catalog = agents.value;
    let active_agent = state.agent;
    let agent_name = scope.bond(move || {
        let id = active_agent.get();
        common::array(&agent_catalog.get(), &["agents"])
            .into_iter()
            .find(|entry| common::item_id(entry) == id)
            .map(|entry| common::string(&entry, &["name", "id"]))
            .unwrap_or(id)
    });
    let composer_footer = col().class("chat-input-area").fill(
        col().class("chat-input").fill((
            row().class("chat-input-model-badge").fill((
                text().class("chat-input-model-badge__label").content(label(
                    ctx,
                    "AGENT",
                    "智能体",
                )),
                text()
                    .class("chat-input-model-badge__name")
                    .content(agent_name),
                text().class("chat-input-model-badge__sep").content("/"),
                text()
                    .class("chat-input-model-badge__label")
                    .content(label(ctx, "MODEL", "模型")),
                text()
                    .class("chat-input-model-badge__name")
                    .content(state.model),
            )),
            prompt,
            row().class("chat-input-actions").fill((
                row().class("chat-input-actions-left").fill((
                    catalog_picker(
                        ctx,
                        agents.value,
                        state.agent,
                        PickerKind::Agent,
                        Some(state.session),
                    ),
                    catalog_picker(ctx, models.value, state.model, PickerKind::Model, None),
                    localized_choice(
                        ctx,
                        "Reasoning effort",
                        "推理力度",
                        state.thinking,
                        &[
                            ("off", "Off", "关闭"),
                            ("minimal", "Minimal", "最少"),
                            ("low", "Low", "低"),
                            ("medium", "Medium", "中"),
                            ("high", "High", "高"),
                            ("xhigh", "Extra high", "极高"),
                        ],
                    ),
                )),
                text().class("chat-input-hint").content(label(
                    ctx,
                    "Enter to send · Shift+Enter for newline",
                    "Enter 发送 · Shift+Enter 换行",
                )),
            )),
        )),
    );
    Box::new(
        col()
            .class("session-page")
            .fill(row().class("session-shell").fill((
                sidebar,
                col().class("session-main").fill((
                    header,
                    options,
                    error_text(ctx, state.error),
                    row().class("chat-body").fill(messages),
                    composer_footer,
                )),
            ))),
    )
}

fn scope_text(
    locale: Cage<Locale>,
    busy: Cage<bool>,
    en: &'static str,
    zh: &'static str,
) -> Bond<String> {
    locale.map(move |locale| {
        if busy.get() {
            words(*locale, "Loading conversation…", "正在加载会话…")
        } else {
            words(*locale, en, zh)
        }
    })
}

#[derive(Clone, Copy)]
enum PickerKind {
    Filter,
    Agent,
    Model,
}

fn catalog_picker(
    ctx: &AppContext,
    catalog: Cage<Value>,
    selected: Cage<String>,
    kind: PickerKind,
    session: Option<Cage<String>>,
) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let data = ctx
        .scope
        .bond(move || vec![(locale.get().code().to_owned(), catalog.get())]);
    let context = ctx.clone();
    Box::new(each(
        data,
        |value| format!("{}:{}", value.0, value.1),
        move |source, _| {
            let data = source.get_untracked().1;
            let locale = context.locale.get_untracked();
            let mut options = vec![(
                if matches!(kind, PickerKind::Filter) {
                    String::new()
                } else {
                    "default".into()
                },
                words(
                    locale,
                    if matches!(kind, PickerKind::Filter) {
                        "All agents"
                    } else {
                        "Default"
                    },
                    if matches!(kind, PickerKind::Filter) {
                        "全部智能体"
                    } else {
                        "默认"
                    },
                ),
            )];
            let prefs = crate::utils::model_visibility::load_model_preferences();
            for entry in common::array(
                &data,
                if matches!(kind, PickerKind::Model) {
                    &["models"]
                } else {
                    &["agents"]
                },
            ) {
                if matches!(kind, PickerKind::Model)
                    && !crate::utils::model_visibility::is_model_visible(
                        &prefs,
                        &crate::pages::models::visibility_key(&entry),
                    )
                {
                    continue;
                }
                let id = common::item_id(&entry);
                if matches!(kind, PickerKind::Agent) && id == "default" {
                    options[0].1 = common::string(&entry, &["name", "id"]);
                }
                if id.is_empty() || options.iter().any(|(value, _)| *value == id) {
                    continue;
                }
                let name = common::string(&entry, &["name", "id"]);
                options.push((id, name));
            }
            let current = selected.get_untracked();
            if !options.iter().any(|(value, _)| *value == current) {
                options.push((current.clone(), current));
            }
            picker(
                &context,
                match kind {
                    PickerKind::Filter => "Agent filter",
                    PickerKind::Agent => "Agent",
                    PickerKind::Model => "Model",
                },
                match kind {
                    PickerKind::Filter => "智能体筛选",
                    PickerKind::Agent => "智能体",
                    PickerKind::Model => "模型",
                },
                selected,
                options,
                matches!(kind, PickerKind::Model),
                session,
            )
        },
    ))
}

fn localized_choice(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    selected: Cage<String>,
    options: &'static [(&'static str, &'static str, &'static str)],
) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let context = ctx.clone();
    Box::new(each(
        ctx.scope.bond(move || vec![locale.get().code().to_owned()]),
        |value| value.clone(),
        move |_, _| {
            picker(
                &context,
                en,
                zh,
                selected,
                options
                    .iter()
                    .map(|(value, en, zh)| {
                        (value.to_string(), words(locale.get_untracked(), en, zh))
                    })
                    .collect(),
                false,
                None,
            )
        },
    ))
}

fn picker(
    ctx: &AppContext,
    en: &'static str,
    zh: &'static str,
    selected: Cage<String>,
    options: Vec<(String, String)>,
    searchable: bool,
    session: Option<Cage<String>>,
) -> Box<dyn Widget> {
    let key = format!("chat-{}", en.to_ascii_lowercase().replace(' ', "-"));
    let values = Rc::new(
        options
            .iter()
            .map(|(value, _)| value.clone())
            .collect::<Vec<_>>(),
    );
    let bound = values.clone();
    let selected_key = selected.map(move |value| {
        bound
            .iter()
            .position(|candidate| candidate == value)
            .map(|index| format!("option-{index}"))
    });
    let native_options = options
        .into_iter()
        .enumerate()
        .map(|(index, (value, label))| {
            sapid::select_option(format!("option-{index}"), label).value(value)
        });
    let callback = move |key: Option<String>| {
        if let Some(value) = key
            .and_then(|key| {
                key.strip_prefix("option-")
                    .and_then(|index| index.parse::<usize>().ok())
            })
            .and_then(|index| values.get(index))
        {
            set(selected, value.clone());
        }
    };
    if searchable {
        Box::new(
            sapid::combobox(key)
                .unstyled()
                .class("chat-model-select")
                .label(label(ctx, en, zh))
                .placeholder(label(ctx, "Choose model", "选择模型"))
                .value(selected_key)
                .options(native_options)
                .empty_message(words(
                    ctx.locale.get_untracked(),
                    "No matching models",
                    "没有匹配的模型",
                ))
                .on_value_change(callback),
        )
    } else {
        let mut control = sapid::native_select(key)
            .unstyled()
            .class("chat-agent-select")
            .label(label(ctx, en, zh))
            .value(selected_key)
            .options(native_options)
            .on_value_change(callback);
        if let Some(session) = session {
            control = control.enabled(session.map(|session| session.is_empty()));
        }
        Box::new(control)
    }
}
