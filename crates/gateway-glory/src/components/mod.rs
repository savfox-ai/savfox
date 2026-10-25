//! Semantic application shell; widgets own their subscriptions and async work.
mod approvals;
pub mod markdown_renderer;
mod semantic_markdown;

use glory::prelude::*;

use crate::api::client::validate_token;
use crate::api::ws::{WsRpc, clear_token, get_token, set_token};
use crate::i18n::{self, Locale};
use crate::ui::{AppContext, action, error_text, set};

pub fn app(scope: &Scope) -> Box<dyn Widget> {
    let ctx = AppContext {
        scope: scope.clone(),
        rpc: WsRpc::new(),
        connected: scope.cage(false),
        locale: scope.cage(i18n::detect_locale()),
        path: scope.cage(crate::route::current_path()),
        authenticated: scope.cage(get_token().is_some()),
    };
    let reconnect_epoch = scope.cage(0_u64);
    if ctx.authenticated.get() {
        ctx.rpc.connect(ctx.connected, reconnect_epoch);
    }
    let rpc = ctx.rpc.clone();
    let _ = scope.on_cleanup(move || rpc.disconnect());
    install_history(&ctx);
    let authenticated = ctx.authenticated;
    let yes = ctx.clone();
    let no = ctx.clone();
    Box::new(
        when(authenticated, move || {
            let yes = yes.clone();
            widget(move |scope| shell(&yes.with_scope(scope)).build(scope))
        })
        .otherwise(move || {
            let no = no.clone();
            widget(move |scope| auth(&no.with_scope(scope), reconnect_epoch).build(scope))
        }),
    )
}

fn install_history(ctx: &AppContext) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        let path = ctx.path;
        let handler = Closure::wrap(
            Box::new(move || set(path, crate::route::current_path())) as Box<dyn FnMut()>
        );
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("popstate", handler.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = window.remove_event_listener_with_callback(
                    "popstate",
                    handler.as_ref().unchecked_ref(),
                );
                drop(handler);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ctx;
}

fn locale_control(ctx: &AppContext) -> Box<dyn Widget> {
    let mut buttons = Vec::<Box<dyn Widget>>::new();
    for locale in Locale::ALL.iter().copied() {
        let selected = ctx.locale;
        buttons.push(action(locale.label(), move || {
            i18n::save_locale(locale);
            set(selected, locale);
            #[cfg(target_arch = "wasm32")]
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                document.set_title(&i18n::t(locale, "app.title"));
                if let Some(root) = document.document_element() {
                    let _ = root.set_attribute("lang", locale.code());
                }
            }
        }));
    }
    Box::new(row().class("locale-control").fill(buttons))
}

fn auth(ctx: &AppContext, reconnect_epoch: Cage<u64>) -> Box<dyn Widget> {
    let token = ctx.scope.cage(String::new());
    let error = ctx.scope.cage(String::new());
    let busy = ctx.scope.cage(false);
    let submit_ctx = ctx.clone();
    let title_ctx = ctx.clone();
    let subtitle_ctx = ctx.clone();
    let button_ctx = ctx.clone();
    Box::new(
        col().class("auth-container").fill(
            col().class("auth-card").fill((
                image()
                    .source("/assets/savfox.svg")
                    .alt("Savfox")
                    .class("auth-logo-img"),
                locale_control(ctx),
                text()
                    .class("auth-title")
                    .kind(TextKind::Heading(1))
                    .content(ctx.scope.bond(move || title_ctx.tr("auth.title"))),
                text()
                    .class("auth-subtitle")
                    .content(ctx.scope.bond(move || subtitle_ctx.tr("auth.subtitle"))),
                sapid::password_input()
                    .name("Gateway token")
                    .value(token)
                    .on_value_change(move |value| set(token, value)),
                error_text(ctx, error),
                sapid::button()
                    .class("auth-submit")
                    .label(ctx.scope.bond(move || button_ctx.tr("auth.connect")))
                    .loading(busy)
                    .on_press(move || {
                        let value = token.get().trim().to_owned();
                        if value.is_empty() || busy.get() {
                            return;
                        }
                        set(busy, true);
                        set(error, String::new());
                        let ctx = submit_ctx.clone();
                        submit_ctx.spawn(async move {
                            if validate_token(&value).await {
                                set_token(&value);
                                ctx.rpc.connect(ctx.connected, reconnect_epoch);
                                set(ctx.authenticated, true);
                            } else {
                                set(error, ctx.tr("auth.invalid_token"));
                            }
                            set(busy, false);
                        });
                    }),
            )),
        ),
    )
}

fn shell(ctx: &AppContext) -> Box<dyn Widget> {
    let palette_open = ctx.scope.cage(false);
    keyboard_shortcuts(ctx, palette_open);
    let mut links = Vec::<Box<dyn Widget>>::new();
    for &(path, label) in crate::route::NAVIGATION {
        let selected = ctx.path;
        let locale = ctx.locale;
        let navigation = ctx.clone();
        let class = ctx.scope.bond(move || {
            if crate::route::is_active(&selected.get(), path) {
                "nav-link active".into()
            } else {
                "nav-link".into()
            }
        });
        links.push(Box::new(
            sapid::anchor()
                .class(class)
                .href(path)
                .label(ctx.scope.bond(move || {
                    let translated = i18n::t(locale.get(), label);
                    if translated == label {
                        path.trim_start_matches('/').replace('-', " ")
                    } else {
                        translated
                    }
                }))
                .on_navigate(move |_| navigation.navigate(path)),
        ));
    }
    let connected = ctx.connected;
    let logout_ctx = ctx.clone();
    let path = ctx.path;
    let routes = ctx.scope.bond(move || vec![path.get()]);
    let routed = ctx.clone();
    Box::new(
        row().class("gateway-shell").fill((
            col().class("gateway-sidebar").fill((
                image()
                    .source("/assets/savfox.svg")
                    .alt("Savfox")
                    .class("sidebar-logo"),
                text().class("brand").content("Savfox"),
                col().class("gateway-navigation").fill(links),
                locale_control(ctx),
                action("Sign out", move || {
                    logout_ctx.rpc.disconnect();
                    clear_token();
                    set(logout_ctx.connected, false);
                    set(logout_ctx.authenticated, false);
                }),
            )),
            col().class("gateway-main").fill((
                row().class("gateway-topbar").fill((
                    text().class("gateway-label").content("Workspace"),
                    Box::new(
                        button()
                            .label("Search pages (Ctrl+K)")
                            .semantics(ctx.scope.bond(move || {
                                Some(Box::new(Semantics {
                                    popup: Some(glory::render::PopupKind::Dialog),
                                    expanded: Some(palette_open.get()),
                                    ..Default::default()
                                }))
                            }))
                            .on(activate, move |_| set(palette_open, true)),
                    ) as Box<dyn Widget>,
                    text()
                        .class(ctx.scope.bond(move || {
                            if connected.get() {
                                "connection connected".into()
                            } else {
                                "connection disconnected".into()
                            }
                        }))
                        .content(ctx.scope.bond(move || {
                            if connected.get() {
                                "Connected".into()
                            } else {
                                "Connecting…".into()
                            }
                        })),
                )),
                approvals::center(ctx),
                command_palette(ctx, palette_open),
                each(
                    routes,
                    |value| value.clone(),
                    move |route, _| {
                        let ctx = routed.clone();
                        widget(move |scope| {
                            crate::pages::render(&ctx.with_scope(scope), &route.get_untracked())
                                .build(scope)
                        })
                    },
                ),
            )),
        )),
    )
}

fn command_palette(ctx: &AppContext, open: Cage<bool>) -> Box<dyn Widget> {
    let navigation = ctx.clone();
    let actions = crate::route::NAVIGATION
        .iter()
        .enumerate()
        .map(|(index, (_, label))| {
            let translated = ctx.tr(label);
            sapid::command_action(format!("nav-{index}"), translated)
        })
        .collect::<Vec<_>>();
    Box::new(
        sapid::command("gateway-navigation")
            .label("Go to page")
            .open(open)
            .on_open_change(move |value| set(open, value))
            .actions(actions)
            .on_action(move |key| {
                if let Some(index) = key
                    .strip_prefix("nav-")
                    .and_then(|value| value.parse::<usize>().ok())
                    && let Some((path, _)) = crate::route::NAVIGATION.get(index)
                {
                    set(open, false);
                    navigation.navigate(path);
                }
            }),
    )
}

fn keyboard_shortcuts(ctx: &AppContext, open: Cage<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let handler = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
                if (event.ctrl_key() || event.meta_key()) && event.key().eq_ignore_ascii_case("k") {
                    event.prevent_default();
                    set(open, !open.get());
                } else if event.key() == "Escape" && open.get() {
                    set(open, false);
                }
            }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
            let _ = document
                .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = document.remove_event_listener_with_callback(
                    "keydown",
                    handler.as_ref().unchecked_ref(),
                );
                drop(handler);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, open);
}
