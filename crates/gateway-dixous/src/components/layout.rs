use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::ExecApprovalFull;
use crate::api::ws::WsRpc;
use crate::components::command_palette::CommandPalette;
use crate::components::exec_approval_modal::ExecApprovalModal;
use crate::components::toast::{ToastContainer, Toaster};
use crate::route::Route;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NavGroup {
    Main,
    Manage,
    Media,
    System,
}

impl NavGroup {
    fn label(&self) -> &'static str {
        match self {
            NavGroup::Main => "Main",
            NavGroup::Manage => "Manage",
            NavGroup::Media => "Media",
            NavGroup::System => "System",
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
        Route::Agents {} | Route::Memory {} | Route::Models {} | Route::Channels {} => {
            NavGroup::Manage
        }
        Route::Tts {} | Route::Voice {} => NavGroup::Media,
        Route::ConnectProvider {} => NavGroup::Manage,
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
    let ws_connected = use_signal(|| false);
    let ws = use_signal(WsRpc::new);
    let mut sidebar_open = use_signal(|| true);

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

    use_context_provider(|| ws.read().clone());
    use_context_provider(|| ws_connected);
    use_context_provider(|| sidebar_open);
    use_context_provider(Toaster::new);

    let ws_approvals = ws.read().clone();

    use_effect(move || {
        let ws = ws.read();
        ws.connect(ws_connected);
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
    let tab_agents_active = current_route == Route::Agents {};

    // "More" is highlighted if current route is not one of the primary tabs
    let tab_more_active = !tab_overview_active && !tab_sessions_active && !tab_agents_active;

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

    let health_label = if ws_connected() {
        "Health OK"
    } else {
        "Offline"
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
            "Skip to main content"
        }
        div { class: "app-layout",
            // ── Persistent top header ──
            header { class: "top-header",
                div { class: "top-header__left",
                    button {
                        class: "{hamburger_class}",
                        aria_label: if sidebar_open() { "Close navigation menu" } else { "Open navigation menu" },
                        aria_expanded: "{sidebar_open}",
                        onclick: move |_| {
                            let current = sidebar_open();
                            sidebar_open.set(!current);
                        },
                        span { class: "hamburger-line", aria_hidden: "true" }
                        span { class: "hamburger-line", aria_hidden: "true" }
                        span { class: "hamburger-line", aria_hidden: "true" }
                    }
                    div { class: "top-header__brand",
                        img {
                            src: "assets/savfox.svg",
                            alt: "Savfox Logo",
                            style: "width: 24px; height: 24px; object-fit: contain;",
                        }
                        span { class: "top-header__title", style: "color: var(--accent);", "SAVFOX" }
                        span { class: "top-header__subtitle", "AI ASSISTANT" }
                    }
                }
                div { class: "top-header__right",
                    // Health badge
                    div {
                        class: "{health_class}",
                        role: "status",
                        aria_live: "polite",
                        span { class: "top-header__health-dot", aria_hidden: "true" }
                        "{health_label}"
                    }
                    // Theme button group: System | Light | Dark
                    div { class: "top-header__theme-group", role: "radiogroup", aria_label: "Theme",
                        button {
                            class: if theme() == "system" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                            title: "Follow system theme",
                            aria_label: "Follow system theme",
                            onclick: move |_| theme.set("system".to_string()),
                            // Monitor icon
                            svg {
                                width: "16", height: "16", view_box: "0 0 24 24",
                                fill: "none", stroke: "currentColor", stroke_width: "2",
                                stroke_linecap: "round", stroke_linejoin: "round",
                                rect { x: "2", y: "3", width: "20", height: "14", rx: "2", ry: "2" }
                                line { x1: "8", y1: "21", x2: "16", y2: "21" }
                                line { x1: "12", y1: "17", x2: "12", y2: "21" }
                            }
                        }
                        button {
                            class: if theme() == "light" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                            title: "Light theme",
                            aria_label: "Light theme",
                            onclick: move |_| theme.set("light".to_string()),
                            // Sun icon
                            svg {
                                width: "16", height: "16", view_box: "0 0 24 24",
                                fill: "none", stroke: "currentColor", stroke_width: "2",
                                stroke_linecap: "round", stroke_linejoin: "round",
                                circle { cx: "12", cy: "12", r: "5" }
                                line { x1: "12", y1: "1", x2: "12", y2: "3" }
                                line { x1: "12", y1: "21", x2: "12", y2: "23" }
                                line { x1: "4.22", y1: "4.22", x2: "5.64", y2: "5.64" }
                                line { x1: "18.36", y1: "18.36", x2: "19.78", y2: "19.78" }
                                line { x1: "1", y1: "12", x2: "3", y2: "12" }
                                line { x1: "21", y1: "12", x2: "23", y2: "12" }
                                line { x1: "4.22", y1: "19.78", x2: "5.64", y2: "18.36" }
                                line { x1: "18.36", y1: "5.64", x2: "19.78", y2: "4.22" }
                            }
                        }
                        button {
                            class: if theme() == "dark" { "top-header__theme-btn active" } else { "top-header__theme-btn" },
                            title: "Dark theme",
                            aria_label: "Dark theme",
                            onclick: move |_| theme.set("dark".to_string()),
                            // Moon icon
                            svg {
                                width: "16", height: "16", view_box: "0 0 24 24",
                                fill: "none", stroke: "currentColor", stroke_width: "2",
                                stroke_linecap: "round", stroke_linejoin: "round",
                                path { d: "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" }
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
                    aria_label: "Main navigation",
                    role: "navigation",

                    // Nav links
                    div { class: "sidebar-nav",
                    // Main group
                    { nav_group_header("Main", expanded_main(), move |_| expanded_main.toggle()) }
                    if expanded_main() {
                        { nav_link(&current_route,Route::Overview {}, "Overview", "\u{2302}") }
                        { nav_link(&current_route,Route::Sessions {}, "Sessions", "\u{00BB}") }
                    }

                    // Manage group
                    { nav_group_header("Manage", expanded_manage(), move |_| expanded_manage.toggle()) }
                    if expanded_manage() {
                        { nav_link(&current_route,Route::Agents {}, "Agents", "&") }
                        { nav_link(&current_route,Route::Memory {}, "Memory", "*") }
                        { nav_link(&current_route,Route::Models {}, "Models", "M") }
                        { nav_link(&current_route,Route::Channels {}, "Channels", "\u{25CE}") }
                    }

                    // Media group
                    { nav_group_header("Media", expanded_media(), move |_| expanded_media.toggle()) }
                    if expanded_media() {
                        { nav_link(&current_route,Route::Tts {}, "TTS", "\u{25B6}") }
                        { nav_link(&current_route,Route::Voice {}, "Voice", "\u{266A}") }
                    }

                    // System group
                    { nav_group_header("System", expanded_system(), move |_| expanded_system.toggle()) }
                    if expanded_system() {
                        { nav_link(&current_route,Route::Config {}, "Config", "\u{2699}") }
                        { nav_link(&current_route,Route::Cron {}, "Cron", "\u{23F1}") }
                        { nav_link(&current_route,Route::Instances {}, "Instances", "\u{25CF}") }
                        { nav_link(&current_route,Route::Logs {}, "Logs", "\u{2630}") }
                        { nav_link(&current_route,Route::Usage {}, "Usage", "$") }
                        { nav_link_with_badge(&current_route,Route::Approvals {}, "Approvals", "!", approvals_count) }
                        { nav_link(&current_route,Route::Nodes {}, "Nodes", "\u{25CB}") }
                        { nav_link(&current_route,Route::Skills {}, "Skills", "\u{2605}") }
                        { nav_link(&current_route,Route::Debug {}, "Debug", "?") }
                    }
                }

                // Footer with shortcut hint
                div { class: "sidebar-footer",
                    button {
                        class: "sidebar-palette-btn",
                        onclick: move |_| palette_open.set(true),
                        span { class: "sidebar-palette-label", "Command Palette" }
                        span { class: "sidebar-palette-shortcut", "Ctrl+K" }
                    }
                }
            }

                // Main content
                main {
                    id: "main-content",
                    class: "main-content",
                    role: "main",
                    Outlet::<Route> {}
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
                            span { class: "bottom-tab-bar__icon", "\u{2302}" }
                            span { class: "bottom-tab-bar__label", "Overview" }
                        }
                    }
                    // Tab: Session
                    li {
                        Link {
                            to: Route::Sessions {},
                            class: if tab_sessions_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", "\u{00BB}" }
                            span { class: "bottom-tab-bar__label", "Session" }
                        }
                    }
                    // Tab: Agents
                    li {
                        Link {
                            to: Route::Agents {},
                            class: if tab_agents_active { "bottom-tab-bar__item active" } else { "bottom-tab-bar__item" },
                            span { class: "bottom-tab-bar__icon", "&" }
                            span { class: "bottom-tab-bar__label", "Agents" }
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
                            span { class: "bottom-tab-bar__icon", "\u{2026}" }
                            span { class: "bottom-tab-bar__label", "More" }
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
                div { class: "more-sheet__title", "Navigation" }
                div { class: "more-sheet__nav",
                    { more_sheet_link(&current_route, more_open, Route::Memory {}, "Memory", "*") }
                    { more_sheet_link(&current_route, more_open, Route::Models {}, "Models", "M") }
                    { more_sheet_link(&current_route, more_open, Route::Channels {}, "Channels", "\u{25CE}") }
                    { more_sheet_link(&current_route, more_open, Route::Config {}, "Config", "\u{2699}") }
                    { more_sheet_link(&current_route, more_open, Route::Cron {}, "Cron", "\u{23F1}") }
                    { more_sheet_link(&current_route, more_open, Route::Instances {}, "Instances", "\u{25CF}") }
                    { more_sheet_link(&current_route, more_open, Route::Logs {}, "Logs", "\u{2630}") }
                    { more_sheet_link(&current_route, more_open, Route::Usage {}, "Usage", "$") }
                    { more_sheet_link(&current_route, more_open, Route::Approvals {}, "Approvals", "!") }
                    { more_sheet_link(&current_route, more_open, Route::Nodes {}, "Nodes", "\u{25CB}") }
                    { more_sheet_link(&current_route, more_open, Route::Skills {}, "Skills", "\u{2605}") }
                    { more_sheet_link(&current_route, more_open, Route::Tts {}, "TTS", "\u{25B6}") }
                    { more_sheet_link(&current_route, more_open, Route::Voice {}, "Voice", "\u{266A}") }
                    { more_sheet_link(&current_route, more_open, Route::ConnectProvider {}, "Connect Provider", "\u{26A1}") }
                    { more_sheet_link(&current_route, more_open, Route::Debug {}, "Debug", "?") }
                }
            }

            // Exec Approval Modal overlay
            if !pending_approvals().is_empty() {
                ExecApprovalModal {
                    approvals: pending_approvals(),
                    on_dismiss: move |_| {
                        approval_tick += 1;
                    },
                }
            }
        }
    }
}

fn nav_group_header<F: FnMut(MouseEvent) + 'static>(
    label: &'static str,
    is_expanded: bool,
    mut onclick: F,
) -> Element {
    let chevron = if is_expanded { "\u{25BE}" } else { "\u{25B8}" };
    rsx! {
        button {
            class: "nav-group-header",
            aria_expanded: "{is_expanded}",
            aria_label: "Toggle {label} navigation group",
            onclick: move |e| onclick(e),
            span { class: "nav-group-chevron", aria_hidden: "true", "{chevron}" }
            "{label}"
        }
    }
}

fn nav_link(current: &Route, target: Route, label: &str, icon: &str) -> Element {
    let is_active = *current == target;
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
            span { class: "nav-icon", aria_hidden: "true", "{icon}" }
            "{label}"
        }
    }
}

fn nav_link_with_badge(
    current: &Route,
    target: Route,
    label: &str,
    icon: &str,
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
            span { class: "nav-icon", aria_hidden: "true", "{icon}" }
            "{label}"
            if badge_count > 0 {
                span {
                    style: "margin-left:auto;background:var(--danger);color:#fff;font-size:10px;padding:1px 6px;border-radius:10px;font-weight:600;",
                    aria_label: "{badge_count} pending items",
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
    icon: &str,
) -> Element {
    let is_active = *current == target;
    let class = if is_active {
        "more-sheet__link active"
    } else {
        "more-sheet__link"
    };

    rsx! {
        Link {
            to: target,
            class: "{class}",
            span { class: "more-sheet__link-icon", "{icon}" }
            "{label}"
        }
    }
}
