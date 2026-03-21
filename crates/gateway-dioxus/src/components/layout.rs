use dioxus::prelude::*;
use lucide_dioxus::{
    Bot, Brain, Bug, ChartBar, ChevronDown, ChevronRight, Clock, Ellipsis,
    LayoutDashboard, MessageSquareText, Mic, Monitor, Moon, Network, PlugZap,
    Radio, ScrollText, Server, Settings, ShieldCheck, Star, Sun, Volume2,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::ExecApprovalFull;
use crate::api::ws::WsRpc;
use crate::components::command_palette::CommandPalette;
use crate::components::exec_approval_modal::ExecApprovalModal;
use crate::components::toast::{ToastContainer, Toaster};
use crate::i18n::{self, Locale, save_locale, use_i18n};
use crate::route::Route;
use crate::utils::notifications;

const SAVFOX_LOGO: Asset = asset!("/assets/savfox.svg");

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NavGroup {
    Main,
    Manage,
    Media,
    System,
}

impl NavGroup {
    fn label(&self, locale: Locale) -> String {
        match self {
            NavGroup::Main => i18n::t(locale, "nav.dashboard"),
            NavGroup::Manage => i18n::t(locale, "nav.manage"),
            NavGroup::Media => i18n::t(locale, "nav.media"),
            NavGroup::System => i18n::t(locale, "nav.system"),
        }
    }
}

/// Read the saved theme preference from localStorage, defaulting to "system".
fn read_saved_theme() -> String {
    web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
        .and_then(|s| s.get_item("savfox_theme").ok())
        .flatten()
        .unwrap_or_else(|| "system".to_string())
}

/// Determine which nav group a route belongs to.
fn route_group(route: &Route) -> NavGroup {
    match route {
        Route::Overview {} | Route::Sessions {} => NavGroup::Main,
        Route::Agents {}
        | Route::AgentsNew {}
        | Route::AgentsDetail { .. }
        | Route::Models {}
        | Route::Channels {}
        | Route::ChannelsAdd {}
        | Route::ChannelsEdit { .. }
        | Route::ChannelsHealth { .. }
        | Route::Cron {}
        | Route::CronNew {}
        | Route::CronDetail { .. }
        | Route::ConnectProvider {}
        | Route::Skills {} => NavGroup::Manage,
        Route::Tts {} | Route::Voice {} => NavGroup::Media,
        _ => NavGroup::System,
    }
}

/// Check if viewport is narrow (mobile/tablet) — sidebar should auto-close on nav.
fn is_narrow_viewport() -> bool {
    web_sys::window()
        .map(|w| {
            w.inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(1024.0)
                < 768.0
        })
        .unwrap_or(false)
}

/// Detect OS-level dark mode preference via `prefers-color-scheme`.
fn system_prefers_dark() -> bool {
    js_sys::eval("window.matchMedia('(prefers-color-scheme: dark)').matches")
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Async sleep using `setTimeout` (no external crate needed).
async fn wasm_sleep(ms: u32) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let cb = Closure::once(move || {
        let _ = tx.send(());
    });
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms as i32,
        );
    }
    cb.forget();
    let _ = rx.await;
}

/// Navigate to a path using the History API (SPA-friendly).
fn navigate_to(path: &str) {
    if let Some(w) = web_sys::window() {
        if let Ok(history) = w.history() {
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path));
            // Dispatch a popstate event so the Dioxus router picks it up
            if let Ok(event) = web_sys::PopStateEvent::new("popstate") {
                let _ = w.dispatch_event(&event);
            }
        }
    }
}

/// Persist theme choice and set the `data-theme` attribute on `<body>`.
/// Accepts "system", "light", or "dark".
fn apply_theme(theme: &str) {
    let effective = match theme {
        "light" => "light",
        "dark" => "dark",
        _ => {
            if system_prefers_dark() {
                "dark"
            } else {
                "light"
            }
        }
    };
    if let Some(w) = web_sys::window() {
        // Persist the user's choice (not the resolved value)
        if let Ok(Some(storage)) = w.local_storage() {
            let _ = storage.set_item("savfox_theme", theme);
        }
        // Apply resolved theme to <body>
        if let Some(doc) = w.document() {
            if let Some(body) = doc.body() {
                let _ = body.set_attribute("data-theme", effective);
            }
        }
    }
}

/// Install touch event listeners for swipe-to-open/close sidebar.
/// Swipe right from the left edge (startX < 20px, deltaX > 60px) opens sidebar.
/// Swipe left when sidebar is open (deltaX < -60px) closes it.
fn install_swipe_handler(mut sidebar_open: Signal<bool>) {
    use std::cell::Cell;
    use std::rc::Rc;

    let start_x = Rc::new(Cell::new(0.0_f64));
    let start_y = Rc::new(Cell::new(0.0_f64));
    let swiping = Rc::new(Cell::new(false));

    // touchstart
    {
        let start_x = start_x.clone();
        let start_y = start_y.clone();
        let swiping = swiping.clone();
        let cb = Closure::wrap(Box::new(move |e: web_sys::TouchEvent| {
            if let Some(touch) = e.touches().get(0) {
                start_x.set(touch.client_x() as f64);
                start_y.set(touch.client_y() as f64);
                swiping.set(true);
            }
        }) as Box<dyn FnMut(web_sys::TouchEvent)>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    }

    // touchmove — detect horizontal swipe
    {
        let start_x = start_x.clone();
        let start_y = start_y.clone();
        let swiping = swiping.clone();
        let cb = Closure::wrap(Box::new(move |e: web_sys::TouchEvent| {
            if !swiping.get() {
                return;
            }
            if let Some(touch) = e.touches().get(0) {
                let dx = touch.client_x() as f64 - start_x.get();
                let dy = (touch.client_y() as f64 - start_y.get()).abs();

                // Must be a horizontal swipe (|dx| > dy)
                if dx.abs() < 20.0 || dy > dx.abs() {
                    return;
                }

                let sx = start_x.get();

                // Swipe right from left edge to open sidebar
                if sx < 20.0 && dx > 60.0 {
                    swiping.set(false);
                    sidebar_open.set(true);
                }

                // Swipe left to close sidebar when open
                if sidebar_open() && dx < -60.0 {
                    swiping.set(false);
                    sidebar_open.set(false);
                }
            }
        }) as Box<dyn FnMut(web_sys::TouchEvent)>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("touchmove", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    }

    // touchend — reset swiping state
    {
        let cb = Closure::wrap(Box::new(move |_: web_sys::TouchEvent| {
            swiping.set(false);
        }) as Box<dyn FnMut(web_sys::TouchEvent)>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    }
}

#[component]
pub fn Layout() -> Element {
    let (mut locale_sig, t) = use_i18n();
    let locale = locale_sig();

    let ws_connected = use_signal(|| false);
    let ws_reconnect_epoch = use_signal(|| 0u64);
    let ws = use_signal(WsRpc::new);
    let mut sidebar_open = use_signal(|| !is_narrow_viewport());

    // Determine initial group expansion from URL (deep-link support)
    let initial_route: Route = use_route();
    let init_group = route_group(&initial_route);
    let mut expanded_main =
        use_signal(move || init_group == NavGroup::Main || init_group == NavGroup::Manage);
    let mut expanded_manage =
        use_signal(move || init_group == NavGroup::Main || init_group == NavGroup::Manage);
    let mut expanded_media = use_signal(move || init_group == NavGroup::Media);
    let mut expanded_system = use_signal(move || init_group == NavGroup::System);
    let mut theme = use_signal(|| read_saved_theme());

    // "More" bottom sheet state (for mobile bottom tab bar)
    let mut more_open = use_signal(|| false);

    // Pending exec approvals
    let mut pending_approvals = use_signal(|| Vec::<ExecApprovalFull>::new());
    let mut palette_open = use_signal(|| false);

    // Signal to auto-show the approval modal when a new WS event arrives
    let mut show_approval_modal = use_signal(|| false);

    // Request browser notification permission on first render
    use_effect(move || {
        notifications::request_notification_permission();
    });

    // Apply theme on first render and whenever it changes.
    use_effect(move || {
        let current = theme();
        apply_theme(&current);
    });

    // Install swipe-to-open/close sidebar (touch events) once
    use_effect(move || {
        let _s = sidebar_open();
        install_swipe_handler(sidebar_open);
    });

    // Global keyboard shortcuts
    use_effect(move || {
        let _open = palette_open();
        let cb = Closure::wrap(Box::new(move |e: web_sys::KeyboardEvent| {
            let ctrl = e.ctrl_key() || e.meta_key();

            // Ctrl+K -- open command palette
            if ctrl && e.key() == "k" {
                e.prevent_default();
                palette_open.toggle();
                return;
            }

            // Ctrl+/ -- navigate to Sessions
            if ctrl && e.key() == "/" {
                e.prevent_default();
                navigate_to("/sessions");
                return;
            }

            // Ctrl+, -- navigate to Config
            if ctrl && e.key() == "," {
                e.prevent_default();
                navigate_to("/config");
                return;
            }

            // Ctrl+Shift+L -- navigate to Logs
            if ctrl && e.shift_key() && (e.key() == "L" || e.key() == "l") {
                e.prevent_default();
                navigate_to("/logs");
                return;
            }

            // Escape -- close palette / more sheet / sidebar
            if e.key() == "Escape" {
                if palette_open() {
                    palette_open.set(false);
                } else if more_open() {
                    more_open.set(false);
                } else if sidebar_open() {
                    sidebar_open.set(false);
                }
            }
        }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    });

    use_context_provider(|| ws);
    use_context_provider(|| ws.read().clone());
    use_context_provider(|| ws_connected);
    use_context_provider(|| ws_reconnect_epoch);
    use_context_provider(|| sidebar_open);
    use_context_provider(Toaster::new);

    let ws_approvals = ws.read().clone();
    let ws_approvals_notif = ws_approvals.clone();

    use_effect(move || {
        let ws = ws.read();
        ws.connect(ws_connected, ws_reconnect_epoch);
    });

    // Gate child page rendering on first successful WS connection
    let mut ws_ever_connected = use_signal(|| false);

    // Track disconnection state for the WS banner (T034)
    let mut was_disconnected = use_signal(|| false);
    let mut show_reconnected = use_signal(|| false);

    use_effect(move || {
        let connected = ws_connected();
        if connected {
            if was_disconnected() {
                // We just reconnected after a disconnection — show green banner briefly
                show_reconnected.set(true);
                was_disconnected.set(false);

                // Hide the "Connected" banner after 3 seconds
                let mut show_reconnected_clone = show_reconnected;
                spawn(async move {
                    wasm_sleep(3000).await;
                    show_reconnected_clone.set(false);
                });
            }
            ws_ever_connected.set(true);
        } else if ws_ever_connected() {
            // Was connected before but lost connection
            was_disconnected.set(true);
            show_reconnected.set(false);
        }
    });

    // Poll for pending approvals periodically
    let mut approval_tick = use_signal(|| 0u32);
    let approvals_data = use_resource(move || {
        let _c = ws_connected();
        let _t = approval_tick();
        let ws = ws_approvals.clone();
        async move {
            ws.call::<serde_json::Value>("approvals.list", None)
                .await
                .ok()
                .and_then(|v| {
                    serde_json::from_value::<Vec<ExecApprovalFull>>(
                        v.get("pending")
                            .cloned()
                            .unwrap_or(serde_json::Value::Array(vec![])),
                    )
                    .ok()
                })
                .unwrap_or_default()
        }
    });

    // Update pending_approvals when data changes
    use_effect(move || {
        if let Some(data) = approvals_data.read().as_ref() {
            pending_approvals.set(data.clone());
        }
    });

    // Listen for new exec approval events from WebSocket.
    // When a new approval request arrives, auto-show the modal and send a
    // browser notification so the user notices even when the tab is in the
    // background.
    {
        let ws_for_notif = ws_approvals_notif.clone();
        use_effect(move || {
            let _connected = ws_connected();
            let ws_ref = ws_for_notif.clone();
            ws_ref.on_notification_mut("approvals.new", move |_params| {
                // Refresh the approval list
                approval_tick += 1;
                // Auto-show the approval modal
                show_approval_modal.set(true);
                // Fire a browser notification
                notifications::send_notification(
                    &i18n::t(locale, "notifications.exec_approval_required"),
                    &i18n::t(locale, "notifications.new_command_approval"),
                );
            });
        });
    }

    let current_route: Route = use_route();

    // Auto-expand the nav group containing the current route & close sidebar on mobile
    {
        let route_for_effect = current_route.clone();
        use_effect(move || {
            let group = route_group(&route_for_effect);
            match group {
                NavGroup::Main => expanded_main.set(true),
                NavGroup::Manage => expanded_manage.set(true),
                NavGroup::Media => expanded_media.set(true),
                NavGroup::System => expanded_system.set(true),
            }
            if is_narrow_viewport() {
                sidebar_open.set(false);
            }
        });
    }

    let hamburger_class = if sidebar_open() {
        "hamburger-btn open"
    } else {
        "hamburger-btn"
    };

    let sidebar_class = if sidebar_open() {
        "sidebar open"
    } else {
        "sidebar"
    };

    let overlay_class = if sidebar_open() {
        "sidebar-overlay visible"
    } else {
        "sidebar-overlay"
    };

    let approvals_count = pending_approvals().len();

    // Determine which bottom tab is active
    let tab_overview_active = current_route == Route::Overview {};
    let tab_sessions_active = current_route == Route::Sessions {};
    let tab_agents_active = matches!(
        current_route,
        Route::Agents {} | Route::AgentsNew {} | Route::AgentsDetail { .. }
    );
    let tab_channels_active = matches!(
        current_route,
        Route::Channels {}
            | Route::ChannelsAdd {}
            | Route::ChannelsEdit { .. }
            | Route::ChannelsHealth { .. }
    );

    // "More" is highlighted if current route is not one of the primary tabs
    let tab_more_active =
        !tab_overview_active && !tab_sessions_active && !tab_agents_active && !tab_channels_active;

    let more_backdrop_class = if more_open() {
        "more-sheet-backdrop visible"
    } else {
        "more-sheet-backdrop"
    };

    let more_sheet_class = if more_open() {
        "more-sheet open"
    } else {
        "more-sheet"
    };
    let main_content_class = if tab_sessions_active {
        "main-content main-content--sessions"
    } else {
        "main-content"
    };
    let route_render_epoch = ws_reconnect_epoch();

    let health_label = if ws_connected() {
        t("header.health_ok")
    } else {
        t("header.offline")
    };
    let health_class = if ws_connected() {
        "top-header__health top-header__health--ok"
    } else {
        "top-header__health top-header__health--err"
    };

    rsx! {
        ToastContainer {}
        CommandPalette {
            open: palette_open(),
            on_close: move |_| palette_open.set(false),
        }
        // Skip navigation link for keyboard/screen reader users
        a {
            class: "skip-nav",
            href: "#main-content",
            {t("nav.skip_to_content")}
        }
        div { class: "app-layout",
            // ── Persistent top header ──
            header { class: "top-header",
                div { class: "top-header__left",
                    {
                        let aria_nav = if sidebar_open() { t("aria.close_nav") } else { t("aria.open_nav") };
                        rsx! {
                            button {
                                class: "{hamburger_class}",
                                aria_label: "{aria_nav}",
                                aria_expanded: "{sidebar_open}",
                                onclick: move |_| {
                                    let current = sidebar_open();
                                    sidebar_open.set(!current);
                                },
                                span { class: "hamburger-line", aria_hidden: "true" }
                                span { class: "hamburger-line", aria_hidden: "true" }
                                span { class: "hamburger-line", aria_hidden: "true" }
                            }
                        }
                    }
                    div { class: "top-header__brand",
                        img {
                            class: "top-header__logo",
                            src: SAVFOX_LOGO,
                            alt: "Savfox Logo",
                            width: "44",
                            height: "44",
                        }
                        span { class: "top-header__title", "SAVFOX" }
                        span { class: "top-header__subtitle", {t("header.ai_assistant")} }
                    }
                }
                div { class: "top-header__right",
                    // Notification bell with unread badge
                    {
                        let approvals_aria = if approvals_count > 0 {
                            i18n::t_args(locale, "header.pending_approvals", &[("count", &approvals_count.to_string())])
                        } else {
                            t("header.no_pending_approvals")
                        };
                        let approvals_title = t("nav.approvals");
                        rsx! {
                            Link {
                                to: Route::Approvals {},
                                class: "top-header__bell",
                                aria_label: "{approvals_aria}",
                                title: "{approvals_title}",
                                svg {
                                    width: "18", height: "18", view_box: "0 0 24 24",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    path { d: "M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" }
                                    path { d: "M13.73 21a2 2 0 0 1-3.46 0" }
                                }
                                if approvals_count > 0 {
                                    span {
                                        class: "top-header__bell-badge",
                                        aria_hidden: "true",
                                        "{approvals_count}"
                                    }
                                }
                            }
                        }
                    }
                    // Health badge
                    div {
                        class: "{health_class}",
                        role: "status",
                        aria_live: "polite",
                        span { class: "top-header__health-dot", aria_hidden: "true" }
                        "{health_label}"
                    }
                    // Language selector
                    div { class: "top-header__lang-group",
                        for locale_option in Locale::ALL {
                            button {
                                class: if *locale_option == locale { "top-header__lang-btn active" } else { "top-header__lang-btn" },
                                onclick: move |_| {
                                    locale_sig.set(*locale_option);
                                    save_locale(*locale_option);
                                },
                                "{locale_option.label()}"
                            }
                        }
                    }
                    // Theme button group: System | Light | Dark
                    {
                        let theme_system = t("header.follow_system_theme");
                        let theme_light = t("header.light_theme");
                        let theme_dark = t("header.dark_theme");
                        let theme_aria = t("aria.theme");
                        rsx! {
                            div { class: "top-header__theme-group", role: "radiogroup", aria_label: "{theme_aria}",
                                button {
                                    class: if theme() == "system" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                                    title: "{theme_system}",
                                    aria_label: "{theme_system}",
                                    onclick: move |_| theme.set("system".to_string()),
                                    Monitor { size: 16 }
                                }
                                button {
                                    class: if theme() == "light" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                                    title: "{theme_light}",
                                    aria_label: "{theme_light}",
                                    onclick: move |_| theme.set("light".to_string()),
                                    Sun { size: 16 }
                                }
                                button {
                                    class: if theme() == "dark" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                                    title: "{theme_dark}",
                                    aria_label: "{theme_dark}",
                                    onclick: move |_| theme.set("dark".to_string()),
                                    Moon { size: 16 }
                                }
                            }
                        }
                    }
                }
            }

            // ── Body (sidebar + content) ──
            div { class: "app-body",
                // Sidebar overlay (mobile)
                div {
                    class: "{overlay_class}",
                    onclick: move |_| sidebar_open.set(false),
                }

                // Sidebar
                nav {
                    class: "{sidebar_class}",
                    aria_label: "{t(\"aria.main_nav\")}",
                    role: "navigation",

                    // Icon rail (visible when sidebar is collapsed on desktop)
                    div { class: "sidebar-rail",
                        { rail_icon_link(&current_route, Route::Overview {}, &t("nav.overview"), rsx! { LayoutDashboard { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Sessions {}, &t("nav.sessions"), rsx! { MessageSquareText { size: 18 } }) }
                        div { class: "sidebar-rail__divider" }
                        { rail_icon_link(&current_route, Route::Agents {}, &t("nav.agents"), rsx! { Bot { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Models {}, &t("nav.models"), rsx! { Brain { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Channels {}, &t("nav.channels"), rsx! { Radio { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Cron {}, &t("nav.cron_jobs"), rsx! { Clock { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Skills {}, &t("nav.skills"), rsx! { Star { size: 18 } }) }
                        div { class: "sidebar-rail__divider" }
                        { rail_icon_link(&current_route, Route::Tts {}, &t("nav.tts"), rsx! { Volume2 { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Voice {}, &t("nav.voice"), rsx! { Mic { size: 18 } }) }
                        div { class: "sidebar-rail__divider" }
                        { rail_icon_link(&current_route, Route::Config {}, &t("nav.config"), rsx! { Settings { size: 18 } }) }
                        { rail_icon_link(&current_route, Route::Logs {}, &t("nav.logs"), rsx! { ScrollText { size: 18 } }) }
                        { rail_icon_link_with_badge(&current_route, Route::Approvals {}, &t("nav.approvals"), rsx! { ShieldCheck { size: 18 } }, approvals_count) }
                    }

                    // Nav links (visible when sidebar is expanded)
                    div { class: "sidebar-nav",
                    // Dashboard group
                    { nav_group_header(&t("nav.dashboard"), expanded_main(), move |_| expanded_main.toggle()) }
                    if expanded_main() {
                        { nav_link(&current_route,Route::Overview {}, &t("nav.overview"), rsx! { LayoutDashboard { size: 16 } }) }
                        { nav_link(&current_route,Route::Sessions {}, &t("nav.sessions"), rsx! { MessageSquareText { size: 16 } }) }
                    }

                    // Manage group
                    { nav_group_header(&t("nav.manage"), expanded_manage(), move |_| expanded_manage.toggle()) }
                    if expanded_manage() {
                        { nav_link(&current_route,Route::Agents {}, &t("nav.agents"), rsx! { Bot { size: 16 } }) }
                        { nav_link(&current_route,Route::Models {}, &t("nav.models"), rsx! { Brain { size: 16 } }) }
                        { nav_link(&current_route,Route::Channels {}, &t("nav.channels"), rsx! { Radio { size: 16 } }) }
                        { nav_link(&current_route,Route::Cron {}, &t("nav.cron_jobs"), rsx! { Clock { size: 16 } }) }
                        { nav_link(&current_route,Route::Skills {}, &t("nav.skills"), rsx! { Star { size: 16 } }) }
                    }

                    // Media group
                    { nav_group_header(&t("nav.media"), expanded_media(), move |_| expanded_media.toggle()) }
                    if expanded_media() {
                        { nav_link(&current_route,Route::Tts {}, &t("nav.tts"), rsx! { Volume2 { size: 16 } }) }
                        { nav_link(&current_route,Route::Voice {}, &t("nav.voice"), rsx! { Mic { size: 16 } }) }
                    }

                    // System group
                    { nav_group_header(&t("nav.system"), expanded_system(), move |_| expanded_system.toggle()) }
                    if expanded_system() {
                        { nav_link(&current_route,Route::Config {}, &t("nav.config"), rsx! { Settings { size: 16 } }) }
                        { nav_link(&current_route,Route::Instances {}, &t("nav.instances"), rsx! { Server { size: 16 } }) }
                        { nav_link(&current_route,Route::Logs {}, &t("nav.logs"), rsx! { ScrollText { size: 16 } }) }
                        { nav_link(&current_route,Route::Usage {}, &t("nav.usage"), rsx! { ChartBar { size: 16 } }) }
                        { nav_link_with_badge(&current_route,Route::Approvals {}, &t("nav.approvals"), rsx! { ShieldCheck { size: 16 } }, approvals_count) }
                        { nav_link(&current_route,Route::Nodes {}, &t("nav.nodes"), rsx! { Network { size: 16 } }) }
                        { nav_link(&current_route,Route::Debug {}, &t("nav.debug"), rsx! { Bug { size: 16 } }) }
                    }
                }

                // Footer with shortcut hint
                div { class: "sidebar-footer",
                    button {
                        class: "sidebar-palette-btn",
                        onclick: move |_| palette_open.set(true),
                        span { class: "sidebar-palette-label", {t("nav.command_palette")} }
                        span { class: "sidebar-palette-shortcut", "Ctrl+K" }
                    }
                }
            }

                // Main content
                main {
                    id: "main-content",
                    class: "{main_content_class}",
                    role: "main",
                    // WS disconnection / reconnection banner (T034)
                    if !ws_connected() && ws_ever_connected() {
                        div { class: "ws-banner ws-banner--disconnected",
                            span { {t("ws.connection_lost")} }
                            button {
                                class: "ws-banner__btn",
                                onclick: move |_| {
                                    let ws_ref = ws.read();
                                    ws_ref.connect(ws_connected, ws_reconnect_epoch);
                                },
                                {t("ws.reconnect_now")}
                            }
                        }
                    }
                    if show_reconnected() {
                        div { class: "ws-banner ws-banner--connected",
                            span { {t("ws.connected")} }
                        }
                    }
                    if ws_ever_connected() {
                        div {
                            key: "route-{route_render_epoch}",
                            class: "route-outlet",
                            Outlet::<Route> {}
                        }
                    } else {
                        div { class: "main-content__connecting",
                            {t("ws.connecting")}
                        }
                    }
                }
            } // close app-body

            // ---- Bottom Tab Bar (visible on <480px via CSS) ----
            nav { class: "bottom-tab-bar",
                ul { class: "bottom-tab-bar__items",
                    // Tab: Overview
                    li {
                        Link {
                            to: Route::Overview {},
                            class: if tab_overview_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", LayoutDashboard { size: 18 } }
                            span { class: "bottom-tab-bar__label", {t("nav.overview")} }
                        }
                    }
                    // Tab: Session
                    li {
                        Link {
                            to: Route::Sessions {},
                            class: if tab_sessions_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", MessageSquareText { size: 18 } }
                            span { class: "bottom-tab-bar__label", {t("nav.session")} }
                        }
                    }
                    // Tab: Agents
                    li {
                        Link {
                            to: Route::Agents {},
                            class: if tab_agents_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", Bot { size: 18 } }
                            span { class: "bottom-tab-bar__label", {t("nav.agents")} }
                        }
                    }
                    // Tab: Channels
                    li {
                        Link {
                            to: Route::Channels {},
                            class: if tab_channels_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", Radio { size: 18 } }
                            span { class: "bottom-tab-bar__label", {t("nav.channels")} }
                        }
                    }
                    // Tab: More (opens bottom sheet)
                    li {
                        button {
                            class: if tab_more_active || more_open() { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            onclick: move |_| {
                                let current = more_open();
                                more_open.set(!current);
                            },
                            span { class: "bottom-tab-bar__icon", Ellipsis { size: 18 } }
                            span { class: "bottom-tab-bar__label", {t("nav.more")} }
                        }
                    }
                }
            }

            // ---- "More" bottom sheet backdrop ----
            div {
                class: "{more_backdrop_class}",
                onclick: move |_| more_open.set(false),
            }

            // ---- "More" bottom sheet with remaining nav items ----
            div { class: "{more_sheet_class}",
                div { class: "more-sheet__handle" }
                div { class: "more-sheet__title", {t("nav.navigation")} }
                div { class: "more-sheet__nav",
                    { more_sheet_link(&current_route, more_open, Route::Models {}, &t("nav.models"), rsx! { Brain { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Channels {}, &t("nav.channels"), rsx! { Radio { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Cron {}, &t("nav.cron_jobs"), rsx! { Clock { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Config {}, &t("nav.config"), rsx! { Settings { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Instances {}, &t("nav.instances"), rsx! { Server { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Logs {}, &t("nav.logs"), rsx! { ScrollText { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Usage {}, &t("nav.usage"), rsx! { ChartBar { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Approvals {}, &t("nav.approvals"), rsx! { ShieldCheck { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Nodes {}, &t("nav.nodes"), rsx! { Network { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Skills {}, &t("nav.skills"), rsx! { Star { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Tts {}, &t("nav.tts"), rsx! { Volume2 { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Voice {}, &t("nav.voice"), rsx! { Mic { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::ConnectProvider {}, &t("nav.connect_provider"), rsx! { PlugZap { size: 16 } }) }
                    { more_sheet_link(&current_route, more_open, Route::Debug {}, &t("nav.debug"), rsx! { Bug { size: 16 } }) }
                }
            }

            // Exec Approval Modal overlay (TASK-009: auto-popup on WS event OR
            // always show when there are pending approvals from polling)
            if show_approval_modal() && !pending_approvals().is_empty() {
                ExecApprovalModal {
                    approvals: pending_approvals(),
                    on_dismiss: move |_| {
                        show_approval_modal.set(false);
                        approval_tick += 1;
                    },
                }
            }
        }
    }
}

fn nav_group_header(
    label: &str,
    is_expanded: bool,
    mut onclick: impl FnMut(MouseEvent) + 'static,
) -> Element {
    let label_owned = label.to_string();
    rsx! {
        button {
            class: "nav-group-header",
            aria_expanded: "{is_expanded}",
            aria_label: "Toggle {label_owned} navigation group",
            onclick: move |e| onclick(e),
            span { class: "nav-group-chevron", aria_hidden: "true",
                if is_expanded {
                    ChevronDown { size: 14 }
                } else {
                    ChevronRight { size: 14 }
                }
            }
            "{label_owned}"
        }
    }
}

fn route_matches_section(current: &Route, target: &Route) -> bool {
    if current == target {
        return true;
    }
    match target {
        Route::Agents {} => matches!(current, Route::AgentsNew {} | Route::AgentsDetail { .. }),
        Route::Channels {} => matches!(
            current,
            Route::ChannelsAdd {} | Route::ChannelsEdit { .. } | Route::ChannelsHealth { .. }
        ),
        Route::Cron {} => matches!(current, Route::CronNew {} | Route::CronDetail { .. }),
        Route::Nodes {} => matches!(current, Route::NodesDetail { .. }),
        _ => false,
    }
}

fn nav_link(current: &Route, target: Route, label: &str, icon: Element) -> Element {
    let is_active = route_matches_section(current, &target);
    let class = if is_active {
        "nav-link active"
    } else {
        "nav-link"
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            aria_current: if is_active { "page" } else { "" },
            span { class: "nav-icon", aria_hidden: "true", {icon} }
            "{label}"
        }
    }
}

fn nav_link_with_badge(
    current: &Route,
    target: Route,
    label: &str,
    icon: Element,
    badge_count: usize,
) -> Element {
    let is_active = *current == target;
    let class = if is_active {
        "nav-link active"
    } else {
        "nav-link"
    };

    let aria = if badge_count > 0 {
        format!("{label} ({badge_count} pending)")
    } else {
        label.to_string()
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            aria_label: "{aria}",
            aria_current: if is_active { "page" } else { "" },
            span { class: "nav-icon", aria_hidden: "true", {icon} }
            "{label}"
            if badge_count > 0 {
                span {
                    class: "nav-badge",
                    aria_label: "{badge_count} pending items",
                    "{badge_count}"
                }
            }
        }
    }
}

/// An icon-only link for the collapsed sidebar rail.
fn rail_icon_link(current: &Route, target: Route, label: &str, icon: Element) -> Element {
    let is_active = route_matches_section(current, &target);
    let class = if is_active {
        "sidebar-rail__item active"
    } else {
        "sidebar-rail__item"
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            title: "{label}",
            aria_label: "{label}",
            {icon}
        }
    }
}

/// An icon-only link with badge for the collapsed sidebar rail.
fn rail_icon_link_with_badge(
    current: &Route,
    target: Route,
    label: &str,
    icon: Element,
    badge_count: usize,
) -> Element {
    let is_active = route_matches_section(current, &target);
    let class = if is_active {
        "sidebar-rail__item active"
    } else {
        "sidebar-rail__item"
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            title: "{label}",
            aria_label: "{label}",
            {icon}
            if badge_count > 0 {
                span {
                    class: "sidebar-rail__badge",
                    "{badge_count}"
                }
            }
        }
    }
}

/// A navigation link inside the "More" bottom sheet.
fn more_sheet_link(
    current: &Route,
    mut more_open: Signal<bool>,
    target: Route,
    label: &str,
    icon: Element,
) -> Element {
    let is_active = route_matches_section(current, &target);
    let class = if is_active {
        "more-sheet__link active"
    } else {
        "more-sheet__link"
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            span { class: "more-sheet__link-icon", {icon} }
            "{label}"
        }
    }
}
