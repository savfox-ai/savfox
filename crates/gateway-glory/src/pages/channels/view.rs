//! Channel configuration and runtime views built with semantic widgets.
use super::*;

type FormValues = Cage<HashMap<String, String>>;

#[derive(Clone)]
struct Dashboard {
    configs: Cage<Value>,
    status: Cage<Value>,
    error: Cage<String>,
    busy: Cage<bool>,
}

fn update(values: FormValues, change: impl FnOnce(&mut HashMap<String, String>)) {
    if let Ok(mut snapshot) = values.try_get() {
        change(&mut snapshot);
        ui::set(values, snapshot);
    }
}

fn form_text(
    scope: &Scope,
    values: FormValues,
    key: String,
    label: String,
    secret: bool,
    multiline: bool,
) -> Box<dyn Widget> {
    let read_key = key.clone();
    let value = scope.bond(move || values.get().get(&read_key).cloned().unwrap_or_default());
    let control: Box<dyn Widget> = if secret {
        Box::new(
            sapid::password_input()
                .name(label.clone())
                .value(value)
                .on_value_change(move |value| {
                    update(values, |values| {
                        values.insert(key.clone(), value);
                    })
                }),
        )
    } else {
        Box::new(
            sapid::text_field()
                .name(label.clone())
                .multiline(multiline)
                .value(value)
                .on_value_change(move |value| {
                    update(values, |values| {
                        values.insert(key.clone(), value);
                    })
                }),
        )
    };
    Box::new(
        col()
            .class("form-field")
            .fill((text().content(label), control)),
    )
}

fn form_select(
    scope: &Scope,
    values: FormValues,
    key: String,
    label: &str,
    options: Vec<(String, String)>,
) -> Box<dyn Widget> {
    let read_key = key.clone();
    let option_values = std::rc::Rc::new(
        options
            .iter()
            .map(|(value, _)| value.clone())
            .collect::<Vec<_>>(),
    );
    let read_options = option_values.clone();
    let selected = scope.bond(move || {
        let snapshot = values.get();
        let current = snapshot.get(&read_key).or_else(|| read_options.first());
        current
            .and_then(|current| read_options.iter().position(|value| value == current))
            .map(|index| format!("option-{index}"))
    });
    Box::new(
        crate::ui::select_control(crate::pages::common::select_key(&key))
            .label(label)
            .value(selected)
            .options(
                options
                    .into_iter()
                    .enumerate()
                    .map(|(index, (value, label))| {
                        sapid::select_option(format!("option-{index}"), label).value(value)
                    }),
            )
            .on_value_change(move |value| {
                let value = value
                    .as_deref()
                    .and_then(|key| key.strip_prefix("option-"))
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| option_values.get(index))
                    .cloned()
                    .unwrap_or_default();
                update(values, |values| {
                    values.insert(key.clone(), value);
                })
            }),
    )
}

fn reload(ctx: &AppContext, state: &Dashboard) {
    let ctx = ctx.clone();
    let state = state.clone();
    ctx.clone().spawn(async move {
        ui::set(state.busy, true);
        ui::set(state.error, String::new());
        ctx.rpc.wait_connected().await;
        let (configs, status) = futures::future::join(
            ctx.rpc.call::<Value>("channels.config.list", None),
            ctx.rpc
                .call::<Value>("channels.status", Some(json!({"probe":false}))),
        )
        .await;
        match configs {
            Ok(value) => ui::set(state.configs, value),
            Err(error) => ui::set(state.error, error),
        }
        match status {
            Ok(value) => ui::set(state.status, value),
            Err(error) => ui::set(state.error, error),
        }
        ui::set(state.busy, false);
    });
}

fn runtime_action(ctx: &AppContext, state: &Dashboard, method: &'static str, kind: &str, id: &str) {
    let ctx = ctx.clone();
    let state = state.clone();
    let mut params = json!({"platform":kind});
    if kind != id {
        params["id"] = json!(id);
    }
    ctx.clone().spawn(async move {
        ui::set(state.busy, true);
        let result = ctx.rpc.call::<Value>(method, Some(params)).await;
        ui::set(state.busy, false);
        match result {
            Ok(value) => {
                reload(&ctx, &state);
                if method == "channels.test" {
                    ui::set(
                        state.error,
                        value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Channel test completed")
                            .to_owned(),
                    );
                }
            }
            Err(error) => ui::set(state.error, error),
        }
    });
}

fn toggle_enabled(ctx: &AppContext, state: &Dashboard, entry: SavedChannelSummary) {
    let ctx = ctx.clone();
    let state = state.clone();
    ctx.clone().spawn(async move {
        let enabled=!entry.enabled;ui::set(state.busy,true);
        match ctx.rpc.call::<Value>("channels.config.save",Some(json!({"channel":entry.kind,"id":entry.id,"name":entry.name,"config":{"enabled":enabled}}))).await {
            Ok(_)=>runtime_action(&ctx,&state,if enabled {"channels.login"} else {"channels.logout"},&entry.kind,&entry.id),
            Err(message)=>ui::set(state.error,message),
        }
        ui::set(state.busy,false);
    });
}

fn channel_status(status: &Value, id: &str, kind: &str) -> Value {
    status
        .get("instances")
        .and_then(|values| values.get(id))
        .or_else(|| status.get("channels").and_then(|values| values.get(kind)))
        .cloned()
        .unwrap_or(Value::Null)
}

pub(crate) fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    if path == "/channels/add" {
        return editor(ctx, None);
    }
    if let Some(id) = path.strip_prefix("/channels/edit/") {
        return editor(ctx, Some(id.to_owned()));
    }
    if let Some(id) = path.strip_prefix("/channels/health/") {
        return status_page(ctx, id);
    }
    if let Some(platform) = path.strip_prefix("/channels/") {
        return status_page(ctx, platform);
    }
    dashboard(ctx)
}

fn channel_copy(ctx: &AppContext, en: &'static str, zh: &'static str) -> glory::Bond<String> {
    let locale = ctx.locale;
    ctx.scope.bond(move || {
        if locale.get() == crate::i18n::Locale::ZhCn {
            zh.into()
        } else {
            en.into()
        }
    })
}

fn channel_button(
    label: impl Into<glory::Vary<String>>,
    class: &str,
    mut action: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        glory::prelude::button()
            .label(label)
            .class(class.to_owned())
            .on(glory::prelude::activate, move |_| action()),
    )
}

fn dashboard(ctx: &AppContext) -> Box<dyn Widget> {
    let state = Dashboard {
        configs: ctx.scope.cage(json!([])),
        status: ctx.scope.cage(Value::Null),
        error: ctx.scope.cage(String::new()),
        busy: ctx.scope.cage(false),
    };
    let connection_ctx = ctx.clone();
    let connection_state = state.clone();
    ctx.scope.effect(move || {
        if connection_ctx.connected.get() {
            reload(&connection_ctx, &connection_state);
        }
    });
    let status = state.status;
    let configs = state.configs;
    let search = ctx.scope.cage(String::new());
    let cards = ctx.scope.bond(move || {
        let query = search.get().to_ascii_lowercase();
        let mut entries = saved_channel_summaries(Some(&configs.get()));
        let runtime = status.get();
        for platform in get_channel_types() {
            if entries.iter().any(|entry| entry.kind == platform.id) {
                continue;
            }
            let item = channel_status(&runtime, &platform.id, &platform.id);
            if ["configured", "running", "connected"]
                .into_iter()
                .any(|key| item[key].as_bool() == Some(true))
            {
                entries.push(SavedChannelSummary {
                    kind: platform.id.clone(),
                    id: platform.id.clone(),
                    name: platform.name.clone(),
                    enabled: true,
                });
            }
        }
        entries.sort_by_key(|entry| {
            get_channel_types()
                .iter()
                .find(|kind| kind.id == entry.kind)
                .map(|kind| kind.name.to_ascii_lowercase())
                .unwrap_or_else(|| entry.name.to_ascii_lowercase())
        });
        entries
            .into_iter()
            .filter(|entry| {
                format!("{} {} {}", entry.name, entry.id, entry.kind)
                    .to_ascii_lowercase()
                    .contains(&query)
            })
            .collect::<Vec<_>>()
    });
    let mut health = Vec::<Box<dyn Widget>>::new();
    for (index, variant, en, zh) in [
        (0, "connected", "Connected", "已连接"),
        (1, "running", "Running", "运行中"),
        (2, "disconnected", "Disconnected", "未连接"),
        (3, "errors", "Errors", "错误"),
    ] {
        let count = ctx.scope.bond(move || {
            let (a, b, c, d) = compute_health_counts(Some(&status.get()));
            [a, b, c, d][index].to_string()
        });
        let mut content: Vec<Box<dyn Widget>> = vec![
            Box::new(text().class("channels-health__value").content(count)),
            Box::new(
                text()
                    .class("channels-health__label")
                    .content(channel_copy(ctx, en, zh)),
            ),
        ];
        if index == 0 {
            content.push(Box::new(
                glory::prelude::view().class("channels-health__indicator"),
            ));
        }
        health.push(Box::new(
            col()
                .class(format!(
                    "channels-health__card channels-health__card--{variant}"
                ))
                .fill(content),
        ));
    }
    let auto = ctx.scope.cage(false);
    let show_search = ctx.scope.cage(false);
    let refresh_ctx = ctx.clone();
    let refresh_state = state.clone();
    let add_ctx = ctx.clone();
    let alive = std::rc::Rc::new(std::cell::Cell::new(true));
    let cleanup = alive.clone();
    let _ = ctx.scope.on_cleanup(move || cleanup.set(false));
    let auto_ctx = ctx.clone();
    let auto_state = state.clone();
    ctx.spawn(async move {
        while alive.get() {
            crate::utils::sleep_ms(30_000).await;
            if alive.get() && auto.try_get().unwrap_or(false) {
                reload(&auto_ctx, &auto_state);
            }
        }
    });
    let locale = ctx.locale;
    let active = ctx.scope.bond(move || {
        let (a, b, ..) = compute_health_counts(Some(&status.get()));
        format!(
            "{} {}",
            a + b,
            if locale.get() == crate::i18n::Locale::ZhCn {
                "活跃"
            } else {
                "active"
            }
        )
    });
    let configured = ctx.scope.bond(move || {
        format!(
            "{} {}",
            saved_channel_summaries(Some(&configs.get())).len(),
            if locale.get() == crate::i18n::Locale::ZhCn {
                "已配置"
            } else {
                "configured"
            }
        )
    });
    let card_ctx = ctx.clone();
    let card_state = state.clone();
    let rows = each(cards)
        .key(|entry| entry.id.clone())
        .children(move |each_row| {
            let entry = each_row.value;
            let context = card_ctx.clone();
            let state = card_state.clone();
            glory::prelude::widget(move |scope| {
                channel_card(&context.with_scope(scope), &state, entry).build(scope)
            })
        });
    let empty = ctx.scope.bond(move || cards.get().is_empty());
    let empty_label = channel_copy(
        ctx,
        "No channels configured. Add a channel to get started.",
        "尚未配置渠道，添加渠道以连接消息平台。",
    );
    let search_label = channel_copy(ctx, "Search channels", "搜索渠道");
    let search_placeholder = channel_copy(ctx, "Search channels…", "搜索渠道…");
    let show_platforms = ctx.scope.cage(false);
    let platform_links = get_channel_types()
        .iter()
        .map(|platform| {
            let navigation = ctx.clone();
            let target = format!("/channels/{}", platform.id);
            channel_button(platform.name.clone(), "channels-btn", move || {
                navigation.navigate(&target)
            })
        })
        .collect::<Vec<_>>();
    Box::new(
        col().class("channels-page").fill((
            col().class("channels-health").fill(health),
            row().class("channels-toolbar").fill((
                col().class("channels-toolbar__left").fill((
                    text()
                        .class("channels-toolbar__title page-title")
                        .kind(glory::prelude::TextKind::Heading(1))
                        .content(channel_copy(ctx, "Channels", "渠道")),
                    text()
                        .class("channels-toolbar__subtitle")
                        .content(channel_copy(
                            ctx,
                            "Configure messaging platform integrations",
                            "配置消息平台集成",
                        )),
                )),
                row().class("channels-toolbar__actions").fill((
                    channel_button(
                        channel_copy(ctx, "Search", "搜索"),
                        "channels-btn",
                        move || ui::set(show_search, !show_search.get()),
                    ),
                    text()
                        .class("channels-status-badge channels-status-badge--connected")
                        .content(active),
                    text()
                        .class("channels-status-badge channels-status-badge--configured")
                        .content(configured),
                    glory::prelude::button()
                        .label(channel_copy(ctx, "Auto-refresh", "自动刷新"))
                        .class(auto.map(|enabled| {
                            if *enabled {
                                "channels-btn channels-btn--auto-active".into()
                            } else {
                                "channels-btn".into()
                            }
                        }))
                        .on(glory::prelude::activate, move |_| {
                            ui::set(auto, !auto.get())
                        }),
                    channel_button(
                        channel_copy(ctx, "Refresh", "刷新"),
                        "channels-btn",
                        move || reload(&refresh_ctx, &refresh_state),
                    ),
                    channel_button(
                        channel_copy(ctx, "+ Add Channel", "添加渠道"),
                        "channels-btn channels-btn--primary",
                        move || add_ctx.navigate("/channels/add"),
                    ),
                )),
            )),
            ui::error_text(ctx, state.error),
            when(show_search, move || {
                col().class("channels-search").fill((
                    sapid::text_field()
                        .unstyled()
                        .name(search_label)
                        .value(search)
                        .on_value_change(move |value| ui::set(search, value)),
                    when(search.map(|value| value.is_empty()), move || {
                        text()
                            .class("channels-search__hint")
                            .content(search_placeholder)
                    }),
                ))
            }),
            col().class("channels-grid").fill(rows),
            when(empty, move || {
                text().class("channels-empty").content(empty_label)
            }),
            channel_button(
                channel_copy(ctx, "Platform runtime status", "平台运行状态"),
                "channels-catalog-toggle",
                move || ui::set(show_platforms, !show_platforms.get()),
            ),
            row()
                .class(show_platforms.map(|open| {
                    if *open {
                        "channels-platform-catalog".into()
                    } else {
                        "channels-platform-catalog is-hidden".into()
                    }
                }))
                .fill(platform_links),
        )),
    )
}

fn channel_card(
    ctx: &AppContext,
    state: &Dashboard,
    entry: Cage<SavedChannelSummary>,
) -> Box<dyn Widget> {
    let channel = entry.get_untracked();
    let id = channel.id.clone();
    let kind = channel.kind.clone();
    let platform = get_channel_types()
        .iter()
        .find(|platform| platform.id == kind);
    let name = platform
        .map(|platform| platform.name.clone())
        .unwrap_or_else(|| channel.name.clone());
    let description = platform
        .map(|platform| platform.description.clone())
        .unwrap_or_default();
    let status = state.status;
    let read_id = id.clone();
    let read_kind = kind.clone();
    let value = ctx
        .scope
        .bond(move || channel_status(&status.get(), &read_id, &read_kind));
    let connected = ctx
        .scope
        .bond(move || value.get()["connected"].as_bool().unwrap_or(false));
    let running = ctx
        .scope
        .bond(move || value.get()["running"].as_bool().unwrap_or(false));
    let attention = ctx.scope.bond(move || {
        let snapshot = value.get();
        snapshot
            .get("lastError")
            .or_else(|| snapshot.get("last_error"))
            .and_then(Value::as_str)
            .is_some_and(|error| !error.is_empty())
            || matches!(
                snapshot.get("recovery_phase").and_then(Value::as_str),
                Some("failed" | "retrying" | "migration_required" | "unsupported_runtime")
            )
            || snapshot.get("health_state").and_then(Value::as_str) == Some("degraded")
    });
    let locale = ctx.locale;
    let status_label = ctx.scope.bond(move || {
        let zh = locale.get() == crate::i18n::Locale::ZhCn;
        if !entry.get().enabled {
            if zh { "已禁用" } else { "Disabled" }
        } else if attention.get() {
            if zh {
                "需要注意"
            } else {
                "Needs attention"
            }
        } else if connected.get() {
            if zh { "已连接" } else { "Connected" }
        } else if running.get() {
            if zh { "运行中" } else { "Running" }
        } else {
            if zh { "未连接" } else { "Disconnected" }
        }
        .to_owned()
    });
    let menu = ctx.scope.cage(false);
    let edit_ctx = ctx.clone();
    let edit_id = id.clone();
    let health_ctx = ctx.clone();
    let health_id = id.clone();
    let start_ctx = ctx.clone();
    let start_state = state.clone();
    let start_id = id.clone();
    let start_kind = kind.clone();
    let stop_ctx = ctx.clone();
    let stop_state = state.clone();
    let stop_id = id.clone();
    let stop_kind = kind.clone();
    let test_ctx = ctx.clone();
    let test_state = state.clone();
    let test_id = id.clone();
    let test_kind = kind.clone();
    let enable_ctx = ctx.clone();
    let enable_state = state.clone();
    let mut metrics = Vec::<Box<dyn Widget>>::new();
    for (index, en, zh) in [
        (0, "Last message", "最近消息"),
        (1, "Last event", "最近事件"),
        (2, "Uptime", "运行时长"),
        (3, "Error rate", "错误率"),
        (4, "Sent", "已发送"),
        (5, "Failed", "失败"),
    ] {
        let metric = ctx.scope.bond(move || {
            let status = value.get();
            match index {
                0 => status
                    .get("lastMessageTime")
                    .or_else(|| status.get("lastMessageAt"))
                    .or_else(|| status.get("last_message_at"))
                    .and_then(value_to_field_text)
                    .unwrap_or_else(|| "-".into()),
                1 => status
                    .get("lastEventTime")
                    .or_else(|| status.get("lastEventAt"))
                    .or_else(|| status.get("last_event_at"))
                    .and_then(value_to_field_text)
                    .unwrap_or_else(|| "-".into()),
                2 => status
                    .get("uptimeMs")
                    .or_else(|| status.get("uptime_ms"))
                    .and_then(Value::as_u64)
                    .map(format_uptime_compact)
                    .unwrap_or_else(|| "-".into()),
                3 => format!(
                    "{:.1}%",
                    status
                        .get("errorRate")
                        .or_else(|| status.get("error_rate"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                        * 100.0
                ),
                4 => status
                    .get("messages_total")
                    .or_else(|| status.get("sent"))
                    .or_else(|| status.get("sent_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .to_string(),
                _ => status
                    .get("messages_failed")
                    .or_else(|| status.get("failed"))
                    .or_else(|| status.get("failed_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .to_string(),
            }
        });
        metrics.push(Box::new(
            row().class("channels-card__metric").fill((
                text()
                    .class("channels-card__metric-label")
                    .content(channel_copy(ctx, en, zh)),
                text().class("channels-card__metric-value").content(metric),
            )),
        ));
    }
    let error = ctx.scope.bond(move || {
        let status = value.get();
        status
            .get("lastError")
            .or_else(|| status.get("last_error"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let has_error = ctx.scope.bond(move || !error.get().is_empty());
    let badge_class = ctx.scope.bond(move || {
        format!(
            "channels-status-badge channels-status-badge--{}",
            if attention.get() {
                "errors"
            } else if connected.get() {
                "connected"
            } else if running.get() {
                "running"
            } else {
                "disconnected"
            }
        )
    });
    let border = ctx.scope.bond(move || {
        format!(
            "channels-card channel-card {}",
            if !entry.get().enabled {
                "channels-card--disabled"
            } else if attention.get() {
                "channels-card--error"
            } else if connected.get() {
                "channels-card--connected"
            } else if running.get() {
                "channels-card--running"
            } else {
                "channels-card--configured"
            }
        )
    });
    let configuration = format!("{} / {}", id, channel.name);
    Box::new(
        col().class(border).fill((
            row().class("channels-card__header").fill((
                row().class("channels-card__identity").fill((
                    text()
                        .class("channels-card__icon")
                        .content(name.chars().next().unwrap_or('C').to_string()),
                    col().class("channels-card__meta").fill((
                        text().class("channels-card__name").content(name),
                        text().class("channels-card__desc").content(description),
                        text()
                            .class("channels-card__config-id")
                            .content(configuration),
                    )),
                )),
                row().class("channels-card__header-actions").fill((
                    text().class(badge_class).content(status_label),
                    col().class("channels-card__menu-wrap").fill((
                        glory::prelude::button()
                            .name(channel_copy(ctx, "Channel actions", "渠道操作"))
                            .class("channels-card__menu-trigger")
                            .fill(text().class("shell-icon shell-icon--settings").content(""))
                            .on(glory::prelude::activate, move |_| {
                                ui::set(menu, !menu.get())
                            }),
                        col()
                            .class(menu.map(|open| {
                                if *open {
                                    "channels-card__menu".into()
                                } else {
                                    "channels-card__menu is-hidden".into()
                                }
                            }))
                            .fill((
                                channel_button(
                                    channel_copy(ctx, "Configure", "配置"),
                                    "channels-card__menu-item",
                                    move || edit_ctx.navigate(&format!("/channels/edit/{edit_id}")),
                                ),
                                channel_button(
                                    channel_copy(ctx, "Health", "健康状态"),
                                    "channels-card__menu-item",
                                    move || {
                                        health_ctx
                                            .navigate(&format!("/channels/health/{health_id}"))
                                    },
                                ),
                                channel_button(
                                    channel_copy(ctx, "Start", "启动"),
                                    "channels-card__menu-item",
                                    move || {
                                        ui::set(menu, false);
                                        runtime_action(
                                            &start_ctx,
                                            &start_state,
                                            "channels.login",
                                            &start_kind,
                                            &start_id,
                                        )
                                    },
                                ),
                                channel_button(
                                    channel_copy(ctx, "Stop", "停止"),
                                    "channels-card__menu-item",
                                    move || {
                                        ui::set(menu, false);
                                        runtime_action(
                                            &stop_ctx,
                                            &stop_state,
                                            "channels.logout",
                                            &stop_kind,
                                            &stop_id,
                                        )
                                    },
                                ),
                            )),
                    )),
                )),
            )),
            col().class("channels-card__metrics").fill(metrics),
            when(has_error, move || {
                text().class("channels-card__error").content(error)
            }),
            row().class("channels-card__actions").fill((
                glory::prelude::button()
                    .name(entry.map(|entry| {
                        if entry.enabled {
                            "Disable".to_owned()
                        } else {
                            "Enable".to_owned()
                        }
                    }))
                    .label(ctx.scope.bond(move || {
                        if entry.get().enabled {
                            if locale.get() == crate::i18n::Locale::ZhCn {
                                "已启用"
                            } else {
                                "Enabled"
                            }
                        } else if locale.get() == crate::i18n::Locale::ZhCn {
                            "已禁用"
                        } else {
                            "Disabled"
                        }
                        .to_owned()
                    }))
                    .class(entry.map(|entry| {
                        if entry.enabled {
                            "channels-action-btn channels-action-btn--enabled".into()
                        } else {
                            "channels-action-btn".into()
                        }
                    }))
                    .on(glory::prelude::activate, move |_| {
                        toggle_enabled(&enable_ctx, &enable_state, entry.get())
                    }),
                channel_button(
                    channel_copy(ctx, "Test", "测试"),
                    "channels-action-btn channels-action-btn--test",
                    move || {
                        runtime_action(
                            &test_ctx,
                            &test_state,
                            "channels.test",
                            &test_kind,
                            &test_id,
                        )
                    },
                ),
            )),
        )),
    )
}

fn status_page(ctx: &AppContext, selector: &str) -> Box<dyn Widget> {
    let value = ctx.scope.cage(Value::Null);
    let error = ctx.scope.cage(String::new());
    let selector = selector.to_owned();
    let load_ctx = ctx.clone();
    let load_selector = selector.clone();
    let load = move || {
        let ctx = load_ctx.clone();
        let selector = load_selector.clone();
        ctx.clone().spawn(async move {
            ctx.rpc.wait_connected().await;
            let result = match status_params(&ctx.rpc, &selector).await {
                Ok(params) => ctx.rpc.call::<Value>("channels.status", Some(params)).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(payload) => {
                    ui::set(value, payload);
                    ui::set(error, String::new());
                }
                Err(message) => ui::set(error, message),
            }
        });
    };
    let load_effect = load.clone();
    let connected = ctx.connected;
    ctx.scope.effect(move || {
        if connected.get() {
            load_effect();
        }
    });
    let config_ctx = ctx.clone();
    let config_selector = selector.clone();
    let locale = ctx.locale;
    let summary = ctx.scope.bond(move || {
        let payload = value.get();
        let object = payload.get("status").unwrap_or(&payload);
        let mut lines = Vec::new();
        if let Some(fields) = object.as_object() {
            for (label, content) in fields {
                if !content.is_object() && !content.is_array() && !content.is_null() {
                    let display = if matches!(label.as_str(), "uptime_ms" | "uptimeMs") {
                        content
                            .as_u64()
                            .map(format_uptime_compact)
                            .unwrap_or_else(|| content.to_string())
                    } else if label == "pairing_state" {
                        arkret_pairing_state_label(content.as_str().unwrap_or_default())
                    } else if label == "runtime_phase" {
                        arkret_runtime_phase_label(content.as_str().unwrap_or_default())
                    } else if label == "runtime_reason" {
                        crate::i18n::t(locale.get(), arkret_runtime_error_key(content.as_str()))
                    } else {
                        content
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| content.to_string())
                    };
                    lines.push(format!("{}: {}", capitalize_first(label), display));
                }
            }
        }
        lines.join("\n")
    });
    let mut children: Vec<Box<dyn Widget>> = vec![
        Box::new(row().class("actions").fill((
            ui::action("Refresh", load),
            ui::action("Configure", move || {
                config_ctx.navigate(&format!("/channels/edit/{config_selector}"))
            }),
        ))),
        ui::error_text(ctx, error),
        Box::new(text().content(summary)),
        ui::json_text(ctx, value),
    ];
    for (label, method) in [
        ("Start", "channels.login"),
        ("Stop", "channels.logout"),
        ("Test connection", "channels.test"),
    ] {
        let ctx = ctx.clone();
        let selector = selector.clone();
        children.push(ui::action(label, move || {
            status_command(&ctx, value, error, &selector, method, Value::Null)
        }));
    }
    if selector == "whatsapp" || selector.starts_with("whatsapp-") {
        for (label, method, extra) in [
            ("Show QR", "web.login.start", Value::Null),
            ("Relink", "web.login.start", json!({"relink":true})),
            ("Check QR scan", "web.login.wait", Value::Null),
        ] {
            let ctx = ctx.clone();
            let selector = selector.clone();
            children.push(ui::action(label, move || {
                status_command(&ctx, value, error, &selector, method, extra.clone())
            }));
        }
        let qr = value.map(|value| {
            value
                .get("qr_data_url")
                .or_else(|| value.get("qrDataUrl"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        });
        children.push(Box::new(when(
            ctx.scope.bond(move || !qr.get().is_empty()),
            move || glory::ui::image().source(qr).alt("WhatsApp login QR code"),
        )));
    }
    if selector == "matrix" || selector.starts_with("matrix-") {
        children.push(matrix_tools(ctx, value, error));
    }
    if selector == "nostr" || selector.starts_with("nostr-") {
        children.push(nostr_tools(ctx, error));
    }
    ui::page(
        format!("{} channel health", capitalize_first(&selector)),
        "Live runtime and connectivity information",
        children,
    )
}

async fn status_params(rpc: &WsRpc, selector: &str) -> Result<Value, String> {
    let platform = get_channel_types().iter().find(|platform| {
        selector == platform.id || selector.starts_with(&format!("{}-", platform.id))
    });
    let kind = if let Some(platform) = platform {
        platform.id.clone()
    } else {
        let payload = rpc
            .call::<Value>("channels.config.get", Some(json!({"channel":selector})))
            .await?;
        payload["config"]["kind"]
            .as_str()
            .ok_or("Channel configuration was not found")?
            .to_owned()
    };
    let mut params = json!({"channel":kind,"probe":true});
    if selector != kind {
        params["id"] = json!(selector);
    }
    Ok(params)
}

fn status_command(
    ctx: &AppContext,
    value: Cage<Value>,
    error: Cage<String>,
    selector: &str,
    method: &'static str,
    extra: Value,
) {
    let ctx = ctx.clone();
    let selector = selector.to_owned();
    ctx.clone().spawn(async move {
        let mut params = match status_params(&ctx.rpc, &selector).await {
            Ok(params) => params,
            Err(message) => {
                ui::set(error, message);
                return;
            }
        };
        params["platform"] = params["channel"].clone();
        params.as_object_mut().unwrap().remove("probe");
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                params[key] = value.clone();
            }
        }
        match ctx.rpc.call::<Value>(method, Some(params)).await {
            Ok(result) => {
                ui::set(
                    error,
                    result
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Channel command completed")
                        .to_owned(),
                );
                let status_params = match status_params(&ctx.rpc, &selector).await {
                    Ok(params) => params,
                    Err(message) => {
                        ui::set(error, message);
                        return;
                    }
                };
                if let Ok(status) = ctx
                    .rpc
                    .call::<Value>("channels.status", Some(status_params))
                    .await
                {
                    ui::set(value, status);
                }
            }
            Err(message) => ui::set(error, message),
        }
    });
}

fn reload_matrix_invites(ctx: &AppContext, invites: Cage<Vec<Value>>, error: Cage<String>) {
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        ctx.rpc.wait_connected().await;
        match ctx.rpc.call::<Value>("channels.matrix.invites", None).await {
            Ok(payload) => ui::set(
                invites,
                payload["invites"].as_array().cloned().unwrap_or_default(),
            ),
            Err(message) => ui::set(error, message),
        }
    });
}

fn matrix_tools(ctx: &AppContext, status: Cage<Value>, error: Cage<String>) -> Box<dyn Widget> {
    let invites = ctx.scope.cage(Vec::<Value>::new());
    reload_matrix_invites(ctx, invites, error);
    let refresh_ctx = ctx.clone();
    let row_ctx = ctx.clone();
    let rows = each(invites)
        .key(|invite| format!("{}:{}", invite["config_id"], invite["room_id"]))
        .children(move |each_row| {
            let invite = each_row.value;
            let snapshot = invite.get_untracked();
            let room = snapshot["room_id"].as_str().unwrap_or_default().to_owned();
            let config = snapshot["config_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let label = snapshot["room_name"].as_str().unwrap_or(&room).to_owned();
            let mut buttons = Vec::new();
            for (label, method) in [
                ("Accept", "channels.matrix.invite.accept"),
                ("Reject", "channels.matrix.invite.reject"),
                ("Dismiss", "channels.matrix.invite.dismiss"),
            ] {
                let ctx = row_ctx.clone();
                let room = room.clone();
                let config = config.clone();
                buttons.push(ui::action(label, move || {
                    let ctx = ctx.clone();
                    let params = json!({"config_id":config,"room_id":room});
                    ctx.clone().spawn(async move {
                        match ctx.rpc.call::<Value>(method, Some(params)).await {
                            Ok(_) => reload_matrix_invites(&ctx, invites, error),
                            Err(message) => ui::set(error, message),
                        }
                    });
                }));
            }
            col().class("card").fill((
                text().content(format!("{label} · {room}")),
                row().class("actions").fill(buttons),
            ))
        });
    let yaml = status.map(|status| {
        status
            .get("registration")
            .map(registration_to_yaml)
            .unwrap_or_default()
    });
    let registration = when(ctx.scope.bond(move || !yaml.get().is_empty()), move || {
        col().fill((
            text().class("json-output").content(yaml),
            ui::action("Export appservice YAML", move || {
                #[cfg(target_arch = "wasm32")]
                crate::utils::download::trigger_download(
                    "appservice-registration.yaml",
                    &yaml.get(),
                    "application/x-yaml",
                );
            }),
        ))
    });
    Box::new(
        col().class("card").fill((
            text()
                .class("card-title")
                .content("Matrix appservice and room invites"),
            registration,
            ui::action("Refresh room invites", move || {
                reload_matrix_invites(&refresh_ctx, invites, error)
            }),
            rows,
        )),
    )
}

fn registration_to_yaml(reg: &Value) -> String {
    let mut yaml = String::new();
    for key in ["id", "url", "as_token", "hs_token", "sender_localpart"] {
        yaml.push_str(&format!(
            "{key}: {}\n",
            serde_json::to_string(reg.get(key).and_then(Value::as_str).unwrap_or_default())
                .unwrap_or_default()
        ));
    }
    yaml.push_str(&format!(
        "rate_limited: {}\nnamespaces:\n",
        reg["rate_limited"].as_bool().unwrap_or(false)
    ));
    for section in ["users", "aliases", "rooms"] {
        let entries = reg["namespaces"][section]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if entries.is_empty() {
            yaml.push_str(&format!("  {section}: []\n"));
            continue;
        }
        yaml.push_str(&format!("  {section}:\n"));
        for entry in entries {
            yaml.push_str(&format!(
                "    - exclusive: {}\n      regex: {}\n",
                entry["exclusive"].as_bool().unwrap_or(false),
                serde_json::to_string(entry["regex"].as_str().unwrap_or_default())
                    .unwrap_or_default()
            ));
        }
    }
    yaml
}

fn load_nostr(ctx: &AppContext, values: FormValues, error: Cage<String>) {
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        ctx.rpc.wait_connected().await;
        match ctx
            .rpc
            .call::<Value>("channels.nostr.profile.get", None)
            .await
        {
            Ok(payload) => {
                let profile = payload.get("profile").unwrap_or(&payload);
                update(values, |values| {
                    for key in [
                        "name",
                        "about",
                        "picture",
                        "nip05",
                        "public_key",
                        "private_key",
                    ] {
                        values.insert(
                            key.to_owned(),
                            profile[key].as_str().unwrap_or_default().to_owned(),
                        );
                    }
                });
            }
            Err(message) => ui::set(error, message),
        }
        match ctx
            .rpc
            .call::<Value>("channels.nostr.relays.get", None)
            .await
        {
            Ok(payload) => {
                let relays = payload
                    .get("relays")
                    .unwrap_or(&payload)
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                update(values, |values| {
                    values.insert(
                        "relays".to_owned(),
                        relays
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                });
            }
            Err(message) => ui::set(error, message),
        }
    });
}

fn nostr_tools(ctx: &AppContext, error: Cage<String>) -> Box<dyn Widget> {
    let values = ctx.scope.cage(HashMap::<String, String>::new());
    let exported = ctx.scope.cage(Value::Null);
    load_nostr(ctx, values, error);
    let mut children = Vec::new();
    for (key, label, multiline, secret) in [
        ("name", "Display name", false, false),
        ("about", "About", true, false),
        ("picture", "Picture URL", false, false),
        ("nip05", "NIP-05 address", false, false),
        ("public_key", "Public key (npub or hex)", false, false),
        ("private_key", "Private key (nsec or hex)", false, true),
        ("relays", "Relay URLs (one per line)", true, false),
    ] {
        children.push(form_text(
            &ctx.scope,
            values,
            key.to_owned(),
            label.to_owned(),
            secret,
            multiline,
        ));
    }
    for (label, method) in [
        ("Save profile", "channels.nostr.profile.set"),
        ("Import keypair", "channels.nostr.profile.import"),
        ("Save relays", "channels.nostr.relays.set"),
        ("Export profile", "channels.nostr.profile.export"),
    ] {
        let ctx = ctx.clone();
        children.push(ui::action(label,move || {
            let ctx=ctx.clone();let snapshot=values.get();
            let params=match method {
                "channels.nostr.profile.set"=>Some(json!({"profile":snapshot.iter().filter(|(key,_)|key.as_str()!="relays").map(|(key,value)|(key.clone(),json!(value))).collect::<serde_json::Map<_,_>>()})),
                "channels.nostr.profile.import"=>Some(json!({"private_key":snapshot.get("private_key"),"public_key":snapshot.get("public_key")})),
                "channels.nostr.relays.set"=>Some(json!({"relays":snapshot.get("relays").map(|relays|relays.lines().map(str::trim).filter(|line|!line.is_empty()).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default()})),
                _=>None,
            };
            ctx.clone().spawn(async move {
                match ctx.rpc.call::<Value>(method,params).await {
                    Ok(payload)=>{
                        if method=="channels.nostr.profile.export" {ui::set(exported,payload.get("profile").unwrap_or(&payload).clone());}
                        else {load_nostr(&ctx,values,error);}
                        ui::set(error,"Nostr operation completed".to_owned());
                    },Err(message)=>ui::set(error,message)
                }
            });
        }));
    }
    let refresh_ctx = ctx.clone();
    children.push(ui::action("Reload profile", move || {
        load_nostr(&refresh_ctx, values, error)
    }));
    children.push(ui::json_text(ctx, exported));
    Box::new(
        col()
            .class("card")
            .fill(
                text()
                    .class("card-title")
                    .content("Nostr profile and relays"),
            )
            .fill(children),
    )
}

fn editor(ctx: &AppContext, selector: Option<String>) -> Box<dyn Widget> {
    let initial_kind = selector
        .as_ref()
        .and_then(|id| {
            get_channel_types()
                .iter()
                .find(|platform| id == &platform.id || id.starts_with(&format!("{}-", platform.id)))
        })
        .map(|platform| platform.id.clone())
        .unwrap_or_else(|| "discord".to_owned());
    let kind = ctx.scope.cage(initial_kind.clone());
    let initial = get_channel_types()
        .iter()
        .find(|platform| platform.id == initial_kind)
        .unwrap();
    let values = ctx.scope.cage(new_channel_form_values(
        &initial_kind,
        &initial.config_fields,
        initial.name.clone(),
    ));
    let error = ctx.scope.cage(String::new());
    let busy = ctx.scope.cage(false);
    let loaded = ctx.scope.cage(selector.is_none());
    let pairing_generation = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let cancel_pairing = pairing_generation.clone();
    let _ = ctx
        .scope
        .on_cleanup(move || cancel_pairing.set(cancel_pairing.get().wrapping_add(1)));
    if let Some(selector) = selector.clone() {
        let ctx = ctx.clone();
        ctx.clone().spawn(async move {
            ctx.rpc.wait_connected().await;
            match ctx
                .rpc
                .call::<Value>("channels.config.get", Some(json!({"channel":selector})))
                .await
            {
                Ok(payload) => {
                    if let Some(saved) = payload.get("config").filter(|value| !value.is_null()) {
                        let resolved = saved
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or(&initial_kind)
                            .to_owned();
                        if let Some(platform) = get_channel_types()
                            .iter()
                            .find(|platform| platform.id == resolved)
                        {
                            let mut restored =
                                default_channel_values(&resolved, &platform.config_fields);
                            restored.extend(restore_channel_values(
                                &resolved,
                                &platform.config_fields,
                                saved,
                            ));
                            if let Some(id) = saved.get("id").and_then(Value::as_str) {
                                restored.insert(saved_channel_id_key(&resolved), id.to_owned());
                            }
                            ui::set(values, restored);
                            ui::set(kind, resolved);
                            ui::set(loaded, true);
                        } else {
                            ui::set(
                                error,
                                "Unknown channel type in saved configuration".to_owned(),
                            );
                        }
                    } else {
                        ui::set(error, "Channel configuration was not found".to_owned());
                    }
                }
                Err(message) => ui::set(error, message),
            }
        });
    } else {
        let ctx = ctx.clone();
        ctx.clone().spawn(async move {
            ctx.rpc.wait_connected().await;
            if let Ok(configs) = ctx.rpc.call::<Value>("channels.config.list", None).await {
                let current = kind.get();
                if let Some(platform) = get_channel_types()
                    .iter()
                    .find(|platform| platform.id == current)
                {
                    let snapshot = values.get();
                    if snapshot.get(&channel_name_key(&current)) == Some(&platform.name) {
                        let name = next_available_channel_name(
                            &current,
                            &platform.name,
                            &saved_channel_summaries(Some(&configs)),
                        );
                        update(values, |values| {
                            values.insert(channel_name_key(&current), name);
                        });
                    }
                }
            }
        });
    }
    let visible = ctx.scope.bond(move || {
        let platform = kind.get();
        let snapshot = values.get();
        get_channel_types()
            .iter()
            .find(|descriptor| descriptor.id == platform)
            .map(|descriptor| {
                descriptor
                    .config_fields
                    .iter()
                    .filter(|field| field_is_visible(&platform, field, &snapshot))
                    .cloned()
                    .map(|field| (platform.clone(), field))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let field_ctx = ctx.clone();
    let fields_view = each(visible)
        .key(move |(kind, field)| {
            let snapshot = values.get();
            let mode = match kind.as_str() {
                "arkret" => current_arkret_mode(kind, &snapshot),
                "matrix" => current_matrix_mode(kind, &snapshot),
                _ => String::new(),
            };
            format!("{kind}.{mode}.{}", field.key)
        })
        .children(move |each_row| {
            let entry = each_row.value;
            let (kind, descriptor) = entry.get_untracked();
            let snapshot = values.get();
            let label = field_display_label(&kind, &descriptor, &snapshot);
            let field_key = field_value_key(&kind, &descriptor.key);
            let read_key = field_key.clone();
            let control: Box<dyn Widget> = match descriptor.field_type.clone() {
                FieldType::Toggle => {
                    let checked = field_ctx.scope.bond(move || {
                        values
                            .get()
                            .get(&read_key)
                            .is_some_and(|value| value == "true")
                    });
                    Box::new(
                        sapid::checkbox()
                            .label(label)
                            .checked(checked)
                            .on_checked_change(move |checked| {
                                update(values, |values| {
                                    values.insert(field_key.clone(), checked.to_string());
                                })
                            }),
                    )
                }
                FieldType::Select(options) => form_select(
                    &field_ctx.scope,
                    values,
                    field_key,
                    &label,
                    options
                        .into_iter()
                        .map(|option| (option.clone(), option))
                        .collect(),
                ),
                FieldType::Password => {
                    form_text(&field_ctx.scope, values, field_key, label, true, false)
                }
                FieldType::Textarea => form_text(
                    &field_ctx.scope,
                    values,
                    field_key,
                    label,
                    descriptor.secret,
                    true,
                ),
                _ => form_text(
                    &field_ctx.scope,
                    values,
                    field_key,
                    label,
                    descriptor.secret,
                    false,
                ),
            };
            col().class("form-field").fill((
                control,
                text().class("field-help").content(field_display_help(
                    &kind,
                    &descriptor,
                    &snapshot,
                )),
                text()
                    .class("field-help")
                    .content(field_display_placeholder(&kind, &descriptor, &snapshot)),
            ))
        });
    let name = ctx.scope.bond(move || {
        values
            .get()
            .get(&channel_name_key(&kind.get()))
            .cloned()
            .unwrap_or_default()
    });
    let name_control = sapid::text_field()
        .name("Channel name")
        .value(name)
        .on_value_change(move |value| {
            update(values, |values| {
                values.insert(channel_name_key(&kind.get()), value);
            })
        });
    let kind_control = crate::ui::select_control("channel-type")
        .label("Messaging platform")
        .value(kind.map(|kind| Some(kind.clone())))
        .enabled(selector.is_none())
        .options(get_channel_types().iter().map(|platform| {
            sapid::select_option(platform.id.clone(), platform.name.clone())
                .value(platform.id.clone())
        }))
        .on_value_change(move |selected| {
            if let Some(selected) = selected
                && let Some(platform) = get_channel_types()
                    .iter()
                    .find(|platform| platform.id == selected)
            {
                ui::set(
                    values,
                    new_channel_form_values(
                        &selected,
                        &platform.config_fields,
                        platform.name.clone(),
                    ),
                );
                ui::set(kind, selected);
            }
        });
    let router_ctx = ctx.clone();
    let routers = each(kind.map(|kind| vec![kind.clone()]))
        .key(|kind| kind.clone())
        .children(move |each_row| {
            let kind = each_row.value;
            router_fields(&router_ctx, values, &kind.get_untracked())
        });
    let arkret_ctx = ctx.clone();
    let generation = pairing_generation.clone();
    let pairing = when(kind.map(|kind| kind == "arkret"), move || {
        arkret_pairing(&arkret_ctx, values, error, busy, generation.clone())
    });
    let save_ctx = ctx.clone();
    let can_export = ctx.scope.bond(move || {
        kind.get() == "matrix" && current_matrix_mode("matrix", &values.get()) == "appservice"
    });
    let export_registration = when(can_export, move || {
        ui::action("Export appservice registration YAML", move || {
            let registration = build_appservice_registration_from_values("matrix", &values.get());
            let yaml = registration_to_yaml(&registration);
            #[cfg(target_arch = "wasm32")]
            crate::utils::download::trigger_download(
                "appservice-registration.yaml",
                &yaml,
                "application/x-yaml",
            );
            #[cfg(not(target_arch = "wasm32"))]
            drop(yaml);
        })
    });
    let save = sapid::button()
        .label("Save channel")
        .enabled(ctx.scope.bond(move || loaded.get() && !busy.get()))
        .on_press(move || {
            let ctx = save_ctx.clone();
            let platform = kind.get();
            let snapshot = values.get();
            let Some(descriptor) = get_channel_types()
                .iter()
                .find(|descriptor| descriptor.id == platform)
            else {
                return;
            };
            if let Err(message) = validate_form(&platform, &descriptor.config_fields, &snapshot) {
                ui::set(error, message);
                return;
            }
            let fields = descriptor.config_fields.clone();
            ctx.clone().spawn(async move {
                ui::set(busy, true);
                match persist_channel_form(&ctx.rpc, &platform, &fields, &snapshot).await {
                    Ok(payload) => {
                        if let Some(id) = payload
                            .get("config")
                            .and_then(|config| config.get("id"))
                            .and_then(Value::as_str)
                        {
                            update(values, |values| {
                                values.insert(saved_channel_id_key(&platform), id.to_owned());
                            });
                        }
                        update(values, |values| {
                            values.remove(&arkret_pairing_state_key(&platform));
                        });
                        ui::set(error, "Channel saved".to_owned());
                    }
                    Err(message) => ui::set(error, message),
                }
                ui::set(busy, false);
            });
        });
    let back_ctx = ctx.clone();
    let mut children: Vec<Box<dyn Widget>> = vec![
        ui::error_text(ctx, error),
        Box::new(
            sapid::fieldset("channel-configuration")
                .legend("Platform configuration")
                .disabled(ctx.scope.bond(move || busy.get() || !loaded.get()))
                .fill((kind_control, name_control, fields_view, routers)),
        ),
        Box::new(pairing),
        Box::new(export_registration),
        Box::new(row().class("actions").fill((
            save,
            ui::action("Back to channels", move || back_ctx.navigate("/channels")),
        ))),
    ];
    if let Some(selector) = selector {
        let confirm = ctx.scope.cage(false);
        let delete_ctx = ctx.clone();
        children.push(Box::new(
            col().class("card danger").fill((
                sapid::checkbox()
                    .label("Confirm deleting this channel configuration")
                    .checked(confirm)
                    .on_checked_change(move |checked| ui::set(confirm, checked)),
                sapid::button()
                    .label("Delete channel")
                    .enabled(ctx.scope.bond(move || confirm.get() && !busy.get()))
                    .on_press(move || {
                        if !confirm.get() {
                            return;
                        }
                        let ctx = delete_ctx.clone();
                        let selector = selector.clone();
                        ctx.clone().spawn(async move {
                            ui::set(busy, true);
                            match ctx
                                .rpc
                                .call::<Value>(
                                    "channels.config.delete",
                                    Some(json!({"channel":selector})),
                                )
                                .await
                            {
                                Ok(value) if value["deleted"].as_bool() == Some(true) => {
                                    ctx.navigate("/channels")
                                }
                                Ok(_) => {
                                    ui::set(error, "Channel configuration was not found".to_owned())
                                }
                                Err(message) => ui::set(error, message),
                            }
                            ui::set(busy, false);
                        });
                    }),
            )),
        ));
    }
    ui::page(
        "Configure channel",
        "Set platform credentials, delivery routing and access policies",
        children,
    )
}

fn validate_form(
    kind: &str,
    fields: &[ConfigField],
    values: &HashMap<String, String>,
) -> Result<(), String> {
    if values
        .get(&channel_name_key(kind))
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err("Channel name is required".to_owned());
    }
    for field in fields {
        if field_is_visible(kind, field, values)
            && field_display_required(kind, field, values)
            && values
                .get(&field_value_key(kind, &field.key))
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!(
                "{} is required",
                field_display_label(kind, field, values)
            ));
        }
    }
    build_channel_patch(kind, fields, values)?;
    build_router_value(kind, values)?;
    Ok(())
}

fn router_fields(ctx: &AppContext, values: FormValues, platform: &str) -> Box<dyn Widget> {
    let platform = platform.to_owned();
    let mut children = vec![
        form_select(
            &ctx.scope,
            values,
            router_mode_key(&platform),
            "Agent routing",
            vec![
                (String::new(), "Default".into()),
                ("agent_id".into(), "Single agent".into()),
                ("route_rules".into(), "Routing rules".into()),
            ],
        ),
        form_text(
            &ctx.scope,
            values,
            router_agent_id_key(&platform),
            "Agent ID".into(),
            false,
            false,
        ),
        form_text(
            &ctx.scope,
            values,
            router_default_agent_id_key(&platform),
            "Fallback agent ID".into(),
            false,
            false,
        ),
        form_text(
            &ctx.scope,
            values,
            router_rules_key(&platform),
            "Routing rules (JSON)".into(),
            false,
            true,
        ),
    ];
    for (label, mode_key, list_key) in [
        (
            "Direct messages",
            dm_policy_mode_key(&platform),
            dm_policy_list_key(&platform),
        ),
        (
            "Group messages",
            group_policy_mode_key(&platform),
            group_policy_list_key(&platform),
        ),
    ] {
        children.push(form_select(
            &ctx.scope,
            values,
            mode_key,
            &format!("{label} policy"),
            vec![
                ("open".into(), "Open".into()),
                ("allowlist".into(), "Allow listed senders".into()),
                ("denylist".into(), "Deny listed senders".into()),
                ("disabled".into(), "Disabled".into()),
            ],
        ));
        children.push(form_text(
            &ctx.scope,
            values,
            list_key,
            format!("{label} sender IDs (comma separated)"),
            false,
            false,
        ));
    }
    Box::new(
        col()
            .class("card")
            .fill(
                text()
                    .class("card-title")
                    .content("Routing and access policies"),
            )
            .fill(children),
    )
}

fn arkret_pairing(
    ctx: &AppContext,
    values: FormValues,
    error: Cage<String>,
    busy: Cage<bool>,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
) -> Box<dyn Widget> {
    let status = ctx.scope.bond(move || {
        let snapshot = values.get();
        let bootstrap = snapshot
            .get(&field_value_key("arkret", "inksonBootstrap"))
            .cloned()
            .unwrap_or_default();
        let mut lines = Vec::new();
        if let Some((agent, station)) = arkret_bootstrap_summary_from_text(&bootstrap) {
            lines.push(format!("Agent: {agent}\nStation: {station}"));
        }
        if let Some(code) = arkret_pairing_code_from_bootstrap_text(&bootstrap) {
            lines.push(format!(
                "Pairing code: {}",
                format_arkret_pairing_code(&code)
            ));
        }
        if let Some(expiry) = arkret_pairing_expiry_from_bootstrap_text(&bootstrap) {
            lines.push(format!("Expires: {expiry}"));
        }
        if let Some(status) = snapshot.get("arkret.__pairing_message") {
            lines.push(status.clone());
        }
        lines.join("\n")
    });
    let pair_ctx = ctx.clone();
    let pair_generation = generation.clone();
    let start =
        sapid::button()
            .label("Start pairing / Check approval")
            .enabled(ctx.scope.bond(move || {
                !busy.get()
                    && !arkret_agent_is_bound("arkret", &values.get())
                    && arkret_runtime_key_request_can_request("arkret", &values.get())
            }))
            .on_press(move || {
                let ctx = pair_ctx.clone();
                let generation = pair_generation.clone();
                generation.set(generation.get().wrapping_add(1));
                let current = generation.get();
                ctx.clone().spawn(async move {
                    ui::set(busy, true);
                    ui::set(error, String::new());
                    let result = pair_arkret(&ctx, values, generation.clone(), current).await;
                    if generation.get() != current {
                        return;
                    }
                    if let Err(message) = result {
                        update(values, |values| {
                            if !values.get(&arkret_pairing_state_key("arkret")).is_some_and(
                                |state| matches!(state.as_str(), "save_failed" | "poll_failed"),
                            ) {
                                values
                                    .insert(arkret_pairing_state_key("arkret"), "error".to_owned());
                            }
                        });
                        ui::set(error, message);
                    }
                    ui::set(busy, false);
                });
            });
    let stop_generation = generation.clone();
    let stop = sapid::button()
        .label("Stop waiting")
        .enabled(values.map(|values| {
            values
                .get(&arkret_pairing_state_key("arkret"))
                .is_some_and(|state| state == "waiting")
        }))
        .on_press(move || {
            stop_generation.set(stop_generation.get().wrapping_add(1));
            ui::set(busy, false);
            update(values, |values| {
                finish_arkret_pairing_wait(
                    values,
                    &arkret_pairing_state_key("arkret"),
                    "arkret.__pairing_message",
                    "Stopped waiting. Check approval to resume this request.",
                )
            });
        });
    let confirmed = ctx.scope.cage(false);
    let unbind_ctx = ctx.clone();
    let unbind=sapid::button().label("Disconnect current agent").enabled(ctx.scope.bond(move ||confirmed.get() && !busy.get())).on_press(move || {
        if !confirmed.get(){return;}let ctx=unbind_ctx.clone();let snapshot=values.get();
        let channel=snapshot.get(&saved_channel_id_key("arkret")).cloned().unwrap_or_else(||"arkret".to_owned());
        ctx.clone().spawn(async move {
            ui::set(busy,true);
            match ctx.rpc.call::<Value>("channels.arkret.unbind",Some(json!({"platform":"arkret","channel":channel}))).await {
                Ok(result) if arkret_unbind_confirmed(&result)=>{
                    update(values,|values| {
                        for field in ["keyRef","inksonBootstrap","verificationMethod","authorizedEventRef","signerResolutionEvidenceRef","currentSignerEvidence","requestedScope"] {values.remove(&field_value_key("arkret",field));}
                        values.insert(saved_arkret_binding_key("arkret"),"false".to_owned());values.remove(&arkret_pairing_state_key("arkret"));
                        values.insert("arkret.__pairing_message".to_owned(),"Disconnected. Paste a new Inkson pairing link to connect another agent.".to_owned());
                    });ui::set(confirmed,false);
                },Ok(_)=>ui::set(error,"Disconnect was not confirmed; existing pairing retained".to_owned()),Err(message)=>ui::set(error,message)
            }ui::set(busy,false);
        });
    });
    Box::new(col().class("card arkret-pairing").fill((
        text().class("card-title").content("Inkson agent pairing"),
        text().content("Compare the pairing code with Inkson before approving. Runtime private keys remain on the gateway."),
        text().content(status),row().class("actions").fill((start,stop)),
        sapid::checkbox().label("Confirm disconnecting the current agent").checked(confirmed).on_checked_change(move |checked|ui::set(confirmed,checked)),unbind)))
}

async fn pair_arkret(
    ctx: &AppContext,
    values: FormValues,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
    current: u64,
) -> Result<(), String> {
    let kind = "arkret";
    let fields = &get_channel_types()
        .iter()
        .find(|platform| platform.id == kind)
        .unwrap()
        .config_fields;
    let mut snapshot = values.try_get().map_err(|_| "Channel editor closed")?;
    if current_arkret_mode(kind, &snapshot) == "applet" {
        return Err("Agent pairing is available in account mode".to_owned());
    }
    if snapshot
        .get(&arkret_pairing_state_key(kind))
        .is_some_and(|value| value == "save_failed")
    {
        persist_channel_form(&ctx.rpc, kind, fields, &snapshot).await?;
        update(values, |values| {
            values.remove(&arkret_pairing_state_key(kind));
            values.insert(
                "arkret.__pairing_message".to_owned(),
                "Agent paired and channel saved".to_owned(),
            );
        });
        return Ok(());
    }
    let bootstrap_key = field_value_key(kind, "inksonBootstrap");
    let input = snapshot.get(&bootstrap_key).cloned().unwrap_or_default();
    if input.trim().is_empty() {
        return Err("Paste the Inkson pairing link or bootstrap first".to_owned());
    }
    if !input.trim_start().starts_with('{') {
        let base = snapshot
            .get(&field_value_key(kind, "baseUrl"))
            .cloned()
            .unwrap_or_default();
        let response = ctx
            .rpc
            .call::<Value>(
                "channels.arkret.resolve_pairing_bootstrap",
                Some(json!({"input":input,"base_url":base})),
            )
            .await?;
        if generation.get() != current {
            return Ok(());
        }
        let bootstrap = response
            .get("inkson_bootstrap")
            .ok_or("No pairing bootstrap returned")?;
        snapshot.insert(
            bootstrap_key.clone(),
            serde_json::to_string(bootstrap).map_err(|error| error.to_string())?,
        );
        ui::set(values, snapshot.clone());
    }
    apply_arkret_pairing_identity(kind, &mut snapshot)?;
    ui::set(values, snapshot.clone());
    let key_ref_key = field_value_key(kind, "keyRef");
    let needs_key = snapshot
        .get(&key_ref_key)
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .is_none_or(|value| value["kind"].as_str() != Some("keyring"));
    if needs_key {
        let params = arkret_runtime_key_ref_generation_params(kind, &snapshot);
        let response = ctx
            .rpc
            .call::<Value>("channels.arkret.generate_runtime_key_ref", Some(params))
            .await?;
        if generation.get() != current {
            return Ok(());
        }
        let key_ref = response
            .get("key_ref")
            .ok_or("No runtime key reference returned")?;
        snapshot.insert(
            key_ref_key,
            serde_json::to_string(key_ref).map_err(|error| error.to_string())?,
        );
        ui::set(values, snapshot.clone());
    }
    if generation.get() != current {
        return Ok(());
    }
    let patch = build_channel_patch(kind, fields, &snapshot)?;
    let mut remembered = snapshot.clone();
    remember_arkret_scope_candidate(kind, &patch, &mut snapshot, &mut remembered)?;
    ui::set(values, remembered);
    let resume = snapshot
        .get(&arkret_pairing_state_key(kind))
        .is_some_and(|value| matches!(value.as_str(), "waiting" | "poll_failed"));
    if !resume {
        ctx.rpc
            .call::<Value>(
                "channels.arkret.runtime_key_request",
                Some(json!({"platform":kind,"config":patch.clone()})),
            )
            .await?;
    }
    if generation.get() != current {
        return Ok(());
    }
    let code = snapshot
        .get(&bootstrap_key)
        .and_then(|input| arkret_pairing_code_from_bootstrap_text(input));
    update(values, |values| {
        values.insert(arkret_pairing_state_key(kind), "waiting".to_owned());
        values.insert(
            "arkret.__pairing_message".to_owned(),
            arkret_waiting_for_approval_status(code.as_deref()),
        );
    });
    loop {
        crate::utils::sleep_ms(3_000).await;
        if generation.get() != current || values.try_get().is_err() {
            return Ok(());
        }
        let response = ctx
            .rpc
            .call::<Value>(
                "channels.arkret.runtime_key_request_status",
                Some(json!({"platform":kind,"config":patch.clone()})),
            )
            .await;
        if generation.get() != current {
            return Ok(());
        }
        let payload = match response {
            Ok(payload) => payload,
            Err(error) => {
                update(values, |values| {
                    finish_arkret_pairing_wait(
                        values,
                        &arkret_pairing_state_key(kind),
                        "arkret.__pairing_message",
                        "Approval check failed. Check approval to retry.",
                    )
                });
                return Err(error);
            }
        };
        if payload["approved"].as_bool() == Some(true) {
            let authorized = payload["authorized_event_ref"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or("Inkson approved without an authorization reference")?;
            let evidence = payload
                .get("signer_resolution_evidence_ref")
                .filter(|value| !value.is_null())
                .ok_or("Inkson approved without signer resolution evidence")?;
            let signer = payload
                .get("current_signer_evidence")
                .filter(|value| !value.is_null())
                .ok_or("Inkson approved without current signer evidence")?;
            update(values, |values| {
                values.insert(
                    field_value_key(kind, "authorizedEventRef"),
                    authorized.to_owned(),
                );
                values.insert(
                    field_value_key(kind, "signerResolutionEvidenceRef"),
                    evidence.to_string(),
                );
                values.insert(
                    field_value_key(kind, "currentSignerEvidence"),
                    signer.to_string(),
                );
                values.insert(arkret_pairing_state_key(kind), "finalizing".to_owned());
            });
            let snapshot = values.try_get().map_err(|_| "Channel editor closed")?;
            match persist_channel_form(&ctx.rpc, kind, fields, &snapshot).await {
                Ok(payload) => {
                    update(values, |values| {
                        if let Some(id) = payload["config"]["id"].as_str() {
                            values.insert(saved_channel_id_key(kind), id.to_owned());
                        }
                        values.remove(&arkret_pairing_state_key(kind));
                        values.insert(
                            "arkret.__pairing_message".to_owned(),
                            "Agent paired and channel saved".to_owned(),
                        );
                    });
                    return Ok(());
                }
                Err(error) => {
                    update(values, |values| {
                        values.insert(arkret_pairing_state_key(kind), "save_failed".to_owned());
                    });
                    return Err(format!(
                        "Approved in Inkson, but saving failed: {error}. Retry pairing to finish saving."
                    ));
                }
            }
        }
        if payload["paired_by_other_runtime"].as_bool() == Some(true) {
            return Err(
                "Another runtime completed this pairing. Create a new pairing in Inkson."
                    .to_owned(),
            );
        }
        if payload["status"].as_str() == Some("deactivated") {
            return Err("Agent was deactivated before pairing completed".to_owned());
        }
        if matches!(
            payload["runtime_state"].as_str(),
            Some("pairing_expired" | "ready")
        ) {
            return Err(
                "Pairing expired before approval. Create a new pairing in Inkson.".to_owned(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(scope: &Scope) -> AppContext {
        AppContext {
            scope: scope.clone(),
            rpc: WsRpc::new(),
            connected: scope.cage(false),
            locale: scope.cage(crate::i18n::Locale::En),
            path: scope.cage("/channels".to_owned()),
            authenticated: scope.cage(true),
        }
    }

    #[test]
    fn editor_renders_password_controls_and_all_channel_types() {
        let html = glory::ssr::render_with(|scope| render(&context(scope), "/channels/add"))
            .expect("channel editor SSR");
        assert!(
            html.contains("type=\"password\""),
            "credentials must use masked controls"
        );
        assert!(html.contains("Bot Token"));
        for platform in get_channel_types() {
            assert!(
                html.contains(&platform.name),
                "missing platform {}",
                platform.id
            );
        }
        assert!(html.contains("Routing and access policies"));
        assert!(
            !html.contains("Confirm deleting"),
            "new channel must not expose deletion"
        );
    }

    #[test]
    fn channel_status_keeps_whatsapp_and_matrix_workflows() {
        let whatsapp =
            glory::ssr::render_with(|scope| render(&context(scope), "/channels/whatsapp"))
                .expect("WhatsApp SSR");
        for action in ["Show QR", "Relink", "Check QR scan"] {
            assert!(whatsapp.contains(action));
        }
        let matrix = glory::ssr::render_with(|scope| render(&context(scope), "/channels/matrix"))
            .expect("Matrix SSR");
        assert!(matrix.contains("Refresh room invites"));
        let nostr = glory::ssr::render_with(|scope| render(&context(scope), "/channels/nostr"))
            .expect("Nostr SSR");
        for action in [
            "Save profile",
            "Import keypair",
            "Save relays",
            "Export profile",
        ] {
            assert!(nostr.contains(action));
        }
    }

    #[test]
    fn channel_form_validation_uses_only_the_selected_matrix_mode() {
        let fields = &get_channel_types()
            .iter()
            .find(|platform| platform.id == "matrix")
            .unwrap()
            .config_fields;
        let mut values = new_channel_form_values("matrix", fields, "Matrix".to_owned());
        let error = validate_form("matrix", fields, &values).expect_err("missing homeserver");
        assert!(error.contains("required"));
        for field in fields {
            if field_is_visible("matrix", field, &values)
                && field_display_required("matrix", field, &values)
            {
                values.insert(
                    field_value_key("matrix", &field.key),
                    "configured".to_owned(),
                );
            }
        }
        assert!(validate_form("matrix", fields, &values).is_ok());
        values.insert(router_mode_key("matrix"), "agent_id".to_owned());
        assert!(
            validate_form("matrix", fields, &values)
                .unwrap_err()
                .contains("requires an agent ID")
        );
    }

    #[test]
    fn appservice_yaml_quotes_external_values() {
        let yaml = registration_to_yaml(
            &json!({"id":"quote\"\nvalue","namespaces":{"users":[{"exclusive":true,"regex":"@agent\\..*:example.org"}]}}),
        );
        assert!(yaml.contains("id: \"quote\\\"\\nvalue\""));
        assert!(yaml.contains("regex: \"@agent\\\\..*:example.org\""));
        assert!(yaml.contains("  rooms: []\n"));
    }

    #[test]
    fn instance_status_has_priority_over_platform_summary() {
        let status = json!({"channels":{"arkret":{"connected":true}},"instances":{"arkret-second":{"connected":false}}});
        assert_eq!(
            channel_status(&status, "arkret-second", "arkret")["connected"],
            json!(false)
        );
        assert_eq!(
            channel_status(&status, "arkret-other", "arkret")["connected"],
            json!(true)
        );
    }

    #[test]
    fn status_requests_separate_platform_from_instance_id() {
        use std::future::Future;
        let rpc = WsRpc::new();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        for (selector, expected) in [
            (
                "arkret-team",
                json!({"channel":"arkret","id":"arkret-team","probe":true}),
            ),
            ("arkret", json!({"channel":"arkret","probe":true})),
        ] {
            let mut request = Box::pin(status_params(&rpc, selector));
            match request.as_mut().poll(&mut context) {
                std::task::Poll::Ready(Ok(params)) => assert_eq!(params, expected),
                _ => panic!("known channel selector must not need a network lookup"),
            }
        }
    }
}
