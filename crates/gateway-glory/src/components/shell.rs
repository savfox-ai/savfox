//! The persistent application frame, matching the original Gateway layout.
use glory::prelude::*;

use crate::api::ws::clear_token;
use crate::i18n;
use crate::ui::{AppContext, set};

type NavItem = (&'static str, &'static str, &'static str);
type NavGroup = (&'static str, &'static [NavItem]);

const PRIMARY_LINKS: &[NavItem] = &[
    ("/sessions", "nav.sessions", "message"),
    ("/models", "nav.models", "brain"),
    ("/approvals", "nav.approvals", "shield"),
];

const GROUPS: &[NavGroup] = &[
    (
        "nav.manage",
        &[
            ("/agents", "nav.agents", "bot"),
            ("/channels", "nav.channels", "radio"),
            ("/cron", "nav.cron_jobs", "clock"),
            ("/skills", "nav.skills", "star"),
            ("/speech", "nav.speech", "volume"),
            ("/devices", "nav.devices", "network"),
        ],
    ),
    (
        "nav.observe",
        &[
            ("/overview", "nav.overview", "home"),
            ("/logs", "nav.logs", "logs"),
            ("/usage", "nav.usage", "chart"),
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
    press: impl FnMut() + 'static,
) -> Box<dyn Widget> {
    Box::new(
        sapid::button()
            .unstyled()
            .name(translated(ctx, key))
            .class(class.into())
            .fill((
                icon(glyph),
                text().class("nav-label").content(translated(ctx, key)),
            ))
            .on_press(press),
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
        sapid::anchor()
            .unstyled()
            .href(path)
            .name(label)
            .class(class)
            .fill((
                text()
                    .class(format!("{glyph_class} shell-icon shell-icon--{glyph}"))
                    .content(""),
                text().class("nav-label").content(label),
            ))
            .on_navigate(move |_| {
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
    let sidebar = ctx
        .scope
        .cage(!narrow_viewport() && !crate::route::is_session_path(&ctx.path.get_untracked()));
    let more = ctx.scope.cage(false);
    let theme = ctx.scope.cage(saved_theme());
    let initial_group = active_group(&ctx.path.get_untracked());
    let groups = [
        ctx.scope.cage(initial_group == Some(0)),
        ctx.scope.cage(initial_group == Some(1)),
    ];
    let path = ctx.path;
    ctx.scope.effect(move || {
        let current = path.get();
        if let Some(group) = active_group(&current)
            && !groups[group].get_untracked()
        {
            set(groups[group], true);
        }
    });
    ctx.scope.effect(move || apply_theme(&theme.get()));
    install_system_theme(ctx, theme);
    watch_theme(ctx, theme);
    install_viewport(ctx, sidebar, more);
    install_shortcuts(ctx, palette, sidebar, more);
    install_swipe(ctx, sidebar);

    let header_left = row().class("top-header__left").fill((
        sapid::burger()
            .unstyled()
            .size(sapid::Size::Small)
            .class("hamburger-btn")
            .labels(
                translated(ctx, "aria.open_nav"),
                translated(ctx, "aria.close_nav"),
            )
            .open(sidebar)
            .on_open_change(move |open| set(sidebar, open)),
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
    let connection_label = ctx.scope.bond(move || {
        i18n::t(
            locale.get(),
            if connected.get() {
                "header.health_ok"
            } else {
                "header.offline"
            },
        )
    });
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
            .name(connection_label)
            .class(ctx.scope.bond(move || {
                if connected.get() {
                    "top-header__health top-header__health--ok connection connected".into()
                } else {
                    "top-header__health top-header__health--err connection disconnected".into()
                }
            }))
            .fill((
                view().class("top-header__health-dot"),
                text().content(connection_label),
            )),
    ));

    let mut rail = Vec::<Box<dyn Widget>>::new();
    let mut full = Vec::<Box<dyn Widget>>::new();
    full.push(Box::new(
        col().class("nav-group-links sidebar-primary-links").fill(
            PRIMARY_LINKS
                .iter()
                .map(|&(path, key, glyph)| {
                    nav_link(ctx, path, key, glyph, "nav-link", sidebar, more)
                })
                .collect::<Vec<_>>(),
        ),
    ));
    for &(path, key, glyph) in PRIMARY_LINKS {
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
    rail.push(Box::new(
        sapid::separator()
            .unstyled()
            .decorative(true)
            .class("sidebar-rail__divider"),
    ));
    for (index, &(key, links)) in GROUPS.iter().enumerate() {
        let expanded = groups[index];
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
        let label = translated(ctx, key);
        let mut nav = Some(nav);
        full.push(Box::new(
            sapid::collapsible(format!("nav-group-{index}"))
                .unstyled()
                .class("nav-group")
                .label(label)
                .open(expanded)
                .on_open_change(move |open| set(expanded, open))
                .content_policy(sapid::ContentPolicy::KeepMounted)
                .trigger_content(move || {
                    group().fill((
                        glory::ui::graphic()
                            .class("nav-group-chevron")
                            .drawing(expanded.map(|open| {
                                let icon = if *open {
                                    sapid::Icon::ChevronDown
                                } else {
                                    sapid::Icon::ChevronRight
                                };
                                icon.drawing(glory::style::Color(0, 0, 0, 255))
                            })),
                        text().content(label),
                    ))
                })
                .content(move || {
                    col()
                        .class("nav-group-links")
                        .fill(nav.take().unwrap_or_default())
                }),
        ));
        if index < GROUPS.len() - 1 {
            rail.push(Box::new(
                sapid::separator()
                    .unstyled()
                    .decorative(true)
                    .class("sidebar-rail__divider"),
            ));
        }
    }
    rail.push(nav_link(
        ctx,
        "/config",
        "nav.config",
        "settings",
        "sidebar-rail__item sidebar-rail__settings",
        sidebar,
        more,
    ));
    let footer = col().class("sidebar-footer").fill((
        nav_link(
            ctx,
            "/config",
            "nav.config",
            "settings",
            "nav-link sidebar-config-link",
            sidebar,
            more,
        ),
        sapid::button()
            .unstyled()
            .class("sidebar-palette-btn")
            .name(translated(ctx, "nav.command_palette"))
            .fill((
                text()
                    .class("sidebar-palette-label")
                    .content(translated(ctx, "nav.command_palette")),
                text().class("sidebar-palette-shortcut").content("Ctrl+K"),
            ))
            .on_press(move || set(palette, true)),
    ));
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
            if crate::route::is_session_path(path) {
                "main-content main-content--sessions gateway-main".into()
            } else {
                "main-content gateway-main".into()
            }
        }))
        .fill((
            super::approvals::center(ctx),
            super::command_palette(ctx, palette),
            crate::ui::modal_host(ctx),
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
    let overlay = sapid::button()
        .unstyled()
        .name(translated(ctx, "aria.close_nav"))
        .class(sidebar.map(|open| {
            if *open {
                "sidebar-overlay visible".into()
            } else {
                "sidebar-overlay".into()
            }
        }))
        .on_press(move || set(sidebar, false));
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

fn mobile_navigation(ctx: &AppContext, sidebar: Cage<bool>, more: Cage<bool>) -> Box<dyn Widget> {
    let primary = [
        ("/sessions", "nav.sessions", "message"),
        ("/models", "nav.models", "brain"),
        ("/agents", "nav.agents", "bot"),
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
    for &(path, key, glyph) in PRIMARY_LINKS {
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
    links.push(nav_link(
        ctx,
        "/config",
        "nav.config",
        "settings",
        "more-sheet__link",
        sidebar,
        more,
    ));
    let mut drawer_links = Some(links);
    Box::new(
        col().class("mobile-navigation").fill((
            row()
                .class("bottom-tab-bar")
                .role(Role::Navigation)
                .name(translated(ctx, "nav.navigation"))
                .fill(row().class("bottom-tab-bar__items").fill(tabs)),
            sapid::drawer("mobile-navigation")
                .class("mobile-navigation-drawer")
                .show_trigger(false)
                .side(sapid::DrawerSide::Bottom)
                .title(translated(ctx, "nav.navigation"))
                .close_label(translated(ctx, "common.close"))
                .open(more)
                .on_open_change(move |open| set(more, open))
                .content_policy(sapid::ContentPolicy::KeepMounted)
                .content(move || {
                    col()
                        .class("more-sheet__nav")
                        .fill(drawer_links.take().unwrap_or_default())
                }),
        )),
    )
}

fn active_group(path: &str) -> Option<usize> {
    GROUPS.iter().position(|(_, links)| {
        links
            .iter()
            .any(|(target, ..)| crate::route::is_active(path, target))
    })
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
            let previous = storage.get_item("savfox_theme").ok().flatten();
            let _ = storage.set_item("savfox_theme", value);
            if previous.as_deref() != Some(value)
                && let Ok(event) = web_sys::Event::new("savfox:preferences")
            {
                let _ = window.dispatch_event(&event);
            }
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

pub(super) fn watch_theme(ctx: &AppContext, theme: Cage<String>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;
        if let Some(window) = web_sys::window() {
            let callback =
                Closure::wrap(Box::new(move || set(theme, saved_theme())) as Box<dyn FnMut()>);
            let _ = window.add_event_listener_with_callback(
                "savfox:preferences",
                callback.as_ref().unchecked_ref(),
            );
            let _ = ctx.scope.on_cleanup(move || {
                let _ = window.remove_event_listener_with_callback(
                    "savfox:preferences",
                    callback.as_ref().unchecked_ref(),
                );
                drop(callback);
            });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ctx, theme);
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
            let path = ctx.path;
            let callback = Closure::wrap(Box::new(move || {
                set(
                    sidebar,
                    !responsive.matches() && !crate::route::is_session_path(&path.get_untracked()),
                );
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
        assert_eq!(super::active_group("/sessions?session_id=one"), None);
        assert_eq!(super::active_group("/agents/alice/files"), Some(0));
        assert_eq!(super::active_group("/logs"), Some(1));
        assert_eq!(super::active_group("/config/gateway"), None);
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
            "sapid-collapsible-trigger",
            "sidebar-palette-btn",
            "top-header__health",
            "top-header__bell",
            "bottom-tab-bar",
            "more-sheet",
        ] {
            assert!(
                html.contains(landmark),
                "missing original frame landmark {landmark}"
            );
        }
        assert!(html.contains("SAVFOX"));
        assert!(!html.contains("top-header__theme-group"));
        assert!(!html.contains("top-header__lang-group"));
        assert!(!html.contains("Search pages (Ctrl+K)"));
    }
}
