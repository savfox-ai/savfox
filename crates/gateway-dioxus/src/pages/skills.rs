use std::collections::BTreeMap;

use base64::Engine;
use dioxus::prelude::*;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{SkillDetail, SkillsBinsResponse, SkillsStatusResponse};
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::pagination::Pagination;
use crate::components::search_input::SearchInput;
use crate::components::skeleton::*;
use crate::components::toggle_switch::ToggleSwitch;

const CATEGORIES: &[(&str, &str)] = &[
    ("", "All"),
    ("workspace", "Workspace"),
    ("built-in", "Built-in"),
    ("installed", "Installed"),
    ("extra", "Extra"),
];

#[component]
pub fn Skills() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search_query = use_signal(String::new);
    let mut active_category = use_signal(String::new);
    let mut current_page = use_signal(|| 1usize);

    // Debounce: when search changes, reset to page 1
    let mut last_search = use_signal(String::new);
    let query = search_query();
    if query != last_search() {
        last_search.set(query.clone());
        current_page.set(1);
    }

    let ws_status = ws.clone();
    let status_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_status.clone();
        async move {
            ws.call::<SkillsStatusResponse>("skills.status", None)
                .await
                .ok()
        }
    });

    let ws_bins = ws.clone();
    let bins_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let page = current_page();
        let category = active_category();
        let search = search_query();
        let ws = ws_bins.clone();
        async move {
            let mut params = json!({ "page": page, "page_size": 50 });
            if !category.is_empty() {
                params["category"] = json!(category);
            }
            if !search.is_empty() {
                params["search"] = json!(search);
            }
            ws.call::<SkillsBinsResponse>("skills.bins", Some(params))
                .await
                .ok()
        }
    });

    let status_read = status_data.read();
    let status = status_read.as_ref().and_then(|s| s.as_ref());
    let bins_read = bins_data.read();
    let bins_resp = bins_read.as_ref().and_then(|r| r.as_ref());
    let is_loading = bins_data.read().is_none();

    let all_skills: Vec<SkillDetail> = bins_resp
        .map(|r| {
            serde_json::from_value::<Vec<SkillDetail>>(
                serde_json::to_value(&r.bins).unwrap_or_default(),
            )
            .unwrap_or_else(|_| {
                r.bins
                    .iter()
                    .map(|b| SkillDetail {
                        name: b.name.clone(),
                        version: b.version.clone(),
                        installed: b.installed,
                        category: b.category.clone(),
                        eligible: b.eligible,
                        missing_deps: b.missing_deps.clone(),
                        primary_env: b.primary_env.clone(),
                        env_set: b.env_set,
                        enabled: b.enabled.or(b.installed),
                        description: b.description.clone(),
                        disabled_reason: b.disabled_reason.clone(),
                        allowlist_blocked: b.allowlist_blocked,
                        flock: b.flock.clone(),
                    })
                    .collect()
            })
        })
        .unwrap_or_default();

    let total_pages = bins_resp
        .and_then(|r| r.total_pages)
        .unwrap_or(1) as usize;
    let total = bins_resp.and_then(|r| r.total).unwrap_or(0);

    // Summary stats
    let installed_count = status.and_then(|s| s.installed_count).unwrap_or_else(|| {
        all_skills
            .iter()
            .filter(|s| s.installed == Some(true))
            .count() as u32
    });
    let available_count = status
        .and_then(|s| s.available_count)
        .unwrap_or(all_skills.len() as u32);
    let enabled_count = all_skills
        .iter()
        .filter(|s| s.enabled == Some(true))
        .count();
    let blocked_count = all_skills
        .iter()
        .filter(|s| s.eligible == Some(false))
        .count();
    let match_count = if search_query().is_empty() {
        None
    } else {
        Some(total as usize)
    };

    let mut install_url = use_signal(String::new);
    let mut install_status = use_signal(|| Option::<(bool, String)>::None);
    let mut installing = use_signal(|| false);
    let ws_install = ws.clone();

    rsx! {
        div { class: "page-content skills-page",
            // Toolbar: Title, Search, Install URL, Install btn, Upload ZIP, Refresh
            div { class: "skills-toolbar",
                h2 { style: "font-size:20px;font-weight:600;white-space:nowrap;margin:0;", "Skills" }
                SearchInput {
                    value: search_query(),
                    on_change: move |v: String| search_query.set(v),
                    placeholder: "Search...".to_string(),
                    match_count: match_count,
                }
                input {
                    class: "skill-url-install__input",
                    r#type: "text",
                    placeholder: "Git URL...",
                    value: "{install_url}",
                    oninput: move |e: Event<FormData>| {
                        install_url.set(e.value());
                        install_status.set(None);
                    },
                    onkeydown: {
                        let ws = ws_install.clone();
                        move |e: Event<KeyboardData>| {
                            if e.key() == Key::Enter && !install_url().trim().is_empty() && !installing() {
                                let ws = ws.clone();
                                let url = install_url().trim().to_string();
                                installing.set(true);
                                install_status.set(None);
                                spawn(async move {
                                    let result = ws.call::<serde_json::Value>(
                                        "skills.install_url",
                                        Some(json!({ "url": url })),
                                    ).await;
                                    installing.set(false);
                                    match result {
                                        Ok(v) => {
                                            install_status.set(Some((true, format_install_result(&v))));
                                            install_url.set(String::new());
                                            refresh_tick += 1;
                                        }
                                        Err(e) => {
                                            install_status.set(Some((false, format!("{e}"))));
                                        }
                                    }
                                });
                            }
                        }
                    },
                    spellcheck: false,
                    autocomplete: "off",
                }
                button {
                    class: "skill-url-install__btn",
                    disabled: install_url().trim().is_empty() || installing(),
                    onclick: {
                        let ws = ws_install.clone();
                        move |_| {
                            let ws = ws.clone();
                            let url = install_url().trim().to_string();
                            if url.is_empty() { return; }
                            installing.set(true);
                            install_status.set(None);
                            spawn(async move {
                                let result = ws.call::<serde_json::Value>(
                                    "skills.install_url",
                                    Some(json!({ "url": url })),
                                ).await;
                                installing.set(false);
                                match result {
                                    Ok(v) => {
                                        install_status.set(Some((true, format_install_result(&v))));
                                        install_url.set(String::new());
                                        refresh_tick += 1;
                                    }
                                    Err(e) => {
                                        install_status.set(Some((false, format!("{e}"))));
                                    }
                                }
                            });
                        }
                    },
                    if installing() { "Installing..." } else { "Install" }
                }
                {
                    let ws_zip = ws.clone();
                    rsx! {
                        UploadZipButton {
                            ws: ws_zip,
                            installing: installing,
                            install_status: install_status,
                            on_installed: move |_| refresh_tick += 1,
                        }
                    }
                }
                button {
                    onclick: move |_| refresh_tick += 1,
                    class: "{ACTION_BTN}",
                    "Refresh"
                }
            }
            if let Some((ok, ref msg)) = install_status() {
                span {
                    class: if ok { "skill-url-install__msg skill-url-install__msg--ok" } else { "skill-url-install__msg skill-url-install__msg--err" },
                    "{msg}"
                }
            }

            // Summary cards
            div { class: "skills-stats-grid",
                { stat_card("Installed", &installed_count.to_string()) }
                { stat_card("Available", &available_count.to_string()) }
                { stat_card("Enabled", &enabled_count.to_string()) }
                { stat_card("Blocked", &blocked_count.to_string()) }
            }

            // Category filter chips
            div { class: "skills-category-filters",
                for (value, label) in CATEGORIES.iter() {
                    {
                        let is_active = active_category() == *value;
                        let cls = if is_active {
                            "skills-category-chip skills-category-chip--active"
                        } else {
                            "skills-category-chip"
                        };
                        let val = value.to_string();
                        rsx! {
                            button {
                                key: "{value}",
                                class: "{cls}",
                                onclick: move |_| {
                                    active_category.set(val.clone());
                                    current_page.set(1);
                                },
                                "{label}"
                            }
                        }
                    }
                }
            }

            if is_loading {
                SkeletonLines { count: 4 }
            } else if all_skills.is_empty() {
                p { style: "color:var(--text-muted);font-size:14px;", "No skills match your search" }
            } else {
                { render_skill_list(&all_skills, ws.clone(), refresh_tick) }

                // Pagination controls
                Pagination {
                    current_page: current_page(),
                    total_pages: total_pages,
                    on_page: move |page: usize| current_page.set(page),
                }
            }
        }
    }
}

fn render_skill_list(
    skills: &[SkillDetail],
    ws: WsRpc,
    refresh_tick: Signal<u32>,
) -> Element {
    // Separate skills by flock for sub-grouping.
    let mut no_flock: Vec<&SkillDetail> = Vec::new();
    let mut by_flock: BTreeMap<String, Vec<&SkillDetail>> = BTreeMap::new();
    for skill in skills {
        if let Some(ref flock) = skill.flock {
            by_flock.entry(flock.clone()).or_default().push(skill);
        } else {
            no_flock.push(skill);
        }
    }

    rsx! {
        div { style: "display:flex;flex-direction:column;gap:8px;",
            // Render un-flocked skills first.
            for skill in no_flock.iter() {
                { render_skill_row(skill, ws.clone(), refresh_tick) }
            }
            // Render each flock as a sub-group with a label.
            for (flock_name, flock_skills) in by_flock.iter() {
                div {
                    style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                    div {
                        style: "padding:6px 12px;background:var(--bg-secondary);font-size:12px;font-weight:600;color:var(--text-muted);display:flex;align-items:center;gap:6px;",
                        span { "Flock({flock_name})" }
                        Chip { label: format!("{}", flock_skills.len()), variant: ChipVariant::Info }
                    }
                    div { style: "display:flex;flex-direction:column;gap:4px;padding:4px;",
                        for skill in flock_skills.iter() {
                            { render_skill_row(skill, ws.clone(), refresh_tick) }
                        }
                    }
                }
            }
        }
    }
}

fn render_skill_row(skill: &SkillDetail, ws: WsRpc, mut refresh_tick: Signal<u32>) -> Element {
    let is_enabled = skill.enabled.unwrap_or(false);
    let is_installed = skill.installed.unwrap_or(false);
    let is_eligible = skill.eligible.unwrap_or(true);
    let allowlist_blocked = skill.allowlist_blocked.unwrap_or(false);
    let name = skill.name.clone();
    let name_toggle = skill.name.clone();
    let ws_toggle = ws.clone();
    let ws_action = ws.clone();

    rsx! {
        div {
            key: "{skill.name}",
            style: "padding:12px 16px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                div { style: "flex:1;min-width:0;",
                    div { style: "display:flex;align-items:center;gap:8px;flex-wrap:wrap;",
                        span { style: "font-weight:500;font-size:14px;", "{skill.name}" }
                        if let Some(ref ver) = skill.version {
                            span { style: "font-size:11px;color:var(--text-muted);", "v{ver}" }
                        }
                        // Category label
                        if let Some(ref cat) = skill.category {
                            Chip { label: cat.clone(), variant: ChipVariant::Info }
                        }
                        if is_eligible {
                            Chip { label: "Eligible".to_string(), variant: ChipVariant::Success }
                        } else {
                            Chip { label: "Blocked".to_string(), variant: ChipVariant::Danger }
                        }
                        if allowlist_blocked {
                            Chip { label: "Allowlist Blocked".to_string(), variant: ChipVariant::Warning }
                        }
                        if is_installed {
                            if is_enabled {
                                Chip { label: "Enabled".to_string(), variant: ChipVariant::Success }
                            } else {
                                Chip { label: "Disabled".to_string(), variant: ChipVariant::Muted }
                            }
                        } else {
                            Chip { label: "Not Installed".to_string(), variant: ChipVariant::Warning }
                        }
                    }
                    if let Some(ref desc) = skill.description {
                        p { style: "font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4;", "{desc}" }
                    }
                    if let Some(ref reason) = skill.disabled_reason {
                        if !reason.is_empty() {
                            p { style: "font-size:11px;color:var(--danger);margin-top:4px;line-height:1.4;", "Disabled reason: {reason}" }
                        }
                    }
                    // Missing deps
                    if let Some(ref deps) = skill.missing_deps {
                        if !deps.is_empty() {
                            div { style: "display:flex;gap:4px;flex-wrap:wrap;margin-top:6px;",
                                span { style: "font-size:11px;color:var(--danger);", "Missing:" }
                                for dep in deps.iter() {
                                    span {
                                        class: "skill-dep-badge",
                                        "{dep}"
                                    }
                                }
                            }
                        }
                    }
                    // Primary env key input
                    if let Some(ref env_key) = skill.primary_env {
                        {
                            let env_key_val = env_key.clone();
                            let is_set = skill.env_set.unwrap_or(false);
                            let ws_env = ws.clone();
                            rsx! {
                                SkillApiKeyInput {
                                    env_key: env_key_val,
                                    is_set: is_set,
                                    ws: ws_env,
                                    on_saved: move |_| refresh_tick += 1,
                                }
                            }
                        }
                    }
                }
                div { style: "display:flex;gap:8px;align-items:center;margin-left:12px;flex-shrink:0;",
                    if is_installed {
                        // Toggle enable/disable via .disabled marker file
                        ToggleSwitch {
                            label: String::new(),
                            checked: is_enabled,
                            on_toggle: move |enabled: bool| {
                                let ws = ws_toggle.clone();
                                let n = name_toggle.clone();
                                spawn(async move {
                                    let params = serde_json::json!({
                                        "name": n,
                                        "enabled": enabled,
                                    });
                                    let _ = ws.call::<serde_json::Value>("skills.update", Some(params)).await;
                                    refresh_tick += 1;
                                });
                            },
                        }
                    } else {
                        button {
                            onclick: {
                                let name = name.clone();
                                move |_| {
                                    let ws = ws_action.clone();
                                    let n = name.clone();
                                    spawn(async move {
                                        let params = serde_json::json!({ "name": n });
                                        let _ = ws.call::<serde_json::Value>("skills.registry.install", Some(params)).await;
                                        refresh_tick += 1;
                                    });
                                }
                            },
                            class: "{INSTALL_BTN}",
                            "Install"
                        }
                    }
                }
            }
        }
    }
}

fn stat_card(label: &str, value: &str) -> Element {
    rsx! {
        div { class: "stat-card",
            div { class: "stat-card__label", "{label}" }
            div { class: "stat-card__value", "{value}" }
        }
    }
}

/// Inline API key input for skills with `primary_env` requirement.
#[component]
fn SkillApiKeyInput(
    env_key: String,
    is_set: bool,
    ws: WsRpc,
    on_saved: EventHandler<()>,
) -> Element {
    let mut key_value = use_signal(String::new);
    let mut save_status = use_signal(|| Option::<&str>::None);
    let mut saving = use_signal(|| false);
    let mut show_key = use_signal(|| false);

    rsx! {
        div { style: "margin-top:6px;display:flex;align-items:center;gap:6px;flex-wrap:wrap;",
            label { style: "font-size:11px;color:var(--text-muted);white-space:nowrap;", "{env_key}:" }
            if is_set {
                span { class: "skill-env-badge--set", "set" }
            } else {
                span { class: "skill-env-badge--missing", "missing" }
            }
            input {
                r#type: if show_key() { "text" } else { "password" },
                placeholder: "Enter API key...",
                value: "{key_value}",
                oninput: move |e| {
                    key_value.set(e.value());
                    save_status.set(None);
                },
                class: "skill-api-key-input",
            }
            button {
                onclick: move |_| show_key.toggle(),
                class: "skill-toggle-btn",
                if show_key() { "Hide" } else { "Show" }
            }
            button {
                disabled: key_value().trim().is_empty() || saving(),
                onclick: {
                    let ws = ws.clone();
                    let env = env_key.clone();
                    move |_| {
                        let ws = ws.clone();
                        let env = env.clone();
                        let val = key_value().trim().to_string();
                        if val.is_empty() { return; }
                        saving.set(true);
                        save_status.set(None);
                        spawn(async move {
                            let result = ws.call::<serde_json::Value>(
                                "skills.setEnv",
                                Some(json!({ "key": env, "value": val })),
                            ).await;
                            saving.set(false);
                            match result {
                                Ok(_) => {
                                    save_status.set(Some("saved"));
                                    key_value.set(String::new());
                                    on_saved.call(());
                                }
                                Err(_) => {
                                    save_status.set(Some("error"));
                                }
                            }
                        });
                    }
                },
                class: "skill-save-btn",
                if saving() { "Saving..." } else { "Save" }
            }
            match save_status() {
                Some("saved") => rsx! {
                    span { style: "font-size:11px;color:var(--success);", "Saved" }
                },
                Some("error") => rsx! {
                    span { style: "font-size:11px;color:var(--danger);", "Error" }
                },
                _ => rsx! {},
            }
        }
    }
}

/// Upload a .zip file to install a skill via `skills.install_zip`.
#[component]
fn UploadZipButton(
    ws: WsRpc,
    mut installing: Signal<bool>,
    mut install_status: Signal<Option<(bool, String)>>,
    on_installed: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            class: "skill-url-install__btn skill-url-install__btn--upload",
            disabled: installing(),
            onclick: move |_| {
                let ws = ws.clone();
                let Some(window) = web_sys::window() else { return };
                let Some(doc) = window.document() else { return };
                let Ok(input) = doc.create_element("input") else { return };
                let _ = input.set_attribute("type", "file");
                let _ = input.set_attribute("accept", ".zip,application/zip");
                let _ = input.set_attribute("style", "display:none");

                let cb = Closure::wrap(Box::new(move |event: web_sys::Event| {
                    let Some(target) = event.target() else { return };
                    let Ok(input): Result<web_sys::HtmlInputElement, _> = target.dyn_into() else { return };
                    let Some(files) = input.files() else { return };
                    let Some(file) = files.get(0) else { return };
                    let Ok(reader) = web_sys::FileReader::new() else { return };

                    installing.set(true);
                    install_status.set(None);

                    let reader_clone = reader.clone();
                    let ws_inner = ws.clone();
                    let onload = Closure::wrap(Box::new(move |_: web_sys::Event| {
                        let Ok(result) = reader_clone.result() else {
                            installing.set(false);
                            install_status.set(Some((false, "Failed to read file".into())));
                            return;
                        };
                        let buf = js_sys::Uint8Array::new(&result);
                        let mut bytes = vec![0u8; buf.length() as usize];
                        buf.copy_to(&mut bytes);
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

                        let ws_spawn = ws_inner.clone();
                        spawn(async move {
                            let result = ws_spawn.call::<serde_json::Value>(
                                "skills.install_zip",
                                Some(json!({ "data": b64 })),
                            ).await;
                            installing.set(false);
                            match result {
                                Ok(v) => {
                                    let installed: Vec<String> = v.get("installed")
                                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                                        .unwrap_or_default();
                                    let auto_disabled = v.get("auto_disabled")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let msg = if installed.is_empty() {
                                        "No skills installed".into()
                                    } else {
                                        let base = format!("Installed: {}", installed.join(", "));
                                        if auto_disabled {
                                            format!("{base} (auto-disabled: >10 new skills, enable individually)")
                                        } else {
                                            base
                                        }
                                    };
                                    install_status.set(Some((true, msg)));
                                    on_installed.call(());
                                }
                                Err(e) => {
                                    install_status.set(Some((false, format!("{e}"))));
                                }
                            }
                        });
                    }) as Box<dyn FnMut(web_sys::Event)>);

                    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                    onload.forget();
                    let _ = reader.read_as_array_buffer(&file);

                    if let Some(parent) = input.parent_node() {
                        let _ = parent.remove_child(&input);
                    }
                }) as Box<dyn FnMut(web_sys::Event)>);

                let _ = input.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
                cb.forget();

                if let Some(body) = doc.body() {
                    let _ = body.append_child(&input);
                    if let Some(el) = input.dyn_ref::<web_sys::HtmlElement>() {
                        el.click();
                    }
                }
            },
            "Upload ZIP"
        }
    }
}

/// Build a human-readable message from a `skills.install_url` response.
fn format_install_result(v: &serde_json::Value) -> String {
    let flock = v
        .get("flock")
        .and_then(|f| f.as_str())
        .unwrap_or("skill");
    let count = v
        .get("new_skills_count")
        .and_then(|c| c.as_u64())
        .unwrap_or(0);
    let auto_disabled = v
        .get("auto_disabled")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let mut msg = if count > 0 {
        format!("Flock({flock}): {count} skill(s) installed")
    } else {
        format!("{flock}: installed")
    };
    if auto_disabled {
        msg.push_str(" (auto-disabled: >10 new skills, enable individually)");
    }
    msg
}

const ACTION_BTN: &str = "action-btn";
const INSTALL_BTN: &str = "install-btn";
