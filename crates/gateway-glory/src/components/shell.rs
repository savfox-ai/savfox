//! The persistent application frame, matching the original Gateway layout.
use glory::prelude::*;

use crate::api::ws::clear_token;
use crate::i18n::{self, Locale};
use crate::ui::{AppContext, set};

type NavItem = (&'static str, &'static str, &'static str);
type NavGroup = (&'static str, &'static [NavItem]);

const GROUPS: &[NavGroup] = &[
    (
        "nav.dashboard",
        &[
            ("/", "nav.overview", "home"),
            ("/sessions", "nav.sessions", "message"),
        ],
    ),
    (
        "nav.manage",
        &[
            ("/agents", "nav.agents", "bot"),
            ("/models", "nav.models", "brain"),
            ("/channels", "nav.channels", "radio"),
            ("/cron", "nav.cron_jobs", "clock"),
            ("/skills", "nav.skills", "star"),
        ],
    ),
    (
        "nav.media",
        &[
            ("/tts", "nav.tts", "volume"),
            ("/voice", "nav.voice", "mic"),
        ],
    ),
    (
        "nav.system",
        &[
            ("/config", "nav.config", "settings"),
            ("/instances", "nav.instances", "server"),
            ("/logs", "nav.logs", "logs"),
            ("/usage", "nav.usage", "chart"),
            ("/approvals", "nav.approvals", "shield"),
            ("/nodes", "nav.nodes", "network"),
            ("/debug", "nav.debug", "bug"),
            ("/device-pair", "nav.device_pair", "device"),
            ("/settings", "nav.settings", "settings"),
        ],
    ),
];

fn translated(ctx: &AppContext, key: &'static str) -> Bond<String> {
    let locale = ctx.locale;
    ctx.scope.bond(move || i18n::t(locale.get(), key))
}

fn icon(name: &str) -> impl Widget + use<> {
    text()
        .class(format!("shell-icon shell-icon--{name}"))
        .content("")
}

fn icon_button(
    ctx: &AppContext,
    key: &'static str,
    glyph: &str,
    class: impl Into<String>,
    mut press: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        button()
            .name(translated(ctx, key))
            .class(class.into())
            .fill((
                icon(glyph),
                text().class("nav-label").content(translated(ctx, key)),
            ))
            .on(activate, move |_| press()),
    )
}

fn nav_link(
    ctx: &AppContext,
    path: &'static str,
    key: &'static str,
    glyph: &'static str,
    class: &'static str,
    sidebar: Cage<bool>,
    more: Cage<bool>,
) -> Box<dyn Widget> {
    let selected = ctx.path;
    let navigation = ctx.clone();
    let label = translated(ctx, key);
    let glyph_class = if class == "nav-link" {
        "nav-icon"
    } else if class == "bottom-tab-bar__item" {
        "bottom-tab-bar__icon"
    } else if class == "more-sheet__link" {
        "more-sheet__link-icon"
    } else {
        "shell-icon-wrap"
    };
    let class = ctx.scope.bond(move || {
        if crate::route::is_active(&selected.get(), path) {
            format!("{class} active")
        } else {
            class.to_owned()
        }
    });
    Box::new(
        link()
            .target(path)
            .name(label)
            .class(class)
            .semantics(Some(Box::new(Semantics {
                link_request: true,
                ..Default::default()
            })))
            .fill((
                text()
                    .class(format!("{glyph_class} shell-icon shell-icon--{glyph}"))
                    .content(""),
                text().class("nav-label").content(label),
            ))
            .on(activate, move |_| {
                set(more, false);
                if narrow_viewport() {
                    set(sidebar, false);
                }
                navigation.navigate(path);
            }),
    )
}

pub(super) fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let palette = ctx.scope.cage(false);
    let sidebar = ctx.scope.cage(!narrow_viewport());
    let more = ctx.scope.cage(false);
    let theme = ctx.scope.cage(saved_theme());
    let initial_group = active_group(&ctx.path.get_untracked());
    let groups = [
        ctx.scope.cage(true),
        ctx.scope.cage(true),
        ctx.scope.cage(initial_group == 2),
        ctx.scope.cage(initial_group == 3),
    ];
    let path = ctx.path;
    ctx.scope.effect(move || {
        let current = path.get();
        let group = active_group(&current);
        if !groups[group].get_untracked() {
            set(groups[group], true);
        }
    });
    ctx.scope.effect(move || apply_theme(&theme.get()));
    install_system_theme(ctx, theme);
    install_palette_focus(ctx, palette);
    install_viewport(ctx, sidebar, more);
    install_shortcuts(ctx, palette, sidebar, more);
    install_swipe(ctx, sidebar);

    let toggle_label = ctx.locale;
    let header_left = row().class("top-header__left").fill((
        button()
            .class("hamburger-btn")
            .name(ctx.scope.bond(move || {
                i18n::t(
                    toggle_label.get(),
                    if sidebar.get() {
                        "aria.close_nav"
                    } else {
                        "aria.open_nav"
                    },
                )
            }))
            .semantics(sidebar.map(|open| {
                Some(Box::new(Semantics {
                    expanded: Some(*open),
                    ..Default::default()
                }))
            }))
            .fill(icon("menu"))
            .on(activate, move |_| set(sidebar, !sidebar.get())),
        row().class("top-header__brand").fill((
            image()
                .source("/assets/savfox.svg")
                .alt("Savfox")
                .class("top-header__logo"),
            text().class("top-header__title").content("SAVFOX"),
            text()
                .class("top-header__subtitle")
                .content(translated(ctx, "header.ai_assistant")),
        )),
    ));
    let connected = ctx.connected;
    let locale = ctx.locale;
    let header_right = row().class("top-header__right").fill((
        nav_link(
            ctx,
            "/approvals",
            "nav.approvals",
            "bell",
            "top-header__bell",
            sidebar,
            more,
        ),
        row()
            .role(Role::Status)
            .class(ctx.scope.bond(move || {
                if connected.get() {
                    "top-header__health top-header__health--ok connection connected".into()
                } else {
                    "top-header__health top-header__health--err connection disconnected".into()
                }
            }))
            .fill((
                view().class("top-header__health-dot"),
                text().content(ctx.scope.bond(move || {
                    i18n::t(
                        locale.get(),
                        if connected.get() {
                            "header.health_ok"
                        } else {
                            "header.offline"
                        },
                    )
                })),
            )),
        locale_buttons(ctx),
        theme_buttons(ctx, theme),
    ));

    let mut rail = Vec::<Box<dyn Widget>>::new();
    let mut full = Vec::<Box<dyn Widget>>::new();
    for (index, &(key, links)) in GROUPS.iter().enumerate() {
        let expanded = groups[index];
        full.push(Box::new(
            button()
                .class("nav-group-header")
                .name(translated(ctx, key))
                .semantics(expanded.map(|value| {
                    Some(Box::new(Semantics {
                        expanded: Some(*value),
                        ..Default::default()
                    }))
                }))
                .fill((
                    text().class(expanded.map(|value| {
                        if *value {
                            "shell-icon shell-icon--chevron-down".into()
                        } else {
                            "shell-icon shell-icon--chevron-right".into()
                        }
                    })),
                    text().content(translated(ctx, key)),
                ))
                .on(activate, move |_| set(expanded, !expanded.get())),
        ));
        let mut nav = Vec::new();
        for &(path, key, glyph) in links {
            nav.push(nav_link(ctx, path, key, glyph, "nav-link", sidebar, more));
            rail.push(nav_link(
                ctx,
                path,
                key,
                glyph,
                "sidebar-rail__item",
                sidebar,
                more,
            ));
        }
        full.push(Box::new(
            col()
                .class(expanded.map(|value| {
                    if *value {
                        "nav-group-links".into()
                    } else {
                        "nav-group-links is-collapsed".into()
                    }
                }))
                .fill(nav),
        ));
        if index < GROUPS.len() - 1 {
            rail.push(Box::new(view().class("sidebar-rail__divider")));
        }
    }
    let footer = col().class("sidebar-footer").fill(
        button()
            .class("sidebar-palette-btn")
            .name(translated(ctx, "nav.command_palette"))
            .fill((
                text()
                    .class("sidebar-palette-label")
                    .content(translated(ctx, "nav.command_palette")),
                text().class("sidebar-palette-shortcut").content("Ctrl+K"),
            ))
            .on(activate, move |_| {
                remember_palette_focus();
                set(palette, true);
            }),
    );
    let sidebar_view = col()
        .role(Role::Navigation)
        .name(translated(ctx, "aria.main_nav"))
        .class(sidebar.map(|open| {
            if *open {
                "sidebar gateway-sidebar open".into()
            } else {
                "sidebar gateway-sidebar".into()
            }
        }))
        .fill((
            col().class("sidebar-rail").fill(rail),
            col().class("sidebar-nav").fill(full),
            footer,
        ));
    let routed = ctx.clone();
    let routes = ctx.scope.bond(move || vec![path.get()]);
    let main = col()
        .role(Role::Main)
        .class(path.map(|path| {
            if path.split('?').next() == Some("/sessions") {
                "main-content main-content--sessions gateway-main".into()
            } else {
                "main-content gateway-main".into()
            }
        }))
        .fill((
            super::approvals::center(ctx),
            super::command_palette(ctx, palette),
            col()
                .class("route-outlet")
                .fill(each(routes).key(Clone::clone).children(move |each_row| {
                    let route = each_row.value;
                    let context = routed.clone();
                    widget(move |scope| {
                        crate::pages::render(&context.with_scope(scope), &route.get_untracked())
                            .build(scope)
                    })
                })),
        ));
    let overlay = button()
        .name(translated(ctx, "aria.close_nav"))
        .class(sidebar.map(|open| {
            if *open {
                "sidebar-overlay visible".into()
            } else {
                "sidebar-overlay".into()
            }
        }))
        .on(activate, move |_| set(sidebar, false));
    Box::new(
        col().class("app-layout gateway-shell").fill((
            row()
                .class("top-header gateway-topbar")
                .role(Role::Banner)
                .fill((header_left, header_right)),
            row().class("app-body").fill((overlay, sidebar_view, main)),
            mobile_navigation(ctx, sidebar, more),
        )),
    )
}

fn locale_buttons(ctx: &AppContext) -> Box<dyn Widget> {
    let mut buttons = Vec::<Box<dyn Widget>>::new();
    for locale in Locale::ALL.iter().copied() {
        let current = ctx.locale;
        buttons.push(Box::new(
            button()
                .label(locale.label())
                .class(current.map(move |value| {
                    if *value == locale {
                        "top-header__lang-btn active".into()
                    } else {
                        "top-header__lang-btn".into()
                    }
                }))
                .on(activate, move |_| {
                    i18n::save_locale(locale);
                    set(current, locale);
                    #[cfg(target_arch = "wasm32")]
                    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                        document.set_title(&i18n::t(locale, "app.title"));
                        if let Some(root) = document.document_element() {
                            let _ = root.set_attribute("lang", locale.code());
                        }
                    }
                }),
        ));
    }
    Box::new(row().class("top-header__lang-group").fill(buttons))
}

fn theme_buttons(ctx: &AppContext, theme: Cage<String>) -> Box<dyn Widget> {
    let mut controls = Vec::<Box<dyn Widget>>::new();
    for (value, label, glyph) in [
        ("system", "header.follow_system_theme", "monitor"),
        ("light", "header.light_theme", "sun"),
        ("dark", "header.dark_theme", "moon"),
    ] {
        let value = value.to_owned();
        let selected = value.clone();
        controls.push(Box::new(
            button()
                .name(translated(ctx, label))
                .class(theme.map(move |current| {
                    if *current == selected {
                        "top-header__theme-btn active".into()
                    } else {
                        "top-header__theme-btn".into()
                    }
                }))
                .semantics(theme.map({
                    let value = value.clone();
                    move |current| {
                        Some(Box::new(Semantics {
                            pressed: Some(*current == value),
                            ..Default::default()
                        }))
                    }
                }))
                .fill(icon(glyph))
                .on(activate, move |_| set(theme, value.clone())),
        ));
    }
    Box::new(
        row()
            .class("top-header__theme-group")
            .name(translated(ctx, "aria.theme"))
            .fill(controls),
    )
}

fn mobile_navigation(ctx: &AppContext, sidebar: Cage<bool>, more: Cage<bool>) -> Box<dyn Widget> {
    let primary = [
        ("/", "nav.overview", "home"),
        ("/sessions", "nav.sessions", "message"),
        ("/agents", "nav.agents", "bot"),
        ("/channels", "nav.channels", "radio"),
    ];
    let mut tabs = primary
        .iter()
        .map(|&(path, key, glyph)| {
            nav_link(ctx, path, key, glyph, "bottom-tab-bar__item", sidebar, more)
        })
        .collect::<Vec<_>>();
    tabs.push(icon_button(
        ctx,
        "nav.more",
        "more",
        "bottom-tab-bar__item bottom-tab-bar__more",
        move || set(more, !more.get()),
    ));
    let mut links = Vec::new();
    for &(_, entries) in GROUPS {
        for &(path, key, glyph) in entries {
            links.push(nav_link(
                ctx,
                path,
                key,
                glyph,
                "more-sheet__link",
                sidebar,
                more,
            ));
        }
    }
    Box::new(
        col().class("mobile-navigation").fill((
            row()
                .class("bottom-tab-bar")
                .role(Role::Navigation)
                .name(translated(ctx, "nav.navigation"))
                .fill(row().class("bottom-tab-bar__items").fill(tabs)),
            button()
                .name(translated(ctx, "common.close"))
                .class(more.map(|open| {
                    if *open {
                        "more-sheet-backdrop visible".into()
                    } else {
                        "more-sheet-backdrop".into()
                    }
                }))
                .on(activate, move |_| set(more, false)),
            col()
                .class(more.map(|open| {
                    if *open {
                        "more-sheet open".into()
                    } else {
                        "more-sheet".into()
                    }
                }))
                .semantics(more.map(|open| {
                    Some(Box::new(Semantics {
                        hidden: !*open,
                        ..Default::default()
                    }))
                }))
                .fill((
                    view().class("more-sheet__handle"),
                    text()
                        .class("more-sheet__title")
                        .content(translated(ctx, "nav.navigation")),
                    col().class("more-sheet__nav").fill(links),
                )),
        )),
    )
}

fn active_group(path: &str) -> usize {
    GROUPS
        .iter()
        .position(|(_, links)| {
            links
                .iter()
                .any(|(target, ..)| crate::route::is_active(path, target))
        })
        .unwrap_or(0)
}

fn saved_theme() -> String {
    #[cfg(target_arch = "wasm32")]
    if let Some(value) = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("savfox_theme").ok().flatten())
        && matches!(value.as_str(), "system" | "light" | "dark")
    {
        return value;
    }
    "system".into()
}

fn narrow_viewport() -> bool {
    #[cfg(target_arch = "wasm32")]
    if let Some(width) = web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|value| value.as_f64())
    {
        return width <= 768.0;
    }
    false
}

fn apply_theme(value: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        let effective = if value == "system" {
            if system_dark() { "dark" } else { "light" }
        } else {
            value
        };
        if let Some(storage) = window.local_storage().ok().flatten() {
            let _ = storage.set_item("savfox_theme", value);
        }
        if let Some(document) = window.document() {
            if let Some(root) = document.document_element() {
                let _ = root.set_attribute("data-theme", effective);
            }
            if let Some(body) = document.body() {
                let _ = body.set_attribute("data-theme", effective);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = value;
}

pub(super) fn initialize_theme() {
    apply_theme(&saved_theme());
}

#[cfg(target_arch = "wasm32")]
fn system_dark() -> bool {
    web_sys::window()
        .and_then(|window| {
            window
                .match_media("(prefers-color-scheme: dark)")
                .ok()
                .flatten()
        })
        .is_some_and(|query| query.matches())
}

fn install_system_theme(ctx: &AppContext, theme: Cage<String>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(media) = web_sys::window().and_then(|window| {
            window
                .match_media("(prefers-color-scheme: dark)")
                .ok()
                .flatten()
        }) {
            let callback = Closure::wrap(Box::new(move || {
                if theme.get() == "system" {
                    apply_theme("system");
                }
            }) as Box<dyn FnMut()>);
            let _ =
                media.add_event_listener_with_callback("change", callback.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = media.remove_event_listener_with_callback(
                    "change",
                    callback.as_ref().unchecked_ref(),
                );
                drop(callback);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, theme);
}

fn install_viewport(ctx: &AppContext, sidebar: Cage<bool>, more: Cage<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(media) = web_sys::window()
            .and_then(|window| window.match_media("(max-width: 768px)").ok().flatten())
        {
            let responsive = media.clone();
            let callback = Closure::wrap(Box::new(move || {
                set(sidebar, !responsive.matches());
                set(more, false);
            }) as Box<dyn FnMut()>);
            let _ =
                media.add_event_listener_with_callback("change", callback.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = media.remove_event_listener_with_callback(
                    "change",
                    callback.as_ref().unchecked_ref(),
                );
                drop(callback);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, sidebar, more);
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static PALETTE_FOCUS: std::cell::RefCell<Option<web_sys::HtmlElement>> = const { std::cell::RefCell::new(None) };
}

fn remember_palette_focus() {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        PALETTE_FOCUS.with(|previous| {
            if previous.borrow().is_none() {
                *previous.borrow_mut() = web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.active_element())
                    .and_then(|element| element.dyn_into().ok());
            }
        });
    }
}

fn install_palette_focus(ctx: &AppContext, palette: Cage<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        // The visible launcher belongs to the application frame, while Sapid's
        // internal trigger is hidden. Return to the actual invoking control.
        ctx.scope.effect(move || {
            let Some(window) = web_sys::window() else {
                return;
            };
            if palette.get() {
                remember_palette_focus();
            } else if let Some(element) =
                PALETTE_FOCUS.with(|previous| previous.borrow_mut().take())
            {
                // Run after the modal layer has removed its focus trap.
                let callback = Closure::once_into_js(move || {
                    if element.is_connected() {
                        let _ = element.focus();
                    }
                });
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    0,
                );
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, palette);
}

fn install_shortcuts(ctx: &AppContext, palette: Cage<bool>, sidebar: Cage<bool>, more: Cage<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let navigation = ctx.clone();
            let callback = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
                let control = event.ctrl_key() || event.meta_key();
                if control && event.key().eq_ignore_ascii_case("k") {
                    event.prevent_default();
                    if !palette.get() {
                        remember_palette_focus();
                    }
                    set(palette, !palette.get());
                } else if control && event.key() == "/" {
                    event.prevent_default();
                    navigation.navigate("/sessions");
                } else if control && event.key() == "," {
                    event.prevent_default();
                    navigation.navigate("/config");
                } else if control && event.shift_key() && event.key().eq_ignore_ascii_case("l") {
                    event.prevent_default();
                    navigation.navigate("/logs");
                } else if event.key() == "Escape" {
                    if palette.get() {
                        set(palette, false);
                    } else if more.get() {
                        set(more, false);
                    } else if narrow_viewport() {
                        set(sidebar, false);
                    }
                }
            }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
            let _ = document
                .add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = document.remove_event_listener_with_callback(
                    "keydown",
                    callback.as_ref().unchecked_ref(),
                );
                drop(callback);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, palette, sidebar, more);
}

fn install_swipe(ctx: &AppContext, sidebar: Cage<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        use std::cell::Cell;
        use std::rc::Rc;

        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let origin = Rc::new(Cell::new((0.0, 0.0)));
            let start_origin = origin.clone();
            let start = Closure::wrap(Box::new(move |event: web_sys::TouchEvent| {
                if let Some(touch) = event.touches().item(0) {
                    start_origin.set((touch.client_x() as f64, touch.client_y() as f64));
                }
            }) as Box<dyn FnMut(web_sys::TouchEvent)>);
            let end = Closure::wrap(Box::new(move |event: web_sys::TouchEvent| {
                if !narrow_viewport() {
                    return;
                }
                if let Some(touch) = event.changed_touches().item(0) {
                    let (x, y) = origin.get();
                    let dx = touch.client_x() as f64 - x;
                    let dy = touch.client_y() as f64 - y;
                    if dx.abs() > 60.0 && dx.abs() > dy.abs() {
                        if x < 24.0 && dx > 0.0 {
                            set(sidebar, true);
                        } else if sidebar.get() && dx < 0.0 {
                            set(sidebar, false);
                        }
                    }
                }
            }) as Box<dyn FnMut(web_sys::TouchEvent)>);
            let _ = document
                .add_event_listener_with_callback("touchstart", start.as_ref().unchecked_ref());
            let _ =
                document.add_event_listener_with_callback("touchend", end.as_ref().unchecked_ref());
            let _ = ctx.scope.on_cleanup(move || {
                let _ = document.remove_event_listener_with_callback(
                    "touchstart",
                    start.as_ref().unchecked_ref(),
                );
                let _ = document
                    .remove_event_listener_with_callback("touchend", end.as_ref().unchecked_ref());
                drop((start, end));
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, sidebar);
}

pub(super) fn sign_out(ctx: &AppContext) {
    ctx.rpc.disconnect();
    clear_token();
    set(ctx.connected, false);
    set(ctx.authenticated, false);
}

#[cfg(test)]
mod tests {
    #[test]
    fn deep_links_expand_the_original_navigation_group() {
        assert_eq!(super::active_group("/sessions?session_id=one"), 0);
        assert_eq!(super::active_group("/agents/alice/files"), 1);
        assert_eq!(super::active_group("/voice"), 2);
        assert_eq!(super::active_group("/config/gateway"), 3);
    }

    #[test]
    fn persistent_frame_retains_header_group_navigation_and_mobile_landmarks() {
        let html = glory::ssr::render_with(|scope| {
            let context = crate::ui::AppContext::new(scope);
            super::render(&context)
        })
        .expect("the application frame renders without a browser");
        let header = html
            .find("top-header gateway-topbar")
            .expect("persistent header");
        let body = html.find("app-body").expect("sidebar and content body");
        assert!(
            header < body,
            "the full-width header must precede both columns"
        );
        for landmark in [
            "sidebar-nav",
            "sidebar-rail",
            "nav-group-header",
            "sidebar-palette-btn",
            "top-header__theme-group",
            "top-header__lang-group",
            "bottom-tab-bar",
            "more-sheet",
        ] {
            assert!(
                html.contains(landmark),
                "missing original frame landmark {landmark}"
            );
        }
        assert!(html.contains("SAVFOX"));
        assert!(!html.contains("Search pages (Ctrl+K)"));
    }
}
