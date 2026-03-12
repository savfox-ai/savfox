use std::collections::HashSet;

use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{SkillDetail, SkillsBinsResponse, SkillsStatusResponse};
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::collapsible_group::CollapsibleGroup;
use crate::components::search_input::SearchInput;
use crate::components::skeleton::*;
use crate::components::toggle_switch::ToggleSwitch;

const COLLAPSED_STORAGE_KEY: &str = "savfox_skills_collapsed";

#[component]
pub fn Skills() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search_query = use_signal(String::new);

    // T081: Collapse state persisted to localStorage
    let mut collapsed_groups: Signal<HashSet<String>> = use_signal(HashSet::new);

    // Load collapsed state from localStorage on mount
    use_effect(move || {
        let doc = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten();
        if let Some(storage) = doc {
            if let Ok(Some(raw)) = storage.get_item(COLLAPSED_STORAGE_KEY) {
                if let Ok(names) = serde_json::from_str::<Vec<String>>(&raw) {
                    collapsed_groups.set(names.into_iter().collect());
                }
            }
        }
    });

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
        let ws = ws_bins.clone();
        async move {
            ws.call::<SkillsBinsResponse>("skills.bins", None)
                .await
                .map(|r| {
                    // Try to deserialize as SkillDetail for richer data
                    serde_json::from_value::<Vec<SkillDetail>>(
                        serde_json::to_value(&r.bins).unwrap_or_default(),
                    )
                    .unwrap_or_else(|_| {
                        r.bins
                            .into_iter()
                            .map(|b| SkillDetail {
                                name: b.name,
                                version: b.version,
                                installed: b.installed,
                                category: b.category,
                                eligible: b.eligible,
                                missing_deps: b.missing_deps,
                                primary_env: b.primary_env,
                                env_set: b.env_set,
                                enabled: b.enabled.or(b.installed),
                                description: b.description,
                                disabled_reason: b.disabled_reason,
                                allowlist_blocked: b.allowlist_blocked,
                            })
                            .collect()
                    })
                })
                .unwrap_or_default()
        }
    });

    let status_read = status_data.read();
    let status = status_read.as_ref().and_then(|s| s.as_ref());
    let all_skills: Vec<SkillDetail> = bins_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = bins_data.read().is_none();

    // Filter by search
    let query = search_query().to_lowercase();
    let filtered: Vec<SkillDetail> = if query.is_empty() {
        all_skills.clone()
    } else {
        all_skills
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&query)
                    || s.description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                    || s.category
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect()
    };

    // Group by category
    let workspace: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| s.category.as_deref() == Some("workspace"))
        .collect();
    let builtin: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| s.category.as_deref() == Some("built-in"))
        .collect();
    let installed_group: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| {
            s.category.as_deref() == Some("installed")
                || (s.installed == Some(true) && s.category.is_none())
        })
        .collect();
    let extra: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| s.category.as_deref() == Some("extra"))
        .collect();
    let other: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| {
            !matches!(
                s.category.as_deref(),
                Some("workspace") | Some("built-in") | Some("installed") | Some("extra")
            ) && s.installed != Some(true)
        })
        .collect();

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
    let match_count = if query.is_empty() {
        None
    } else {
        Some(filtered.len())
    };

    rsx! {
        div { style: "padding:24px;max-width:960px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;flex-wrap:wrap;gap:12px;",
                h2 { style: "font-size:20px;font-weight:600;", "Skills" }
                div { style: "display:flex;gap:8px;align-items:center;",
                    SearchInput {
                        value: search_query(),
                        on_change: move |v: String| search_query.set(v),
                        placeholder: "Filter skills...".to_string(),
                        match_count: match_count,
                    }
                    button {
                        onclick: move |_| refresh_tick += 1,
                        class: "{ACTION_BTN}",
                        "Refresh"
                    }
                }
            }

            // Summary cards
            div { style: "display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:12px;margin-bottom:24px;",
                { stat_card("Installed", &installed_count.to_string()) }
                { stat_card("Available", &available_count.to_string()) }
                { stat_card("Enabled", &enabled_count.to_string()) }
                { stat_card("Blocked", &blocked_count.to_string()) }
            }

            if is_loading {
                SkeletonLines { count: 4 }
            } else if filtered.is_empty() {
                p { style: "color:var(--text-muted);font-size:14px;", "No skills match your search" }
            } else {
                // Collapsible groups with localStorage-persisted state
                {
                    let groups: Vec<(&str, &Vec<&SkillDetail>, bool)> = vec![
                        ("Workspace", &workspace, true),
                        ("Built-in", &builtin, true),
                        ("Installed", &installed_group, true),
                        ("Extra", &extra, false),
                        ("Other", &other, false),
                    ];
                    rsx! {
                        for (title, skills, default_open) in groups.into_iter() {
                            if !skills.is_empty() {
                                {
                                    let title_str = title.to_string();
                                    let collapsed = collapsed_groups();
                                    let is_open = if collapsed.is_empty() {
                                        default_open
                                    } else {
                                        !collapsed.contains(title)
                                    };
                                    let title_for_toggle = title_str.clone();
                                    rsx! {
                                        CollapsibleGroup {
                                            title: title_str,
                                            count: skills.len(),
                                            is_open: is_open,
                                            on_toggle: move |open: bool| {
                                                let mut current = collapsed_groups();
                                                if open {
                                                    current.remove(title_for_toggle.as_str());
                                                } else {
                                                    current.insert(title_for_toggle.clone());
                                                }
                                                // Persist to localStorage
                                                let names: Vec<&str> = current.iter().map(|s| s.as_str()).collect();
                                                if let Some(storage) = web_sys::window()
                                                    .and_then(|w| w.local_storage().ok())
                                                    .flatten()
                                                {
                                                    let _ = storage.set_item(
                                                        COLLAPSED_STORAGE_KEY,
                                                        &serde_json::to_string(&names).unwrap_or_default(),
                                                    );
                                                }
                                                collapsed_groups.set(current);
                                            },
                                            { render_skill_group(skills, ws.clone(), refresh_tick) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_skill_group(
    skills: &[&SkillDetail],
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
) -> Element {
    rsx! {
        div { style: "display:flex;flex-direction:column;gap:8px;",
            for skill in skills.iter() {
                { render_skill_row(skill, ws.clone(), refresh_tick) }
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

const ACTION_BTN: &str = "action-btn";
const INSTALL_BTN: &str = "install-btn";
