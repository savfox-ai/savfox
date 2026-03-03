use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{
    AgentDetail, AgentEntry, AgentFile, AgentFilesResponse, AgentsResponse, CronJob,
    CronListResponse, ModelInfo, ModelsResponse, SkillDetail, SkillsBinsResponse,
};
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::search_input::SearchInput;
use crate::components::tab_bar::{TabBar, TabItem};
use crate::components::toast::Toaster;
use crate::components::toggle_switch::ToggleSwitch;
use crate::utils::provider_registry::{canonical_provider_id, provider_display_name};

fn agent_ref(entry: &AgentEntry) -> String {
    entry
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| entry.name.clone())
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

#[component]
pub fn Agents() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    let mut selected_agent = use_signal(|| Option::<String>::None);
    let mut show_create = use_signal(|| false);
    let mut new_id = use_signal(String::new);
    let mut new_name = use_signal(String::new);
    let mut new_provider = use_signal(String::new);
    let mut new_model = use_signal(String::new);
    let mut new_prompt = use_signal(String::new);
    let mut active_tab = use_signal(|| "overview".to_string());

    // Fetch agent list
    let ws_list = ws.clone();
    let agents_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_list.clone();
        async move {
            ws.call::<AgentsResponse>("agents.list", None)
                .await
                .map(|r| r.agents)
                .unwrap_or_default()
        }
    });

    let agents: Vec<AgentEntry> = agents_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = agents_data.read().is_none();

    let selected_entry: Option<AgentEntry> = selected_agent()
        .and_then(|selected| agents.iter().find(|a| agent_ref(a) == selected).cloned());

    let tabs = vec![
        TabItem {
            id: "overview".into(),
            label: "Overview".into(),
        },
        TabItem {
            id: "files".into(),
            label: "Files".into(),
        },
        TabItem {
            id: "tools".into(),
            label: "Tools".into(),
        },
        TabItem {
            id: "skills".into(),
            label: "Skills".into(),
        },
        TabItem {
            id: "channels".into(),
            label: "Channels".into(),
        },
        TabItem {
            id: "cron".into(),
            label: "Cron".into(),
        },
    ];

    rsx! {
        div { style: "display:flex;height:100%;",
            // ── Left sidebar: agent list (~30%) ──
            div { style: "width:30%;min-width:260px;max-width:380px;border-right:1px solid var(--border);display:flex;flex-direction:column;",
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
                    h2 { style: "font-size:16px;font-weight:600;", "Agents" }
                    div { style: "display:flex;gap:6px;",
                        button {
                            onclick: move |_| refresh_tick += 1,
                            style: "{TOOL_BTN}",
                            "Refresh"
                        }
                        button {
                            onclick: move |_| {
                                show_create.set(true);
                                selected_agent.set(None);
                                new_id.set(String::new());
                                new_name.set(String::new());
                                new_provider.set(String::new());
                                new_model.set(String::new());
                                new_prompt.set(String::new());
                            },
                            style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;",
                            "+ New"
                        }
                    }
                }

                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "Loading..." }
                    } else if agents.is_empty() {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No agents configured" }
                    } else {
                        for agent in agents.iter() {
                            {
                                let aid = agent_ref(agent);
                                let is_sel = selected_agent() == Some(aid.clone());
                                let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                                let a = agent.clone();
                                let status = agent.status.as_deref().unwrap_or("idle");
                                let sc = status_color(status);
                                rsx! {
                                    div {
                                        key: "{aid}",
                                        onclick: move |_| {
                                            selected_agent.set(Some(agent_ref(&a)));
                                            show_create.set(false);
                                            active_tab.set("overview".to_string());
                                        },
                                        style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                                            span { style: "font-weight:500;font-size:14px;", "{agent.name}" }
                                            span {
                                                style: "font-size:11px;padding:1px 6px;border-radius:4px;background:{sc};color:#fff;",
                                                "{status}"
                                            }
                                        }
                                        if let Some(ref model) = agent.model {
                                            div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;", "{model}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Right panel: detail / create (~70%) ──
            div { style: "flex:1;display:flex;flex-direction:column;min-width:0;",
                if show_create() {
                    AgentCreateForm {
                        ws: ws.clone(),
                        refresh_tick,
                        show_create,
                        new_id,
                        new_name,
                        new_provider,
                        new_model,
                        new_prompt,
                    }
                } else if let Some(ref entry) = selected_entry {
                    TabBar {
                        tabs: tabs.clone(),
                        active: active_tab(),
                        on_change: move |t: String| active_tab.set(t),
                    }
                    div { style: "flex:1;overflow:auto;",
                        match active_tab().as_str() {
                            "overview" => rsx! {
                                AgentOverviewTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                    selected_agent,
                                }
                            },
                            "files" => rsx! {
                                AgentFilesTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                }
                            },
                            "tools" => rsx! {
                                AgentToolsTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                }
                            },
                            "skills" => rsx! {
                                AgentSkillsTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                }
                            },
                            "channels" => rsx! {
                                AgentChannelsTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                }
                            },
                            "cron" => rsx! {
                                AgentCronTab {
                                    ws: ws.clone(),
                                    refresh_tick,
                                    entry: entry.clone(),
                                }
                            },
                            _ => rsx! { div { "Unknown tab" } },
                        }
                    }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;color:var(--text-muted);font-size:14px;",
                        "Select an agent or create a new one"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Create form
// ---------------------------------------------------------------------------

#[component]
fn AgentCreateForm(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut show_create: Signal<bool>,
    mut new_id: Signal<String>,
    mut new_name: Signal<String>,
    mut new_provider: Signal<String>,
    mut new_model: Signal<String>,
    mut new_prompt: Signal<String>,
) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();

    // Fetch configured models so we can derive available providers.
    let ws_models = ws.clone();
    let models_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_models.clone();
        async move {
            ws.call::<ModelsResponse>("models.list", None)
                .await
                .map(|r| r.models)
                .unwrap_or_default()
        }
    });
    let models: Vec<ModelInfo> = models_data.read().as_ref().cloned().unwrap_or_default();
    let (providers, provider_models): (
        Vec<String>,
        std::collections::BTreeMap<String, Vec<(String, String)>>,
    ) = {
        let mut set = std::collections::BTreeSet::new();
        let mut model_map =
            std::collections::BTreeMap::<String, std::collections::BTreeMap<String, String>>::new();
        for m in &models {
            let from_field = m.provider.as_deref().and_then(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(canonical_provider_id(trimmed))
                }
            });
            let from_id = m.id.split_once('/').map(|(p, _)| canonical_provider_id(p));
            if let Some(provider_id) = from_field.or(from_id)
                && !provider_id.is_empty()
            {
                set.insert(provider_id.clone());

                let full_model_id = m.id.trim();
                if !full_model_id.is_empty() {
                    let display = m
                        .name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(ToString::to_string)
                        .unwrap_or_else(|| full_model_id.to_string());

                    let label = if display == full_model_id {
                        display
                    } else {
                        format!("{display} ({full_model_id})")
                    };

                    model_map
                        .entry(provider_id)
                        .or_default()
                        .insert(full_model_id.to_string(), label);
                }
            }
        }

        let mut normalized = std::collections::BTreeMap::<String, Vec<(String, String)>>::new();
        for (provider_id, entries) in model_map {
            let mut options: Vec<(String, String)> = entries.into_iter().collect();
            options.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            normalized.insert(provider_id, options);
        }

        (set.into_iter().collect(), normalized)
    };
    let selected_provider = new_provider();
    let model_options = provider_models
        .get(selected_provider.as_str())
        .cloned()
        .unwrap_or_default();
    let model_placeholder = if selected_provider.is_empty() {
        "-- Select provider first --"
    } else {
        "-- Select model --"
    };

    rsx! {
        div { style: "padding:24px;",
            h3 { style: "font-size:18px;margin-bottom:16px;", "New Agent" }
            div { style: "display:flex;flex-direction:column;gap:12px;max-width:600px;",
                div {
                    label { style: "{LABEL}", "ID" }
                    input {
                        value: "{new_id}",
                        oninput: move |e| new_id.set(e.value()),
                        placeholder: "assistant-main",
                        style: "{INPUT}",
                    }
                }
                div {
                    label { style: "{LABEL}", "Name" }
                    input {
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                        placeholder: "my-agent",
                        style: "{INPUT}",
                    }
                }
                div {
                    label { style: "{LABEL}", "Model Provider" }
                    select {
                        value: "{new_provider}",
                        onchange: move |e| {
                            new_provider.set(e.value());
                            // Keep model in sync with selected provider options.
                            new_model.set(String::new());
                        },
                        style: "{INPUT}",
                        option { value: "", "-- Select provider --" }
                        for provider_id in providers.iter() {
                            {
                                let label = provider_display_name(provider_id);
                                rsx! {
                                    option { key: "{provider_id}", value: "{provider_id}", "{label} ({provider_id})" }
                                }
                            }
                        }
                    }
                    if providers.is_empty() {
                        p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                            "No configured providers found. Add models/providers in Models page first."
                        }
                    }
                }
                div {
                    label { style: "{LABEL}", "Model" }
                    select {
                        value: "{new_model}",
                        onchange: move |e| new_model.set(e.value()),
                        style: "{INPUT}",
                        disabled: selected_provider.is_empty(),
                        option { value: "", "{model_placeholder}" }
                        for model in model_options.iter() {
                            {
                                let model_id = model.0.as_str();
                                let model_label = model.1.as_str();
                                rsx! {
                                    option { key: "{model_id}", value: "{model_id}", "{model_label}" }
                                }
                            }
                        }
                    }
                    if !selected_provider.is_empty() && model_options.is_empty() {
                        p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                            "No models available for selected provider."
                        }
                    }
                }
                div {
                    label { style: "{LABEL}", "System Prompt" }
                    textarea {
                        value: "{new_prompt}",
                        oninput: move |e| new_prompt.set(e.value()),
                        placeholder: "You are a helpful assistant...",
                        rows: 8,
                        style: "{INPUT}resize:vertical;font-family:monospace;font-size:13px;",
                    }
                }
                div { style: "display:flex;gap:8px;",
                    button {
                        onclick: move |_| {
                            let id = new_id().trim().to_string();
                            let name = new_name().trim().to_string();
                            let provider = new_provider().trim().to_string();
                            let model_input = new_model().trim().to_string();
                            let model = if model_input.is_empty() {
                                String::new()
                            } else if model_input.contains('/') || provider.is_empty() {
                                model_input
                            } else {
                                format!("{provider}/{model_input}")
                            };
                            let prompt = new_prompt();
                            if id.is_empty() {
                                toaster.error("Agent ID is required");
                                return;
                            }
                            if name.is_empty() {
                                toaster.error("Agent name is required");
                                return;
                            }
                            let ws = ws.clone();
                            spawn(async move {
                                let mut payload = json!({
                                    "id": id,
                                    "name": name,
                                    "system_prompt": prompt,
                                });
                                if !model.is_empty() {
                                    payload["model"] = json!(model);
                                }
                                if !provider.is_empty() {
                                    payload["provider"] = json!(provider);
                                }
                                let res = ws.call::<serde_json::Value>("agents.create", Some(payload)).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Agent created");
                                        show_create.set(false);
                                        new_id.set(String::new());
                                        new_name.set(String::new());
                                        new_provider.set(String::new());
                                        new_model.set(String::new());
                                        new_prompt.set(String::new());
                                        refresh_tick += 1;
                                    }
                                    Err(e) => toaster.error(format!("Create failed: {e}")),
                                }
                            });
                        },
                        style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:8px 20px;",
                        "Create"
                    }
                    button {
                        onclick: move |_| show_create.set(false),
                        style: "{TOOL_BTN}padding:8px 20px;",
                        "Cancel"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tab 1 -- Overview
// ---------------------------------------------------------------------------

#[component]
fn AgentOverviewTab(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    entry: AgentEntry,
    mut selected_agent: Signal<Option<String>>,
) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();

    let agent_id = agent_ref(&entry);
    let id_del = agent_id.clone();
    let ws_del = ws.clone();
    let status = entry.status.as_deref().unwrap_or("idle");
    let sc = status_color(status);

    // -- Editable fields, seeded from entry --
    let initial_name = entry.name.clone();
    let mut form_name = use_signal(move || initial_name.clone());

    let initial_prompt = entry.system_prompt.clone().unwrap_or_default();
    let mut form_desc = use_signal(move || initial_prompt.clone());

    let initial_model = entry.model.clone().unwrap_or_default();
    let mut form_model = use_signal(move || initial_model.clone());

    let mut form_fallback = use_signal(String::new);
    let mut form_emoji = use_signal(String::new);
    let mut form_default = use_signal(|| false);
    let mut form_theme_color = use_signal(|| "#6366f1".to_string());
    let mut test_result = use_signal(|| Option::<(bool, String)>::None);
    let mut testing = use_signal(|| false);

    // Fetch full AgentDetail for extra fields
    let ws_detail = ws.clone();
    let detail_id = agent_id.clone();
    let detail_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_detail.clone();
        let id = detail_id.clone();
        async move {
            ws.call::<AgentDetail>("agents.get", Some(json!({ "id": id })))
                .await
                .ok()
        }
    });

    // Sync detail fields once loaded
    {
        let detail_read = detail_data.read();
        if let Some(Some(ref d)) = *detail_read {
            let fb = d
                .fallback_models
                .as_ref()
                .map(|v| v.join(", "))
                .unwrap_or_default();
            form_fallback.set(fb);
            form_emoji.set(d.emoji.clone().unwrap_or_default());
            if let Some(ref tc) = d.theme_color {
                form_theme_color.set(tc.clone());
            }
            form_default.set(d.is_default.unwrap_or(false));
        }
    }

    // Fetch available models for dropdown
    let ws_models = ws.clone();
    let models_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_models.clone();
        async move {
            ws.call::<ModelsResponse>("models.list", None)
                .await
                .map(|r| r.models)
                .unwrap_or_default()
        }
    });
    let models: Vec<ModelInfo> = models_data.read().as_ref().cloned().unwrap_or_default();

    // Dirty tracking
    let is_dirty = {
        let orig_name = entry.name.clone();
        let orig_model = entry.model.clone().unwrap_or_default();
        let orig_desc = entry.system_prompt.clone().unwrap_or_default();
        form_name() != orig_name || form_model() != orig_model || form_desc() != orig_desc
    };

    let entry_id = agent_id.clone();
    let ws_save = ws.clone();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:20px;max-width:720px;",
            // Header with status and delete
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                div { style: "display:flex;align-items:center;gap:8px;",
                    span { style: "font-weight:600;font-size:16px;", "{entry.name}" }
                    span {
                        style: "font-size:11px;padding:1px 6px;border-radius:4px;background:{sc};color:#fff;",
                        "{status}"
                    }
                }
                button {
                    onclick: move |_| {
                        let id = id_del.clone();
                        let ws = ws_del.clone();
                        spawn(async move {
                            let res = ws.call::<serde_json::Value>(
                                "agents.delete",
                                Some(json!({ "id": id })),
                            ).await;
                            match res {
                                Ok(_) => {
                                    toaster.success("Agent deleted");
                                    selected_agent.set(None);
                                    refresh_tick += 1;
                                }
                                Err(e) => toaster.error(format!("Delete failed: {e}")),
                            }
                        });
                    },
                    style: "{TOOL_BTN}color:var(--danger);border-color:var(--danger);",
                    "Delete"
                }
            }

            // Agent name
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Agent Name" }
                input {
                    value: "{form_name}",
                    oninput: move |e| form_name.set(e.value()),
                    style: "{INPUT}",
                }
            }

            // Description / system prompt
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Description / Instructions" }
                textarea {
                    value: "{form_desc}",
                    oninput: move |e| form_desc.set(e.value()),
                    placeholder: "Enter agent description / system prompt...",
                    rows: 6,
                    style: "{INPUT}resize:vertical;font-family:monospace;font-size:13px;line-height:1.5;min-height:100px;",
                }
            }

            // Primary model selector
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Primary Model" }
                if models.is_empty() {
                    input {
                        value: "{form_model}",
                        oninput: move |e| form_model.set(e.value()),
                        placeholder: "e.g. gpt-4o, claude-3-sonnet",
                        style: "{INPUT}",
                    }
                } else {
                    select {
                        value: "{form_model}",
                        onchange: move |e| form_model.set(e.value()),
                        style: "{INPUT}",
                        option { value: "", "-- Select model --" }
                        for m in models.iter() {
                            {
                                let label = m.name.as_deref().unwrap_or(&m.id);
                                rsx! {
                                    option { key: "{m.id}", value: "{m.id}", "{label}" }
                                }
                            }
                        }
                    }
                }
            }

            // Model fallback list
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Model Fallbacks" }
                input {
                    value: "{form_fallback}",
                    oninput: move |e| form_fallback.set(e.value()),
                    placeholder: "e.g. gpt-4o-mini, claude-3-haiku (comma-separated)",
                    style: "{INPUT}",
                }
                p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                    "Comma-separated list of fallback models tried in order."
                }
                // Fallback entries display
                {
                    let fallbacks: Vec<String> = form_fallback()
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !fallbacks.is_empty() {
                        rsx! {
                            div { style: "display:flex;flex-wrap:wrap;gap:6px;margin-top:8px;",
                                for (i, fb) in fallbacks.iter().enumerate() {
                                    div {
                                        key: "{fb}",
                                        style: "display:flex;align-items:center;gap:4px;padding:4px 10px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;",
                                        span { style: "color:var(--text-muted);font-size:10px;", "#{i}" }
                                        span { style: "font-weight:500;", "{fb}" }
                                    }
                                }
                            }
                        }
                    } else {
                        rsx! { div { style: "display:none;" } }
                    }
                }
            }

            // Test connectivity
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Connection Test" }
                div { style: "display:flex;gap:8px;align-items:center;",
                    button {
                        disabled: form_model().is_empty() || testing(),
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                let model_id = form_model();
                                testing.set(true);
                                test_result.set(None);
                                spawn(async move {
                                    let res = ws.call::<serde_json::Value>(
                                        "models.test",
                                        Some(json!({ "model": model_id })),
                                    ).await;
                                    testing.set(false);
                                    match res {
                                        Ok(v) => {
                                            let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
                                            let msg = v.get("message").and_then(|v| v.as_str())
                                                .unwrap_or("Connection successful").to_string();
                                            test_result.set(Some((ok, msg)));
                                        }
                                        Err(e) => {
                                            test_result.set(Some((false, format!("{e}"))));
                                        }
                                    }
                                });
                            }
                        },
                        style: "{TOOL_BTN}padding:6px 16px;",
                        if testing() { "Testing..." } else { "Test Primary Model" }
                    }
                    if let Some((ok, ref msg)) = test_result() {
                        span {
                            style: if ok { "font-size:12px;color:var(--success);" } else { "font-size:12px;color:var(--danger);" },
                            "{msg}"
                        }
                    }
                }
            }

            // Identity section
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Identity" }
                div { style: "display:flex;gap:24px;flex-wrap:wrap;",
                    // Left: identity fields
                    div { style: "flex:1;min-width:280px;display:flex;flex-direction:column;gap:12px;",
                        div { style: "display:flex;gap:12px;",
                            div { style: "flex:0 0 100px;",
                                label { style: "{LABEL}", "Emoji" }
                                input {
                                    value: "{form_emoji}",
                                    oninput: move |e| form_emoji.set(e.value()),
                                    placeholder: "e.g. robot",
                                    style: "{INPUT}",
                                }
                            }
                            div { style: "flex:1;",
                                label { style: "{LABEL}", "Default Agent" }
                                ToggleSwitch {
                                    label: "Set as default agent".to_string(),
                                    description: Some("New sessions use this agent by default".to_string()),
                                    checked: form_default(),
                                    on_toggle: move |v: bool| form_default.set(v),
                                }
                            }
                        }
                        // Theme color selector
                        div {
                            label { style: "{LABEL}", "Theme Color" }
                            div { style: "display:flex;gap:6px;flex-wrap:wrap;",
                                {THEME_COLORS.iter().map(|(color, name)| {
                                    let is_selected = form_theme_color() == *color;
                                    let border = if is_selected { "3px solid var(--text-primary)" } else { "2px solid transparent" };
                                    let c = color.to_string();
                                    rsx! {
                                        button {
                                            key: "{name}",
                                            title: "{name}",
                                            style: "width:28px;height:28px;border-radius:50%;background:{color};border:{border};cursor:pointer;padding:0;",
                                            onclick: move |_| form_theme_color.set(c.clone()),
                                        }
                                    }
                                })}
                            }
                        }
                    }
                    // Right: live preview card
                    div { style: "flex:0 0 200px;",
                        label { style: "{LABEL}", "Preview" }
                        div {
                            style: "border:1px solid var(--border);border-radius:var(--radius);padding:16px;text-align:center;background:var(--bg-tertiary);",
                            div {
                                style: "width:48px;height:48px;border-radius:50%;background:{form_theme_color};display:flex;align-items:center;justify-content:center;margin:0 auto 8px;font-size:24px;",
                                if form_emoji().is_empty() {
                                    span { style: "color:#fff;font-weight:700;font-size:18px;",
                                        "{form_name().chars().next().unwrap_or('A').to_uppercase()}"
                                    }
                                } else {
                                    span { "{form_emoji}" }
                                }
                            }
                            div { style: "font-weight:600;font-size:14px;", "{form_name}" }
                            if !form_model().is_empty() {
                                div { style: "font-size:11px;color:var(--text-muted);margin-top:2px;", "{form_model}" }
                            }
                            if form_default() {
                                div { style: "margin-top:6px;",
                                    span { style: "font-size:10px;padding:2px 8px;border-radius:10px;background:var(--accent);color:#fff;", "Default" }
                                }
                            }
                        }
                    }
                }
            }

            // Save / Reload buttons
            div { style: "display:flex;gap:8px;",
                button {
                    disabled: !is_dirty,
                    onclick: {
                        let id = entry_id.clone();
                        let ws = ws_save.clone();
                        move |_| {
                            let id = id.clone();
                            let ws = ws.clone();
                            let display_name = form_name().trim().to_string();
                            let model_val = form_model().trim().to_string();
                            let desc_val = form_desc();
                            let fallback_str = form_fallback();
                            let emoji_val = form_emoji().trim().to_string();
                            let is_default = form_default();
                            let fallback_list: Vec<String> = fallback_str
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            spawn(async move {
                                let mut params = json!({
                                    "id": id,
                                    "name": display_name,
                                    "model": model_val.clone(),
                                    "system_prompt": desc_val,
                                });
                                // New format: models.primary/fallbacks
                                let mut models_obj = json!({});
                                if !model_val.is_empty() {
                                    models_obj["primary"] = json!(model_val);
                                }
                                if !fallback_list.is_empty() {
                                    models_obj["fallbacks"] = json!(fallback_list.clone());
                                    // Keep legacy field too for backward compat
                                    params["fallback_models"] = json!(fallback_list);
                                }
                                params["models"] = models_obj;
                                if !emoji_val.is_empty() {
                                    params["emoji"] = json!(emoji_val);
                                }
                                let theme_color = form_theme_color();
                                if !theme_color.is_empty() {
                                    params["theme_color"] = json!(theme_color);
                                }
                                if is_default {
                                    params["is_default"] = json!(true);
                                }
                                let res = ws.call::<serde_json::Value>("agents.update", Some(params)).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Agent saved");
                                        refresh_tick += 1;
                                    }
                                    Err(e) => toaster.error(format!("Save failed: {e}")),
                                }
                            });
                        }
                    },
                    style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:8px 20px;",
                    "Save"
                }
                button {
                    onclick: move |_| {
                        // Reload by bumping refresh
                        refresh_tick += 1;
                    },
                    style: "{TOOL_BTN}padding:8px 20px;",
                    "Reload"
                }
                if is_dirty {
                    span { style: "font-size:12px;color:var(--accent);align-self:center;", "unsaved changes" }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tab 2 -- Files
// ---------------------------------------------------------------------------

const KNOWN_FILES: [&str; 3] = ["persona.md", "identity.json", "tool_guidance.md"];

#[component]
fn AgentFilesTab(ws: WsRpc, mut refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();
    let agent_id = agent_ref(&entry);

    let mut selected_file = use_signal(|| Option::<String>::None);
    let mut file_content = use_signal(String::new);
    let mut file_dirty = use_signal(|| false);
    let mut original_content = use_signal(String::new);
    let mut new_file_name = use_signal(String::new);

    // Fetch file list
    let ws_files = ws.clone();
    let agent_name = agent_id.clone();
    let files_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_files.clone();
        let name = agent_name.clone();
        async move {
            ws.call::<AgentFilesResponse>("agents.files.list", Some(json!({ "agent_id": name })))
                .await
                .map(|r| r.files)
                .unwrap_or_default()
        }
    });

    let files: Vec<AgentFile> = files_data.read().as_ref().cloned().unwrap_or_default();

    // Build file list: show known files, mark missing ones
    let file_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
    let all_files: Vec<(String, bool, Option<u64>)> = KNOWN_FILES
        .iter()
        .map(|&name| {
            let exists = file_names.contains(&name.to_string());
            let size = files.iter().find(|f| f.name == name).and_then(|f| f.size);
            (name.to_string(), exists, size)
        })
        .chain(
            files
                .iter()
                .filter(|f| !KNOWN_FILES.contains(&f.name.as_str()))
                .map(|f| (f.name.clone(), true, f.size)),
        )
        .collect();

    rsx! {
        div { style: "display:flex;height:100%;",
            // File list sidebar
            div { style: "width:220px;min-width:180px;border-right:1px solid var(--border);overflow:auto;display:flex;flex-direction:column;",
                div { style: "padding:10px 12px;border-bottom:1px solid var(--border);",
                    span { style: "font-size:12px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;", "Agent Files" }
                    div { style: "display:flex;gap:6px;margin-top:8px;",
                        input {
                            value: "{new_file_name}",
                            placeholder: "new file...",
                            oninput: move |e| new_file_name.set(e.value()),
                            style: "flex:1;padding:4px 8px;font-size:11px;background:var(--bg-primary);border:1px solid var(--border);border-radius:4px;color:var(--text-primary);font-family:monospace;",
                        }
                        button {
                            disabled: new_file_name().trim().is_empty(),
                            onclick: {
                                let ws = ws.clone();
                                let agent = agent_id.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let agent = agent.clone();
                                    let name = new_file_name().trim().to_string();
                                    if name.is_empty() {
                                        return;
                                    }
                                    spawn(async move {
                                        let res = ws.call::<serde_json::Value>(
                                            "agents.files.set",
                                            Some(json!({ "agent_id": agent, "path": name, "content": "" })),
                                        ).await;
                                        match res {
                                            Ok(_) => {
                                                toaster.success("File created");
                                                new_file_name.set(String::new());
                                                refresh_tick += 1;
                                            }
                                            Err(e) => toaster.error(format!("Create failed: {e}")),
                                        }
                                    });
                                }
                            },
                            style: "{TOOL_BTN}font-size:11px;padding:2px 8px;",
                            "Add"
                        }
                    }
                }
                for (fname, exists, size) in all_files.iter() {
                    {
                        let is_sel = selected_file() == Some(fname.clone());
                        let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                        let fname_click = fname.clone();
                        let fname_create = fname.clone();
                        let exists = *exists;
                        let file_size = *size;
                        let ws_get = ws.clone();
                        let agent_get = agent_id.clone();
                        let ws_create = ws.clone();
                        let agent_create = agent_id.clone();
                        rsx! {
                            div {
                                key: "{fname}",
                                style: "padding:8px 12px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};display:flex;justify-content:space-between;align-items:center;",
                                if exists {
                                    div {
                                        onclick: move |_| {
                                            let ws = ws_get.clone();
                                            let agent = agent_get.clone();
                                            let name = fname_click.clone();
                                            selected_file.set(Some(name.clone()));
                                            file_dirty.set(false);
                                            // Fetch file content
                                            spawn(async move {
                                                let result = ws.call::<AgentFile>(
                                                    "agents.files.get",
                                                    Some(json!({ "agent_id": agent, "path": name })),
                                                ).await;
                                                let content = result
                                                    .ok()
                                                    .and_then(|f| f.content)
                                                    .unwrap_or_default();
                                                file_content.set(content.clone());
                                                original_content.set(content);
                                            });
                                        },
                                        style: "flex:1;",
                                        span { style: "font-size:13px;font-family:monospace;", "{fname}" }
                                        if let Some(sz) = file_size {
                                            span { style: "font-size:10px;color:var(--text-muted);margin-left:6px;", "{format_file_size(sz)}" }
                                        }
                                    }
                                } else {
                                    div { style: "flex:1;",
                                        span { style: "font-size:13px;font-family:monospace;color:var(--text-muted);", "{fname}" }
                                        span { style: "font-size:11px;color:var(--text-muted);margin-left:6px;", "(not created)" }
                                    }
                                    button {
                                        onclick: move |_| {
                                            let ws = ws_create.clone();
                                            let agent = agent_create.clone();
                                            let name = fname_create.clone();
                                            spawn(async move {
                                                let res = ws.call::<serde_json::Value>(
                                                    "agents.files.set",
                                                    Some(json!({ "agent_id": agent, "path": name, "content": "" })),
                                                ).await;
                                                match res {
                                                    Ok(_) => {
                                                        toaster.success(format!("Created {name}"));
                                                        refresh_tick += 1;
                                                    }
                                                    Err(e) => toaster.error(format!("Create failed: {e}")),
                                                }
                                            });
                                        },
                                        style: "{TOOL_BTN}font-size:11px;padding:2px 8px;",
                                        "Create"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // File editor
            div { style: "flex:1;display:flex;flex-direction:column;padding:12px;min-width:0;",
                if selected_file().is_some() {
                    div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;",
                        span { style: "font-size:13px;font-weight:600;font-family:monospace;color:var(--text-secondary);",
                            "{selected_file().unwrap_or_default()}"
                        }
                        if file_dirty() {
                            span { style: "font-size:11px;color:var(--accent);", "modified" }
                        }
                    }
                    textarea {
                        value: "{file_content}",
                        oninput: move |e| {
                            file_content.set(e.value());
                            file_dirty.set(e.value() != original_content());
                        },
                        style: "flex:1;{INPUT}resize:none;font-family:monospace;font-size:13px;line-height:1.5;",
                    }
                    div { style: "display:flex;gap:8px;margin-top:8px;",
                        button {
                            disabled: !file_dirty(),
                            onclick: {
                                let ws = ws.clone();
                                let agent = agent_id.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let agent = agent.clone();
                                    let file_name = selected_file().unwrap_or_default();
                                    let content = file_content();
                                    spawn(async move {
                                        let res = ws.call::<serde_json::Value>(
                                            "agents.files.set",
                                            Some(json!({ "agent_id": agent, "path": file_name, "content": content })),
                                        ).await;
                                        match res {
                                            Ok(_) => {
                                                toaster.success("File saved");
                                                original_content.set(file_content());
                                                file_dirty.set(false);
                                                refresh_tick += 1;
                                            }
                                            Err(e) => toaster.error(format!("Save failed: {e}")),
                                        }
                                    });
                                }
                            },
                            style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:6px 16px;",
                            "Save"
                        }
                        button {
                            disabled: !file_dirty(),
                            onclick: move |_| {
                                file_content.set(original_content());
                                file_dirty.set(false);
                            },
                            style: "{TOOL_BTN}padding:6px 16px;",
                            "Reset"
                        }
                        button {
                            onclick: {
                                let ws = ws.clone();
                                let agent = agent_id.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let agent = agent.clone();
                                    let file_name = selected_file().unwrap_or_default();
                                    if file_name.is_empty() {
                                        return;
                                    }
                                    spawn(async move {
                                        let res = ws.call::<serde_json::Value>(
                                            "agents.files.delete",
                                            Some(json!({ "agent_id": agent, "path": file_name })),
                                        ).await;
                                        match res {
                                            Ok(_) => {
                                                toaster.success("File deleted");
                                                selected_file.set(None);
                                                file_content.set(String::new());
                                                original_content.set(String::new());
                                                file_dirty.set(false);
                                                refresh_tick += 1;
                                            }
                                            Err(e) => toaster.error(format!("Delete failed: {e}")),
                                        }
                                    });
                                }
                            },
                            style: "{TOOL_BTN}padding:6px 16px;color:var(--danger);border-color:var(--danger);",
                            "Delete"
                        }
                    }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;color:var(--text-muted);font-size:14px;",
                        "Select a file to edit"
                    }
                }
            }
        }
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Tab 3 -- Tools
// ---------------------------------------------------------------------------

struct ToolCategory {
    name: &'static str,
    tools: Vec<(&'static str, &'static str)>,
}

fn tool_categories() -> Vec<ToolCategory> {
    vec![
        ToolCategory {
            name: "Files",
            tools: vec![
                ("file_read", "Read files from disk"),
                ("file_write", "Write/create files"),
            ],
        },
        ToolCategory {
            name: "Runtime",
            tools: vec![
                ("shell", "Execute shell commands"),
                ("code_search", "Search code in workspace"),
            ],
        },
        ToolCategory {
            name: "Web",
            tools: vec![
                ("browser", "Web browsing"),
                ("http_request", "Make HTTP requests"),
            ],
        },
        ToolCategory {
            name: "Memory",
            tools: vec![("md_memory", "Markdown memory operations")],
        },
        ToolCategory {
            name: "Media",
            tools: vec![
                ("image_gen", "Generate images"),
                ("calculator", "Mathematical calculations"),
            ],
        },
        ToolCategory {
            name: "Other",
            tools: vec![("git", "Git operations")],
        },
    ]
}

#[component]
fn AgentToolsTab(ws: WsRpc, mut refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();
    let agent_id = agent_ref(&entry);

    let profiles = ["minimal", "coding", "messaging", "full", "inherit"];
    let mut tools_profile = use_signal(|| "coding".to_string());
    let mut tool_states: Signal<std::collections::HashMap<String, bool>> =
        use_signal(|| std::collections::HashMap::new());

    // Fetch current tools config
    let ws_get = ws.clone();
    let agent_name = agent_id.clone();
    let tools_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_get.clone();
        let name = agent_name.clone();
        async move {
            ws.call::<AgentDetail>("agents.get", Some(json!({ "id": name })))
                .await
                .ok()
        }
    });

    // Sync from loaded data
    {
        let read = tools_data.read();
        if let Some(Some(ref detail)) = *read {
            if let Some(ref profile) = detail.tools_profile {
                tools_profile.set(profile.clone());
            }
            if let Some(ref enabled_tools) = detail.tools {
                let mut map = std::collections::HashMap::new();
                for cat in tool_categories() {
                    for (tool_id, _) in &cat.tools {
                        map.insert(
                            tool_id.to_string(),
                            enabled_tools.contains(&tool_id.to_string()),
                        );
                    }
                }
                tool_states.set(map);
            }
        }
    }

    let categories = tool_categories();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            // Profile selector (segmented control)
            div { style: "{SECTION_CARD}",
                h4 { style: "{SECTION_TITLE}", "Tools Profile" }
                div { class: "cfg-segmented",
                    for profile in profiles.iter() {
                        {
                            let p = profile.to_string();
                            let is_active = tools_profile() == p;
                            let class = if is_active { "cfg-segmented__btn active" } else { "cfg-segmented__btn" };
                            rsx! {
                                button {
                                    key: "{profile}",
                                    class: "{class}",
                                    onclick: move |_| tools_profile.set(p.clone()),
                                    "{profile}"
                                }
                            }
                        }
                    }
                }
            }

            // Per-tool toggles grouped by category
            for cat in categories.iter() {
                div { style: "{SECTION_CARD}",
                    h4 { style: "{SECTION_TITLE}", "{cat.name}" }
                    div { style: "display:flex;flex-direction:column;gap:4px;",
                        for (tool_id, tool_desc) in cat.tools.iter() {
                            {
                                let id = tool_id.to_string();
                                let id_toggle = tool_id.to_string();
                                let enabled = tool_states
                                    .read()
                                    .get(&id)
                                    .copied()
                                    .unwrap_or(false);
                                rsx! {
                                    ToggleSwitch {
                                        key: "{tool_id}",
                                        label: tool_id.to_string(),
                                        description: Some(tool_desc.to_string()),
                                        checked: enabled,
                                        on_toggle: move |v: bool| {
                                            tool_states.write().insert(id_toggle.clone(), v);
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Save button
            div { style: "display:flex;gap:8px;",
                button {
                    onclick: {
                        let ws = ws.clone();
                        let id = agent_id.clone();
                        move |_| {
                            let ws = ws.clone();
                            let id = id.clone();
                            let profile = tools_profile();
                            let enabled: Vec<String> = tool_states
                                .read()
                                .iter()
                                .filter_map(
                                    |(k, v)| {
                                        if *v { Some(k.clone()) } else { None }
                                    },
                                )
                                .collect();
                            spawn(async move {
                                let res = ws.call::<serde_json::Value>(
                                    "agents.tools.set",
                                    Some(json!({
                                        "agent": id,
                                        "profile": profile,
                                        "tools": enabled,
                                    })),
                                ).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Tools config saved");
                                        refresh_tick += 1;
                                    }
                                    Err(e) => toaster.error(format!("Save failed: {e}")),
                                }
                            });
                        }
                    },
                    style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:8px 20px;",
                    "Save Tools Config"
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tab 4 -- Skills
// ---------------------------------------------------------------------------

#[component]
fn AgentSkillsTab(ws: WsRpc, refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut search_query = use_signal(String::new);
    let agent_id = agent_ref(&entry);

    // Fetch all available skills
    let ws_bins = ws.clone();
    let skills_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_bins.clone();
        async move {
            ws.call::<SkillsBinsResponse>("skills.bins", None)
                .await
                .map(|r| {
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
                                category: None,
                                eligible: None,
                                missing_deps: None,
                                primary_env: None,
                                env_set: None,
                                enabled: b.installed,
                                description: None,
                                disabled_reason: None,
                                allowlist_blocked: None,
                            })
                            .collect()
                    })
                })
                .unwrap_or_default()
        }
    });

    // Fetch agent-specific skill config
    let ws_agent_skills = ws.clone();
    let agent_name = agent_id.clone();
    let agent_skills_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_agent_skills.clone();
        let name = agent_name.clone();
        async move {
            ws.call::<serde_json::Value>("agents.skills.get", Some(json!({ "agent": name })))
                .await
                .ok()
                .and_then(|v| v.get("skills").cloned())
                .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
                .unwrap_or_default()
        }
    });

    let all_skills: Vec<SkillDetail> = skills_data.read().as_ref().cloned().unwrap_or_default();
    let agent_enabled_skills: Vec<String> = agent_skills_data
        .read()
        .as_ref()
        .cloned()
        .unwrap_or_default();

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

    // Group by source
    let workspace: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| s.category.as_deref() == Some("workspace"))
        .collect();
    let builtin: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| s.category.as_deref() == Some("built-in"))
        .collect();
    let third_party: Vec<&SkillDetail> = filtered
        .iter()
        .filter(|s| !matches!(s.category.as_deref(), Some("workspace") | Some("built-in")))
        .collect();

    let match_count = if query.is_empty() {
        None
    } else {
        Some(filtered.len())
    };

    let is_loading = skills_data.read().is_none();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                h4 { style: "font-size:14px;font-weight:600;", "Agent Skills" }
                SearchInput {
                    value: search_query(),
                    on_change: move |v: String| search_query.set(v),
                    placeholder: "Filter skills...".to_string(),
                    match_count: match_count,
                }
            }

            if is_loading {
                p { style: "color:var(--text-muted);font-size:14px;", "Loading skills..." }
            } else if filtered.is_empty() {
                p { style: "color:var(--text-muted);font-size:14px;", "No skills match your search" }
            } else {
                // Workspace group
                if !workspace.is_empty() {
                    { render_skill_group_section("Workspace", &workspace, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster) }
                }
                // Built-in group
                if !builtin.is_empty() {
                    { render_skill_group_section("Built-in", &builtin, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster) }
                }
                // Third-party group
                if !third_party.is_empty() {
                    { render_skill_group_section("Third-party", &third_party, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster) }
                }
            }
        }
    }
}

fn render_skill_group_section(
    title: &str,
    skills: &[&SkillDetail],
    enabled_skills: &[String],
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    agent_id: String,
    mut toaster: Toaster,
) -> Element {
    rsx! {
        div { style: "{SECTION_CARD}",
            h4 { style: "{SECTION_TITLE}", "{title} ({skills.len()})" }
            div { style: "display:flex;flex-direction:column;gap:4px;",
                for skill in skills.iter() {
                    {
                        let is_enabled = enabled_skills.contains(&skill.name);
                        let skill_name = skill.name.clone();
                        let ws_toggle = ws.clone();
                        let agent = agent_id.clone();
                        rsx! {
                            div {
                                key: "{skill.name}",
                                style: "display:flex;justify-content:space-between;align-items:center;padding:8px 0;border-bottom:1px solid var(--border);",
                                div { style: "flex:1;min-width:0;",
                                    div { style: "display:flex;align-items:center;gap:6px;",
                                        span { style: "font-weight:500;font-size:13px;", "{skill.name}" }
                                        if let Some(ref ver) = skill.version {
                                            span { style: "font-size:11px;color:var(--text-muted);", "v{ver}" }
                                        }
                                    }
                                    if let Some(ref desc) = skill.description {
                                        p { style: "font-size:11px;color:var(--text-muted);margin-top:2px;", "{desc}" }
                                    }
                                }
                                ToggleSwitch {
                                    label: String::new(),
                                    checked: is_enabled,
                                    on_toggle: move |enabled: bool| {
                                        let ws = ws_toggle.clone();
                                        let name = skill_name.clone();
                                        let agent = agent.clone();
                                        spawn(async move {
                                            let res = ws.call::<serde_json::Value>(
                                                "agents.skills.set",
                                                Some(json!({
                                                    "agent": agent,
                                                    "skill": name,
                                                    "enabled": enabled,
                                                })),
                                            ).await;
                                            match res {
                                                Ok(_) => {
                                                    toaster.success(if enabled { "Skill enabled" } else { "Skill disabled" });
                                                    refresh_tick += 1;
                                                }
                                                Err(e) => toaster.error(format!("Failed: {e}")),
                                            }
                                        });
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tab 5 -- Channels
// ---------------------------------------------------------------------------

#[component]
fn AgentChannelsTab(ws: WsRpc, mut refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();
    let agent_id = agent_ref(&entry);

    // Fetch all channels
    let ws_channels = ws.clone();
    let channels_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_channels.clone();
        async move {
            ws.call::<serde_json::Value>("channels.status", Some(json!({ "probe": false })))
                .await
                .ok()
        }
    });

    // Fetch agent channel assignments
    let ws_agent_ch = ws.clone();
    let agent_name = agent_id.clone();
    let agent_channels_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_agent_ch.clone();
        let name = agent_name.clone();
        async move {
            ws.call::<serde_json::Value>("agents.get", Some(json!({ "id": name })))
                .await
                .ok()
                .and_then(|v| v.get("channels").cloned())
                .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
                .unwrap_or_default()
        }
    });

    let channels_read = channels_data.read();
    let channels_status = channels_read.as_ref().and_then(|c| c.as_ref());
    let agent_channels: Vec<String> = agent_channels_data
        .read()
        .as_ref()
        .cloned()
        .unwrap_or_default();

    let is_loading = channels_data.read().is_none();

    // Extract channel IDs from status
    let channel_ids: Vec<String> = channels_status
        .and_then(|s| s.get("channels"))
        .and_then(|c| c.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            h4 { style: "font-size:14px;font-weight:600;", "Channel Assignments" }
            p { style: "font-size:12px;color:var(--text-muted);margin-top:-8px;",
                "Enable channels this agent should respond on."
            }

            if is_loading {
                p { style: "color:var(--text-muted);font-size:14px;", "Loading channels..." }
            } else if channel_ids.is_empty() {
                div { style: "{SECTION_CARD}",
                    p { style: "color:var(--text-muted);font-size:14px;",
                        "No channels configured. Add channels from the Channels page first."
                    }
                }
            } else {
                div { style: "{SECTION_CARD}",
                    div { style: "display:flex;flex-direction:column;gap:4px;",
                        for ch_id in channel_ids.iter() {
                            {
                                let ch_data = channels_status
                                    .and_then(|s| s.get("channels"))
                                    .and_then(|c| c.get(ch_id.as_str()));

                                let is_connected = ch_data
                                    .and_then(|c| c.get("connected"))
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let is_configured = ch_data
                                    .and_then(|c| c.get("configured"))
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let is_running = ch_data
                                    .and_then(|c| c.get("running"))
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);

                                let (status_variant, status_text) = if is_running && is_connected {
                                    (ChipVariant::Success, "connected")
                                } else if is_configured {
                                    (ChipVariant::Warning, "configured")
                                } else {
                                    (ChipVariant::Muted, "not configured")
                                };

                                let is_assigned = agent_channels.contains(ch_id);
                                let ch_toggle = ch_id.clone();
                                let ws_toggle = ws.clone();
                                let agent_toggle = agent_id.clone();

                                rsx! {
                                    div {
                                        key: "{ch_id}",
                                        style: "display:flex;justify-content:space-between;align-items:center;padding:10px 0;border-bottom:1px solid var(--border);",
                                        div { style: "display:flex;align-items:center;gap:8px;",
                                            span { style: "font-weight:500;font-size:14px;text-transform:capitalize;", "{ch_id}" }
                                            Chip { label: status_text.to_string(), variant: status_variant }
                                        }
                                        ToggleSwitch {
                                            label: String::new(),
                                            checked: is_assigned,
                                            on_toggle: move |enabled: bool| {
                                                let ws = ws_toggle.clone();
                                                let agent = agent_toggle.clone();
                                                let channel = ch_toggle.clone();
                                                spawn(async move {
                                                    let method = if enabled {
                                                        "agents.channels.add"
                                                    } else {
                                                        "agents.channels.remove"
                                                    };
                                                    let res = ws.call::<serde_json::Value>(
                                                        method,
                                                        Some(json!({ "agent": agent, "channel": channel })),
                                                    ).await;
                                                    match res {
                                                        Ok(_) => {
                                                            toaster.success(if enabled { "Channel enabled" } else { "Channel disabled" });
                                                            refresh_tick += 1;
                                                        }
                                                        Err(e) => toaster.error(format!("Failed: {e}")),
                                                    }
                                                });
                                            },
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

// ---------------------------------------------------------------------------
// Tab 6 -- Cron
// ---------------------------------------------------------------------------

#[component]
fn AgentCronTab(ws: WsRpc, mut refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let ws_connected = use_context::<Signal<bool>>();
    let agent_id = agent_ref(&entry);
    let agent_name = entry.name.clone();

    // Fetch all cron jobs, then filter by agent_id
    let ws_cron = ws.clone();
    let cron_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_cron.clone();
        let id = agent_id.clone();
        let name = agent_name.clone();
        async move {
            // Try filtered call first; if server doesn't support it, fetch all + filter
            let all_jobs = ws
                .call::<CronListResponse>("cron.list", None)
                .await
                .map(|r| r.jobs)
                .unwrap_or_default();
            // Filter jobs that reference this agent via payload or agent_id
            all_jobs
                .into_iter()
                .filter(|job| {
                    // Check if payload contains agent reference
                    if let Some(ref payload) = job.payload {
                        if let Some(agent_id) = payload.get("agent_id").and_then(|v| v.as_str()) {
                            return agent_id == id || agent_id == name;
                        }
                        if let Some(agent_id) = payload.get("agent").and_then(|v| v.as_str()) {
                            return agent_id == id || agent_id == name;
                        }
                    }
                    // Check job name prefix convention
                    if let Some(ref jname) = job.name {
                        if jname.starts_with(&format!("{}/", id))
                            || jname.starts_with(&format!("{}:", id))
                            || jname.starts_with(&format!("{}/", name))
                            || jname.starts_with(&format!("{}:", name))
                        {
                            return true;
                        }
                    }
                    false
                })
                .collect::<Vec<CronJob>>()
        }
    });

    let jobs: Vec<CronJob> = cron_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = cron_data.read().is_none();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                h4 { style: "font-size:14px;font-weight:600;", "Cron Jobs for {entry.name}" }
                button {
                    onclick: move |_| refresh_tick += 1,
                    style: "{TOOL_BTN}",
                    "Refresh"
                }
            }

            if is_loading {
                p { style: "color:var(--text-muted);font-size:14px;", "Loading cron jobs..." }
            } else if jobs.is_empty() {
                div { style: "{SECTION_CARD}",
                    p { style: "color:var(--text-muted);font-size:14px;",
                        "No cron jobs found for this agent."
                    }
                    p { style: "color:var(--text-muted);font-size:12px;margin-top:4px;",
                        "Create cron jobs from the Cron page and assign them to this agent."
                    }
                }
            } else {
                div { style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                    table { style: "width:100%;border-collapse:collapse;",
                        thead {
                            tr { style: "background:var(--bg-tertiary);",
                                th { style: "{TH}", "Name" }
                                th { style: "{TH}", "Schedule" }
                                th { style: "{TH}", "Status" }
                                th { style: "{TH}", "Next Run" }
                            }
                        }
                        tbody {
                            for job in jobs.iter() {
                                {
                                    let enabled = job.enabled.unwrap_or(true);
                                    let status_class = if enabled { "chip chip--success" } else { "chip chip--muted" };
                                    let status_label = if enabled { "Enabled" } else { "Disabled" };
                                    let schedule = job.schedule.as_deref().unwrap_or("-");
                                    let next = job.next_run.as_deref().unwrap_or("-");
                                    let display_name = job.name.as_deref().unwrap_or(&job.id);
                                    rsx! {
                                        tr { key: "{job.id}", style: "border-top:1px solid var(--border);",
                                            td { style: "{TD}font-weight:500;", "{display_name}" }
                                            td { style: "{TD}font-family:monospace;font-size:12px;", "{schedule}" }
                                            td { style: "{TD}",
                                                span { class: "{status_class}", "{status_label}" }
                                            }
                                            td { style: "{TD}font-size:12px;color:var(--text-muted);", "{next}" }
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

// ---------------------------------------------------------------------------
// Helpers & Constants
// ---------------------------------------------------------------------------

fn status_color(status: &str) -> &'static str {
    match status {
        "running" | "active" => "#22c55e",
        "error" => "#ef4444",
        "paused" => "#f59e0b",
        _ => "#666",
    }
}

const TOOL_BTN: &str = "padding:4px 12px;background:transparent;color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;cursor:pointer;";
const INPUT: &str = "width:100%;padding:8px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;";
const LABEL: &str =
    "display:block;font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;";
const SECTION_CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:16px;";
const SECTION_TITLE: &str = "font-size:13px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;margin-bottom:8px;";
const TH: &str = "text-align:left;padding:10px 14px;font-size:12px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;";
const TD: &str = "padding:10px 14px;font-size:14px;";

const THEME_COLORS: &[(&str, &str)] = &[
    ("#6366f1", "Indigo"),
    ("#8b5cf6", "Violet"),
    ("#ec4899", "Pink"),
    ("#ef4444", "Red"),
    ("#f97316", "Orange"),
    ("#eab308", "Yellow"),
    ("#22c55e", "Green"),
    ("#06b6d4", "Cyan"),
];
