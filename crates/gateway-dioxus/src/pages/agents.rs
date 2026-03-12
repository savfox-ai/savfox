use dioxus::prelude::*;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{
    AgentDetail, AgentEntry, AgentFile, AgentFilesResponse, AgentsResponse, ModelInfo,
    ModelsResponse, SkillDetail, SkillsBinsResponse,
};
use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;
use crate::components::icon::Icon;
use crate::components::search_input::SearchInput;
use crate::components::skeleton::*;
use crate::components::tab_bar::{TabBar, TabItem};
use crate::components::toast::Toaster;
use crate::components::toggle_switch::ToggleSwitch;
use crate::utils::deep_link::replace_url;
use crate::utils::download::trigger_download;
use crate::utils::provider_registry::{canonical_provider_id, provider_display_name};

// ---------------------------------------------------------------------------
// TASK-012: Agent status indicator helper
// ---------------------------------------------------------------------------

fn agent_status_color(entry: &AgentEntry) -> &'static str {
    match entry.status.as_deref() {
        Some("active") | Some("running") | Some("online") => "#22c55e", // green
        Some("error") | Some("failed") => "#ef4444",                    // red
        Some("idle") | Some("stopped") | Some("offline") => "#9ca3af",  // gray
        Some(_) => "#9ca3af",
        None => {
            // Derive status from config: if agent has no model configured, show gray (offline)
            if entry.model.as_deref().unwrap_or("").trim().is_empty() {
                "#9ca3af"
            } else {
                "#22c55e"
            }
        }
    }
}

fn agent_status_label(entry: &AgentEntry) -> &'static str {
    match entry.status.as_deref() {
        Some("active") | Some("running") | Some("online") => "Online",
        Some("error") | Some("failed") => "Error",
        Some("idle") | Some("stopped") | Some("offline") => "Offline",
        Some(_) => "Offline",
        None => {
            if entry.model.as_deref().unwrap_or("").trim().is_empty() {
                "Offline"
            } else {
                "Online"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TASK-029: Agent templates
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AgentTemplate {
    name: &'static str,
    description: &'static str,
    model_hint: &'static str,
    system_prompt: &'static str,
}

const AGENT_TEMPLATES: &[AgentTemplate] = &[
    AgentTemplate {
        name: "General Assistant",
        description: "A balanced assistant for everyday questions, planning, and tasks.",
        model_hint: "",
        system_prompt: "You are a capable general-purpose assistant. Help users think through tasks, answer questions clearly, and provide practical next steps. Be concise by default, structured when helpful, and explicit about assumptions or uncertainty.",
    },
    AgentTemplate {
        name: "Code Assistant",
        description: "A coding assistant that helps write, review, and debug code.",
        model_hint: "claude",
        system_prompt: "You are a coding assistant. Help users write, review, debug, and explain code. Follow best practices, suggest improvements, and provide clear explanations. When writing code, include comments and handle edge cases.",
    },
    AgentTemplate {
        name: "DevOps Bot",
        description: "A DevOps and infrastructure assistant.",
        model_hint: "",
        system_prompt: "You are a DevOps and infrastructure assistant. Help users with CI/CD pipelines, container orchestration, cloud infrastructure, monitoring, and deployment strategies. Provide practical solutions using industry-standard tools like Docker, Kubernetes, Terraform, and GitHub Actions.",
    },
    AgentTemplate {
        name: "Research Assistant",
        description: "Summarizes topics, compares options, and organizes findings.",
        model_hint: "",
        system_prompt: "You are a research assistant. Break down complex topics, compare options objectively, and summarize findings in a clear, structured way. Highlight tradeoffs, identify gaps in available information, and distinguish facts from inferences.",
    },
    AgentTemplate {
        name: "Writing Coach",
        description: "Helps draft, rewrite, and improve written communication.",
        model_hint: "",
        system_prompt: "You are a writing coach. Help users draft, rewrite, and refine writing for clarity, tone, structure, and persuasion. Adapt to the requested audience and format, preserve the user's intent, and offer cleaner alternatives when wording is awkward or vague.",
    },
    AgentTemplate {
        name: "Study Tutor",
        description: "Explains concepts, creates examples, and teaches step by step.",
        model_hint: "",
        system_prompt: "You are a patient tutor. Explain concepts step by step, check for understanding, and adapt explanations to the user's level. Use examples, short exercises, and intuitive analogies when helpful. Prefer teaching over simply giving the answer.",
    },
    AgentTemplate {
        name: "Project Planner",
        description: "Turns goals into concrete plans, milestones, and action lists.",
        model_hint: "",
        system_prompt: "You are a project planning assistant. Help users turn goals into concrete plans with milestones, deliverables, sequencing, dependencies, and risks. Make plans realistic, actionable, and easy to follow. Call out blockers and suggest sensible priorities.",
    },
    AgentTemplate {
        name: "Customer Support",
        description: "A helpful customer support agent.",
        model_hint: "",
        system_prompt: "You are a helpful customer support agent. Respond to user inquiries with empathy and professionalism. Provide clear, concise answers. When you cannot resolve an issue, explain next steps. Always maintain a friendly and patient tone.",
    },
    AgentTemplate {
        name: "Document Q&A",
        description: "Answers questions based on provided documents.",
        model_hint: "",
        system_prompt: "You answer questions based on provided documents. When answering, cite relevant sections from the documents. If the answer is not found in the provided documents, clearly state that. Do not make up information that is not supported by the source material.",
    },
    AgentTemplate {
        name: "Meeting Notes",
        description: "Condenses discussions into decisions, action items, and summaries.",
        model_hint: "",
        system_prompt: "You are a meeting notes assistant. Turn messy discussion into clean summaries with key decisions, open questions, risks, and action items. Keep notes concise, organized, and easy to scan. Make ownership and next steps explicit when possible.",
    },
    AgentTemplate {
        name: "Data Analyst",
        description: "Interprets metrics, finds trends, and explains implications.",
        model_hint: "",
        system_prompt: "You are a data analyst. Help users interpret metrics, identify patterns, and explain what the numbers may imply. Be careful about causation claims, call out limitations in the data, and present findings in a structured, decision-oriented way.",
    },
    AgentTemplate {
        name: "Translator",
        description: "A professional translator for multiple languages.",
        model_hint: "",
        system_prompt: "You are a professional translator. Translate text accurately while preserving meaning, tone, and cultural nuances. When the source language is ambiguous, ask for clarification. Provide translation notes when idioms or cultural references require explanation.",
    },
];

fn agent_ref(entry: &AgentEntry) -> String {
    entry
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| entry.name.clone())
}

fn is_builtin_default_agent(entry: &AgentEntry) -> bool {
    entry
        .id
        .as_deref()
        .map(str::trim)
        .is_some_and(|id| id.eq_ignore_ascii_case("default"))
}

fn current_default_source_ref(agents: &[AgentEntry]) -> String {
    agents
        .iter()
        .find(|agent| !is_builtin_default_agent(agent) && agent.is_default.unwrap_or(false))
        .map(agent_ref)
        .unwrap_or_else(|| "default".to_string())
}

fn agent_option_label(entry: &AgentEntry) -> String {
    let name = entry.name.trim();
    let reference = agent_ref(entry);

    if reference.eq_ignore_ascii_case(name) {
        name.to_string()
    } else {
        format!("{name} ({reference})")
    }
}

fn normalized_agent_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

fn has_agent_name_conflict(
    agents: &[AgentEntry],
    candidate_name: &str,
    exclude_agent_ref: Option<&str>,
) -> bool {
    let Some(candidate_name) = normalized_agent_name(candidate_name) else {
        return false;
    };

    agents.iter().any(|agent| {
        let current_ref = agent_ref(agent);
        if exclude_agent_ref.is_some_and(|exclude| exclude == current_ref) {
            return false;
        }

        normalized_agent_name(&agent.name).is_some_and(|name| name == candidate_name)
    })
}

fn normalized_reasoning_level(level: &str) -> Option<String> {
    let trimmed = level.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = match trimmed.to_ascii_lowercase().as_str() {
        "none" | "off" => "off",
        "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        other => return Some(other.to_string()),
    };

    Some(normalized.to_string())
}

fn reasoning_level_label(level: &str) -> &'static str {
    match level {
        "off" => "Off",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "XHigh",
        _ => "Custom",
    }
}

fn normalized_reasoning_presets(model: Option<&ModelInfo>) -> Vec<(String, String)> {
    let Some(model) = model else {
        return Vec::new();
    };

    let mut presets = Vec::new();
    for preset in model
        .supported_reasoning_levels
        .as_deref()
        .unwrap_or_default()
        .iter()
    {
        let Some(effort) = normalized_reasoning_level(&preset.effort) else {
            continue;
        };
        if presets.iter().any(|(value, _)| value == &effort) {
            continue;
        }
        presets.push((effort, preset.description.clone()));
    }
    presets
}

fn model_default_reasoning_level(model: Option<&ModelInfo>) -> Option<String> {
    model.and_then(|model| {
        model
            .default_reasoning_level
            .as_deref()
            .and_then(normalized_reasoning_level)
    })
}

fn selected_model_info<'a>(models: &'a [ModelInfo], model_id: &str) -> Option<&'a ModelInfo> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return None;
    }

    models
        .iter()
        .find(|model| model.id.eq_ignore_ascii_case(trimmed))
}

fn model_option_label(model: &ModelInfo) -> String {
    model
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            model
                .model_slug
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| model.id.clone())
}

fn model_select_value(models: &[ModelInfo], model_id: &str) -> String {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    selected_model_info(models, trimmed)
        .map(|model| model.id.clone())
        .unwrap_or_else(|| trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

enum AgentDeepLink {
    None,
    New,
    Detail(String),
}

#[component]
pub fn Agents() -> Element {
    agents_inner(AgentDeepLink::None)
}

#[component]
pub fn AgentsNew() -> Element {
    agents_inner(AgentDeepLink::New)
}

#[component]
pub fn AgentsDetail(agent_id: String) -> Element {
    agents_inner(AgentDeepLink::Detail(agent_id))
}

fn agents_inner(deep_link: AgentDeepLink) -> Element {
    let is_routed = !matches!(deep_link, AgentDeepLink::None);
    let nav = use_navigator();

    let initial_selected = match &deep_link {
        AgentDeepLink::Detail(id) => Some(id.clone()),
        _ => Option::None,
    };
    let initial_create = matches!(&deep_link, AgentDeepLink::New);
    let initial_show_detail = initial_selected.is_some() || initial_create;

    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    let mut selected_agent = use_signal(move || initial_selected);
    let mut show_create = use_signal(move || initial_create);
    let mut show_settings = use_signal(|| false);
    let mut search_query = use_signal(String::new);
    let mut show_detail = use_signal(move || initial_show_detail);

    // Sync URL with current view state for deep linking
    use_effect(move || {
        let selected = selected_agent();
        let creating = show_create();

        if let Some(ref id) = selected {
            replace_url(&format!("/agents/{id}"));
        } else if creating {
            replace_url("/agents/new");
        } else if is_routed {
            nav.replace(crate::route::Route::Agents {});
        } else {
            replace_url("/agents");
        }
    });
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

    // Fetch file count for the selected agent
    let ws_files_count = ws.clone();
    let files_count_agent = selected_agent();
    let files_count_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_files_count.clone();
        let agent = files_count_agent.clone();
        async move {
            if let Some(agent_id) = agent {
                ws.call::<AgentFilesResponse>(
                    "agents.files.list",
                    Some(json!({ "agent_id": agent_id })),
                )
                .await
                .map(|r| r.files.len())
                .unwrap_or(0)
            } else {
                0
            }
        }
    });

    // Fetch tools count from agent detail (enabled tools in permission_policy)
    let ws_tools_count = ws.clone();
    let tools_count_agent = selected_agent();
    let tools_count_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_tools_count.clone();
        let agent = tools_count_agent.clone();
        async move {
            if let Some(agent_id) = agent {
                ws.call::<AgentDetail>("agents.get", Some(json!({ "id": agent_id })))
                    .await
                    .ok()
                    .and_then(|detail| {
                        detail.permission_policy.as_ref().and_then(|pp| {
                            pp.get("tool_access")
                                .and_then(|ta| ta.get("allowed"))
                                .and_then(|v| v.as_array())
                                .map(|arr| arr.len())
                        })
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        }
    });

    // Fetch skills count
    let ws_skills_count = ws.clone();
    let skills_count_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_skills_count.clone();
        async move {
            ws.call::<SkillsBinsResponse>("skills.bins", None)
                .await
                .map(|r| {
                    r.bins
                        .iter()
                        .filter(|b| b.installed.unwrap_or(false))
                        .count()
                })
                .unwrap_or(0)
        }
    });

    let files_count = files_count_data.read().as_ref().copied().unwrap_or(0);
    let tools_count = tools_count_data.read().as_ref().copied().unwrap_or(0);
    let skills_count = skills_count_data.read().as_ref().copied().unwrap_or(0);

    let tabs = vec![
        TabItem {
            id: "overview".into(),
            label: "Overview".into(),
        },
        TabItem {
            id: "files".into(),
            label: if files_count > 0 {
                format!("Files ({files_count})")
            } else {
                "Files".into()
            },
        },
        TabItem {
            id: "tools".into(),
            label: if tools_count > 0 {
                format!("Tools ({tools_count})")
            } else {
                "Tools".into()
            },
        },
        TabItem {
            id: "skills".into(),
            label: if skills_count > 0 {
                format!("Skills ({skills_count})")
            } else {
                "Skills".into()
            },
        },
    ];
    let settings_btn_class = if show_settings() {
        format!("{TOOL_BTN} tool-btn--icon active")
    } else {
        format!("{TOOL_BTN} tool-btn--icon")
    };

    let detail_class = if show_detail() {
        "split-view--detail-active"
    } else {
        ""
    };

    rsx! {
        div { class: "split-view {detail_class}", style: "max-width:1200px;width:100%;",
            // ── Left sidebar: agent list (~30%) ──
            div { class: "split-view__list",
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
                    h2 { style: "font-size:16px;font-weight:600;", "Agents" }
                    div { style: "display:flex;gap:6px;",
                        button {
                            onclick: move |_| refresh_tick += 1,
                            class: "{TOOL_BTN}",
                            "Refresh"
                        }
                        button {
                            onclick: move |_| {
                                show_settings.set(true);
                                show_create.set(false);
                                show_detail.set(true);
                                selected_agent.set(None);
                            },
                            title: "Agent settings",
                            aria_label: "Agent settings",
                            class: "{settings_btn_class}",
                            Icon {
                                name: "settings".to_string(),
                                class: None,
                            }
                        }
                        // TASK-030: Import agent from JSON file
                        button {
                            onclick: {
                                let ws = ws.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let Some(window) = web_sys::window() else { return };
                                    let Some(doc) = window.document() else { return };
                                    let Ok(input) = doc.create_element("input") else { return };
                                    let _ = input.set_attribute("type", "file");
                                    let _ = input.set_attribute("accept", ".json,application/json");
                                    let _ = input.set_attribute("style", "display:none");

                                    let cb = Closure::wrap(Box::new(move |event: web_sys::Event| {
                                        let Some(target) = event.target() else { return };
                                        let Ok(input): Result<web_sys::HtmlInputElement, _> = target.dyn_into() else { return };
                                        let Some(files) = input.files() else { return };
                                        let Some(file) = files.get(0) else { return };
                                        let Ok(reader) = web_sys::FileReader::new() else { return };

                                        let reader_clone = reader.clone();
                                        let ws_inner = ws.clone();
                                        let onload = Closure::wrap(Box::new(move |_: web_sys::Event| {
                                            let Ok(result) = reader_clone.result() else { return };
                                            let Some(text) = result.as_string() else { return };
                                            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else { return };

                                            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("imported-agent").to_string();
                                            let system_prompt = parsed.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let model = parsed.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let description = parsed.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();

                                            let id_slug: String = name.chars()
                                                .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
                                                .collect();

                                            let ws_spawn = ws_inner.clone();
                                            spawn(async move {
                                                let mut payload = json!({
                                                    "id": id_slug,
                                                    "name": name,
                                                    "system_prompt": if description.is_empty() { system_prompt.clone() } else { description },
                                                });
                                                if !model.is_empty() {
                                                    payload["model"] = json!(model);
                                                }
                                                if !system_prompt.is_empty() {
                                                    payload["system_prompt"] = json!(system_prompt);
                                                }
                                                let _ = ws_spawn.call::<serde_json::Value>("agents.create", Some(payload)).await;
                                                refresh_tick += 1;
                                            });
                                        }) as Box<dyn FnMut(web_sys::Event)>);

                                        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                                        onload.forget();
                                        let _ = reader.read_as_text(&file);

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
                                }
                            },
                            class: "{TOOL_BTN}",
                            "Import"
                        }
                        button {
                            onclick: move |_| {
                                show_create.set(true);
                                show_settings.set(false);
                                show_detail.set(true);
                                selected_agent.set(None);
                                new_id.set(String::new());
                                new_name.set(String::new());
                                new_provider.set(String::new());
                                new_model.set(String::new());
                                new_prompt.set(String::new());
                            },
                            class: "{TOOL_BTN} tool-btn--primary",
                            "+ New"
                        }
                    }
                }

                div { style: "padding:8px 12px;border-bottom:1px solid var(--border);",
                    SearchInput {
                        value: search_query(),
                        on_change: move |v: String| search_query.set(v),
                        placeholder: "Filter agents...".to_string(),
                    }
                }

                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        div { style: "padding:16px;",
                            SkeletonLines { count: 3 }
                        }
                    } else if agents.is_empty() {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No agents configured" }
                    } else {
                        {
                            let query = search_query().trim().to_ascii_lowercase();
                            let filtered: Vec<_> = agents.iter().filter(|a| {
                                query.is_empty() || a.name.to_ascii_lowercase().contains(&query)
                            }).collect();
                            rsx! {
                                if filtered.is_empty() {
                                    p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No matching agents" }
                                }
                                for agent in filtered.into_iter() {
                                    {
                                        let aid = agent_ref(agent);
                                        let is_sel = selected_agent() == Some(aid.clone());
                                        let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                                        let a = agent.clone();
                                        rsx! {
                                            div {
                                                key: "{aid}",
                                                role: "button",
                                                tabindex: "0",
                                                onclick: move |_| {
                                                    selected_agent.set(Some(agent_ref(&a)));
                                                    show_create.set(false);
                                                    show_settings.set(false);
                                                    show_detail.set(true);
                                                    active_tab.set("overview".to_string());
                                                },
                                                style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};",
                                                div { style: "display:flex;justify-content:space-between;align-items:center;",
                                                    div { style: "display:flex;align-items:center;gap:8px;min-width:0;",
                                                        // TASK-012: Status indicator dot
                                                        {
                                                            let color = agent_status_color(agent);
                                                            let label = agent_status_label(agent);
                                                            rsx! {
                                                                span {
                                                                    title: "{label}",
                                                                    style: "width:8px;height:8px;border-radius:50%;background:{color};flex-shrink:0;display:inline-block;",
                                                                }
                                                            }
                                                        }
                                                        span { style: "font-weight:500;font-size:14px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;", "{agent.name}" }
                                                        if is_builtin_default_agent(agent) {
                                                            span {
                                                                class: "agent-default-badge",
                                                                "Default"
                                                            }
                                                        }
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
                }
            }

            // ── Right panel: detail / create (~70%) ──
            div { class: "split-view__detail",
                button {
                    class: "split-view__back",
                    onclick: move |_| show_detail.set(false),
                    "\u{2190} Back"
                }
                if show_create() {
                    AgentCreateForm {
                        agents: agents.clone(),
                        ws: ws.clone(),
                        refresh_tick,
                        show_create,
                        new_id,
                        new_name,
                        new_provider,
                        new_model,
                        new_prompt,
                    }
                } else if show_settings() {
                    AgentSettingsPane {
                        agents: agents.clone(),
                        ws: ws.clone(),
                        refresh_tick,
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
                                    key: "{agent_ref(entry)}:{refresh_tick()}",
                                    agents: agents.clone(),
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
                            _ => rsx! { div { "Unknown tab" } },
                        }
                    }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;",
                        EmptyState {
                            icon: "&".to_string(),
                            message: "Select an agent or create a new one".to_string(),
                            action_label: "Create Agent".to_string(),
                            on_action: move |_| {
                                show_create.set(true);
                                show_settings.set(false);
                                show_detail.set(true);
                                selected_agent.set(None);
                                new_id.set(String::new());
                                new_name.set(String::new());
                                new_provider.set(String::new());
                                new_model.set(String::new());
                                new_prompt.set(String::new());
                            },
                        }
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
    agents: Vec<AgentEntry>,
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

    let name_conflict = has_agent_name_conflict(&agents, &new_name(), None);
    let name_input_class = if name_conflict {
        format!("{INPUT} form-input--error")
    } else {
        INPUT.to_string()
    };

    rsx! {
        div { style: "padding:24px;",
            h3 { style: "font-size:18px;margin-bottom:16px;", "New Agent" }

            // TASK-029: Template selection cards
            div { style: "margin-bottom:20px;",
                p { style: "font-size:13px;color:var(--text-muted);margin-bottom:10px;", "Start from a template or configure from scratch:" }
                div { style: "display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:8px;",
                    for tpl in AGENT_TEMPLATES.iter() {
                        {
                            let tpl_name = tpl.name;
                            let tpl_desc = tpl.description;
                            let tpl_prompt = tpl.system_prompt;
                            rsx! {
                                div {
                                    key: "{tpl_name}",
                                    role: "button",
                                    tabindex: "0",
                                    class: "agent-template-card",
                                    onclick: move |_| {
                                        let slug: String = tpl_name.chars()
                                            .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
                                            .collect();
                                        new_id.set(slug);
                                        new_name.set(tpl_name.to_string());
                                        new_prompt.set(tpl_prompt.to_string());
                                    },
                                    div { style: "font-weight:600;font-size:13px;margin-bottom:4px;", "{tpl_name}" }
                                    div { style: "font-size:11px;color:var(--text-muted);line-height:1.4;", "{tpl_desc}" }
                                }
                            }
                        }
                    }
                }
            }

            div { style: "display:flex;flex-direction:column;gap:12px;max-width:600px;",
                div {
                    label { class: "{LABEL}", "ID" }
                    input {
                        value: "{new_id}",
                        oninput: move |e| new_id.set(e.value()),
                        placeholder: "assistant-main",
                        class: "{INPUT}",
                    }
                }
                div {
                    label { class: "{LABEL}", "Name *" }
                    input {
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                        placeholder: "my-agent",
                        class: "{name_input_class}",
                    }
                    if name_conflict {
                        p { style: "font-size:11px;color:var(--danger);margin-top:4px;",
                            "An agent with this name already exists."
                        }
                    }
                }
                div {
                    label { class: "{LABEL}", "Model Provider" }
                    select {
                        value: "{new_provider}",
                        onchange: move |e| {
                            new_provider.set(e.value());
                            // Keep model in sync with selected provider options.
                            new_model.set(String::new());
                        },
                        class: "{INPUT}",
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
                    label { class: "{LABEL}", "Model" }
                    select {
                        value: "{new_model}",
                        onchange: move |e| new_model.set(e.value()),
                        class: "{INPUT}",
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
                    label { class: "{LABEL}", "System Prompt" }
                    textarea {
                        value: "{new_prompt}",
                        oninput: move |e| new_prompt.set(e.value()),
                        placeholder: "You are a helpful assistant...",
                        rows: 8,
                        class: "{INPUT}", style: "resize:vertical;font-family:var(--font-mono);font-size:13px;",
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
                            let agents_clone = agents.clone();
                            if has_agent_name_conflict(&agents_clone, &name, None) {
                                toaster.error("Agent with this name already exists");
                                return;
                            }
                            if agents_clone.iter().any(|a| agent_ref(a) == id) {
                                toaster.error("Agent with this ID already exists");
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
                        class: "{TOOL_BTN} tool-btn--primary tool-btn--lg",
                        "Create"
                    }
                    button {
                        onclick: move |_| show_create.set(false),
                        class: "{TOOL_BTN} tool-btn--lg",
                        "Cancel"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Global agent settings
// ---------------------------------------------------------------------------

#[component]
fn AgentSettingsPane(agents: Vec<AgentEntry>, ws: WsRpc, mut refresh_tick: Signal<u32>) -> Element {
    let mut toaster = use_context::<Toaster>();

    let current_default = current_default_source_ref(&agents);
    let default_entry_name = agents
        .iter()
        .find(|agent| is_builtin_default_agent(agent))
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| "Savfox Agent".to_string());
    let initial_default = current_default.clone();
    let mut selected_default = use_signal(move || initial_default.clone());

    let selection_summary = if selected_default() == "default" {
        format!("New sessions will use the system default agent entry ({default_entry_name}).")
    } else {
        agents
            .iter()
            .find(|agent| agent_ref(agent) == selected_default())
            .map(|agent| {
                format!(
                    "New sessions will start with {}.",
                    agent_option_label(agent)
                )
            })
            .unwrap_or_else(|| "Choose which agent new sessions should start with.".to_string())
    };
    let is_dirty = selected_default() != current_default;

    rsx! {
        div { style: "padding:24px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            h3 { style: "font-size:18px;", "Agent Settings" }

            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Default Agent" }
                p { style: "font-size:13px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                    "Choose the agent new sessions should use by default. This is a global setting."
                }
                div {
                    label { class: "{LABEL}", "Default Agent" }
                    select {
                        value: "{selected_default}",
                        onchange: move |e| selected_default.set(e.value()),
                        class: "{INPUT}",
                        option { value: "default", "System default agent ({default_entry_name})" }
                        for agent in agents.iter().filter(|agent| !is_builtin_default_agent(agent)) {
                            {
                                let value = agent_ref(agent);
                                let label = agent_option_label(agent);
                                rsx! {
                                    option { key: "{value}", value: "{value}", "{label}" }
                                }
                            }
                        }
                    }
                }
                p { style: "font-size:12px;color:var(--text-muted);margin-top:8px;line-height:1.5;",
                    "{selection_summary}"
                }
            }

            div { style: "display:flex;gap:8px;",
                button {
                    disabled: !is_dirty,
                    onclick: {
                        let ws = ws.clone();
                        move |_| {
                            let ws = ws.clone();
                            let target = selected_default();
                            spawn(async move {
                                let res = ws.call::<serde_json::Value>(
                                    "agents.update",
                                    Some(json!({
                                        "id": target,
                                        "is_default": true,
                                    })),
                                ).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Default agent updated");
                                        refresh_tick += 1;
                                    }
                                    Err(e) => toaster.error(format!("Save failed: {e}")),
                                }
                            });
                        }
                    },
                    class: "{TOOL_BTN} tool-btn--primary tool-btn--lg",
                    "Save"
                }
                button {
                    onclick: move |_| refresh_tick += 1,
                    class: "{TOOL_BTN} tool-btn--lg",
                    "Reload"
                }
                if is_dirty {
                    span { style: "font-size:12px;color:var(--accent);align-self:center;", "unsaved changes" }
                }
            }

            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Reset Default Agent" }
                p { style: "font-size:13px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                    "Reset the default agent configuration back to factory defaults. This clears all customizations (model, system prompt, thinking level, etc.)."
                }
                button {
                    onclick: {
                        let ws = ws.clone();
                        move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                let res = ws.call::<serde_json::Value>(
                                    "agents.reset",
                                    Some(json!({ "id": "default" })),
                                ).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Default agent reset to factory defaults");
                                        refresh_tick += 1;
                                    }
                                    Err(e) => toaster.error(format!("Reset failed: {e}")),
                                }
                            });
                        }
                    },
                    class: "{TOOL_BTN} tool-btn--lg tool-btn--warning",
                    "Reset Default Agent"
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
    agents: Vec<AgentEntry>,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    entry: AgentEntry,
    mut selected_agent: Signal<Option<String>>,
) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();

    let agent_id = agent_ref(&entry);
    let entry_is_default = agent_id.eq_ignore_ascii_case("default");
    let id_del = agent_id.clone();
    let ws_del = ws.clone();
    // -- Editable fields, seeded from entry --
    let initial_name = entry.name.clone();
    let mut form_name = use_signal(move || initial_name.clone());

    let initial_prompt = entry.system_prompt.clone().unwrap_or_default();
    let mut form_desc = use_signal(move || initial_prompt.clone());

    let initial_model = entry
        .model
        .clone()
        .or_else(|| {
            entry
                .models
                .as_ref()
                .and_then(|models| models.primary.clone())
        })
        .unwrap_or_default();
    let mut form_model = use_signal(move || initial_model.clone());
    let initial_reasoning = entry
        .thinking
        .as_deref()
        .and_then(normalized_reasoning_level)
        .unwrap_or_default();
    let mut form_reasoning = use_signal(move || initial_reasoning.clone());

    let mut form_fallback = use_signal(String::new);
    let mut form_matrix_channels: Signal<std::collections::HashSet<String>> =
        use_signal(std::collections::HashSet::new);
    let mut detail_seeded = use_signal(|| false);
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

    let detail_snapshot = detail_data
        .read()
        .as_ref()
        .and_then(|detail| detail.clone());
    if !detail_seeded() {
        if let Some(ref detail) = detail_snapshot {
            let detail_model = detail
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let fallback = detail
                .fallback_models
                .as_ref()
                .map(|items| items.join(", "))
                .unwrap_or_default();
            if form_model().trim().is_empty() {
                if let Some(detail_model) = detail_model {
                    form_model.set(detail_model);
                }
            }
            form_fallback.set(fallback);
            form_matrix_channels.set(
                detail
                    .matrix_auto_user_channels
                    .as_ref()
                    .map(|ids| ids.iter().cloned().collect())
                    .unwrap_or_default(),
            );
            form_reasoning.set(
                detail
                    .thinking
                    .as_deref()
                    .and_then(normalized_reasoning_level)
                    .unwrap_or_default(),
            );
            detail_seeded.set(true);
        }
    }

    // Fetch matrix appservice channel configs for Matrix Identity section
    let ws_matrix = ws.clone();
    let matrix_configs_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_matrix.clone();
        async move {
            ws.call::<serde_json::Value>("channels.config.list", None)
                .await
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|cfg| {
                    let kind = cfg.get("kind")?.as_str()?;
                    if !kind.eq_ignore_ascii_case("matrix") {
                        return None;
                    }
                    let inner = cfg.get("config").and_then(|c| c.as_object())?;
                    let mode = inner.get("mode").and_then(|v| v.as_str()).unwrap_or("user");
                    if !mode.eq_ignore_ascii_case("appservice") {
                        return None;
                    }
                    let id = cfg.get("id")?.as_str()?.to_string();
                    let name = cfg
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let server_name = inner
                        .get("serverName")
                        .or_else(|| inner.get("server_name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let user_prefix = inner
                        .get("userPrefix")
                        .or_else(|| inner.get("user_prefix"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "_savfox_".to_string());
                    Some((id, name, server_name, user_prefix))
                })
                .collect::<Vec<(String, String, String, String)>>()
        }
    });

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
    let selected_model = selected_model_info(&models, &form_model());
    let selected_model_value = model_select_value(&models, &form_model());
    let selected_model_missing = !selected_model_value.is_empty() && selected_model.is_none();
    let mut reasoning_presets = normalized_reasoning_presets(selected_model);
    if reasoning_presets.is_empty() && !form_reasoning().is_empty() {
        reasoning_presets.push((
            form_reasoning(),
            "Current saved override for this agent.".to_string(),
        ));
    }
    let reasoning_default = model_default_reasoning_level(selected_model);
    let reasoning_default_label = reasoning_default
        .as_deref()
        .map(reasoning_level_label)
        .map(ToString::to_string);
    let reasoning_helper = if form_reasoning().is_empty() {
        reasoning_default_label
            .as_ref()
            .map(|label| format!("Uses the model default reasoning effort ({label})."))
            .unwrap_or_else(|| "No reasoning effort override is set.".to_string())
    } else {
        let selected_value = form_reasoning();
        reasoning_presets
            .iter()
            .find(|(effort, _)| effort == &selected_value)
            .map(|(_, description)| description.clone())
            .unwrap_or_else(|| "Uses a custom reasoning effort override.".to_string())
    };
    let show_reasoning_settings = !reasoning_presets.is_empty() || !form_reasoning().is_empty();

    // Dirty tracking
    let is_dirty = {
        let orig_name = entry.name.clone();
        let orig_model = entry
            .model
            .clone()
            .or_else(|| {
                entry
                    .models
                    .as_ref()
                    .and_then(|models| models.primary.clone())
            })
            .unwrap_or_default();
        let orig_desc = entry.system_prompt.clone().unwrap_or_default();
        let current_model = model_select_value(&models, &form_model());
        let original_model = model_select_value(&models, &orig_model);
        let orig_reasoning = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.thinking.as_deref())
            .and_then(normalized_reasoning_level)
            .or_else(|| {
                entry
                    .thinking
                    .as_deref()
                    .and_then(normalized_reasoning_level)
            })
            .unwrap_or_default();
        let orig_fallback = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.fallback_models.as_ref())
            .map(|items| items.join(", "))
            .unwrap_or_default();
        let orig_matrix_channels: std::collections::HashSet<String> = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.matrix_auto_user_channels.as_ref())
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default();

        form_name() != orig_name
            || current_model != original_model
            || form_desc() != orig_desc
            || form_reasoning() != orig_reasoning
            || form_fallback() != orig_fallback
            || *form_matrix_channels.read() != orig_matrix_channels
    };

    let entry_id = agent_id.clone();
    let ws_save = ws.clone();

    // Clone button state
    let ws_clone = ws.clone();
    let clone_entry = entry.clone();
    let clone_detail = detail_snapshot.clone();
    let clone_agents = agents.clone();

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:20px;max-width:720px;",
            // Header with clone + delete
            div { style: "display:flex;justify-content:space-between;align-items:center;",
                div { style: "display:flex;align-items:center;gap:8px;",
                    span { style: "font-weight:600;font-size:16px;", "{entry.name}" }
                }
                div { style: "display:flex;gap:6px;",
                    if !entry_is_default {
                        button {
                            onclick: {
                                let ws = ws_clone.clone();
                                let entry = clone_entry.clone();
                                let detail = clone_detail.clone();
                                let agents = clone_agents.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let entry = entry.clone();
                                    let detail = detail.clone();
                                    let agents = agents.clone();
                                    spawn(async move {
                                        // Generate a unique clone name
                                        let base_name = format!("{} (Copy)", entry.name);
                                        let mut clone_name = base_name.clone();
                                        let mut suffix = 2u32;
                                        while has_agent_name_conflict(&agents, &clone_name, None) {
                                            clone_name = format!("{} (Copy {})", entry.name, suffix);
                                            suffix += 1;
                                        }

                                        // Generate a unique clone ID
                                        let base_id = agent_ref(&entry);
                                        let clone_id = format!("{}-copy-{}", base_id, uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));

                                        let model_val = entry.model.clone()
                                            .or_else(|| entry.models.as_ref().and_then(|m| m.primary.clone()))
                                            .unwrap_or_default();
                                        let prompt_val = entry.system_prompt.clone().unwrap_or_default();

                                        let mut payload = json!({
                                            "id": clone_id,
                                            "name": clone_name,
                                            "system_prompt": prompt_val,
                                        });
                                        if !model_val.is_empty() {
                                            payload["model"] = json!(model_val);
                                        }
                                        if let Some(ref thinking) = entry.thinking {
                                            payload["thinking"] = json!(thinking);
                                        }
                                        // Include fallback models from detail if available
                                        if let Some(ref detail) = detail {
                                            if let Some(ref fallbacks) = detail.fallback_models {
                                                if !fallbacks.is_empty() {
                                                    payload["models"] = json!({
                                                        "primary": model_val,
                                                        "fallbacks": fallbacks,
                                                    });
                                                }
                                            }
                                        }

                                        let clone_id_for_select = clone_id.clone();
                                        let res = ws.call::<serde_json::Value>("agents.create", Some(payload)).await;
                                        match res {
                                            Ok(_) => {
                                                toaster.success("Agent cloned");
                                                selected_agent.set(Some(clone_id_for_select));
                                                refresh_tick += 1;
                                            }
                                            Err(e) => toaster.error(format!("Clone failed: {e}")),
                                        }
                                    });
                                }
                            },
                            class: "{TOOL_BTN}",
                            "Clone"
                        }
                    }
                    // TASK-030: Export agent as JSON
                    button {
                        onclick: {
                            let export_entry = entry.clone();
                            let export_detail = detail_snapshot.clone();
                            move |_| {
                                let mut export_data = json!({
                                    "name": export_entry.name,
                                });
                                if let Some(ref model) = export_entry.model {
                                    export_data["model"] = json!(model);
                                }
                                if let Some(ref prompt) = export_entry.system_prompt {
                                    export_data["system_prompt"] = json!(prompt);
                                }
                                if let Some(ref thinking) = export_entry.thinking {
                                    export_data["thinking"] = json!(thinking);
                                }
                                if let Some(ref detail) = export_detail {
                                    if let Some(ref fallbacks) = detail.fallback_models {
                                        if !fallbacks.is_empty() {
                                            export_data["fallback_models"] = json!(fallbacks);
                                        }
                                    }
                                    if let Some(ref pp) = detail.permission_policy {
                                        if let Some(tools) = pp.get("tool_access")
                                            .and_then(|ta| ta.get("allowed"))
                                            .and_then(|v| v.as_array())
                                        {
                                            let tool_names: Vec<&str> = tools.iter()
                                                .filter_map(|v| v.as_str())
                                                .collect();
                                            export_data["tools"] = json!(tool_names);
                                        }
                                    }
                                }
                                let filename = format!("{}.json",
                                    export_entry.name.chars()
                                        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
                                        .collect::<String>()
                                );
                                let content = serde_json::to_string_pretty(&export_data).unwrap_or_default();
                                trigger_download(&filename, &content, "application/json");
                            }
                        },
                        class: "{TOOL_BTN}",
                        "Export"
                    }
                    if !entry_is_default {
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
                            class: "{TOOL_BTN} tool-btn--danger",
                            "Delete"
                        }
                    }
                }
            }

            // Agent name
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Agent Name" }
                input {
                    value: "{form_name}",
                    oninput: move |e| form_name.set(e.value()),
                    class: "{INPUT}",
                }
            }

            // Description / system prompt
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Description / Instructions" }
                textarea {
                    value: "{form_desc}",
                    oninput: move |e| form_desc.set(e.value()),
                    placeholder: "Enter agent description / system prompt...",
                    rows: 6,
                    class: "{INPUT}", style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;min-height:100px;",
                }
            }

            // Primary model selector
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Primary Model" }
                if models.is_empty() {
                    input {
                        value: "{form_model}",
                        oninput: move |e| form_model.set(e.value()),
                        placeholder: "e.g. gpt-4o, claude-3-sonnet",
                        class: "{INPUT}",
                    }
                } else {
                    select {
                        key: "{selected_model_value}:{models.len()}",
                        value: "{selected_model_value}",
                        onchange: move |e| form_model.set(e.value()),
                        class: "{INPUT}",
                        option {
                            value: "",
                            selected: selected_model_value.is_empty(),
                            "-- Select model --"
                        }
                        if selected_model_missing {
                            option {
                                value: "{selected_model_value}",
                                selected: true,
                                "{selected_model_value}"
                            }
                        }
                        for m in models.iter() {
                            {
                                let label = model_option_label(m);
                                let is_selected = selected_model_value == m.id;
                                rsx! {
                                    option {
                                        key: "{m.id}",
                                        value: "{m.id}",
                                        selected: is_selected,
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                }
                if show_reasoning_settings {
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Reasoning Effort" }
                        select {
                            value: "{form_reasoning}",
                            onchange: move |e| {
                                let next = normalized_reasoning_level(&e.value()).unwrap_or_default();
                                form_reasoning.set(next);
                            },
                            class: "{INPUT}",
                            option {
                                value: "",
                                if let Some(ref label) = reasoning_default_label {
                                    "Model default ({label})"
                                } else {
                                    "Model default"
                                }
                            }
                            for (effort, _) in reasoning_presets.iter() {
                                option {
                                    key: "{effort}",
                                    value: "{effort}",
                                    "{reasoning_level_label(effort)}"
                                }
                            }
                        }
                        p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                            "{reasoning_helper}"
                        }
                    }
                }
            }

            // Model fallback list
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Model Fallbacks" }
                input {
                    value: "{form_fallback}",
                    oninput: move |e| form_fallback.set(e.value()),
                    placeholder: "e.g. gpt-4o-mini, claude-3-haiku (comma-separated)",
                    class: "{INPUT}",
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

            // Matrix Identity (appservice auto-user)
            {
                let appservice_channels: Vec<(String, String, String, String)> = matrix_configs_data
                    .read()
                    .as_ref()
                    .cloned()
                    .unwrap_or_default();
                let agent_slug: String = agent_id
                    .chars()
                    .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '_' })
                    .collect();

                rsx! {
                    div { class: "{SECTION_CARD}",
                        h4 { class: "{SECTION_TITLE}", "Matrix Identity" }
                        p { style: "font-size:13px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                            "Enable auto-creation of a virtual Matrix user for this agent on appservice channels. The user ID is generated automatically based on the channel configuration and cannot be modified."
                        }
                        if appservice_channels.is_empty() {
                            div { style: "padding:12px 14px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-muted);font-size:13px;line-height:1.5;",
                                "No Matrix appservice channels are configured. "
                                "After adding a Matrix channel in "
                                strong { "appservice" }
                                " mode from the Channels page, available channels will appear here for selection."
                            }
                        } else {
                            div { style: "display:flex;flex-direction:column;gap:8px;",
                                for (cfg_id, cfg_name, server_name, user_prefix) in appservice_channels.iter() {
                                    {
                                        let id = cfg_id.clone();
                                        let id_toggle = cfg_id.clone();
                                        let checked = form_matrix_channels.read().contains(cfg_id);
                                        let display_name = if cfg_name.is_empty() { cfg_id.as_str() } else { cfg_name.as_str() };
                                        let generated_user_id = if !server_name.is_empty() {
                                            format!("@{}{}:{}", user_prefix, agent_slug, server_name)
                                        } else {
                                            String::new()
                                        };
                                        rsx! {
                                            div {
                                                key: "{id}",
                                                style: "padding:10px 14px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                                                ToggleSwitch {
                                                    label: display_name.to_string(),
                                                    description: Some(format!("{server_name}")),
                                                    checked: checked,
                                                    on_toggle: move |v: bool| {
                                                        let mut set = form_matrix_channels.write();
                                                        if v {
                                                            set.insert(id_toggle.clone());
                                                        } else {
                                                            set.remove(&id_toggle);
                                                        }
                                                    },
                                                }
                                                if checked && !generated_user_id.is_empty() {
                                                    div { style: "margin-top:8px;padding:8px 12px;background:var(--bg-secondary);border-radius:var(--radius);",
                                                        span { style: "font-size:11px;color:var(--text-muted);margin-right:6px;", "Matrix User:" }
                                                        span { style: "font-family:var(--font-mono);font-size:13px;font-weight:500;", "{generated_user_id}" }
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

            // Test connectivity
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Connection Test" }
                div { style: "display:flex;gap:8px;align-items:center;",
                    button {
                        disabled: selected_model_value.is_empty() || testing(),
                        onclick: {
                            let selected_model_value = selected_model_value.clone();
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                let model_id = selected_model_value.clone();
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
                        class: "{TOOL_BTN} tool-btn--md",
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
                            let name = form_name().trim().to_string();
                            if name.is_empty() {
                                toaster.error("Agent name is required");
                                return;
                            }
                            if has_agent_name_conflict(&agents, &name, Some(id.as_str())) {
                                toaster.error("Agent with this name already exists");
                                return;
                            }
                            let model_val = selected_model_value.clone();
                            let desc_val = form_desc();
                            let reasoning_val = form_reasoning();
                            let fallback_str = form_fallback();
                            let fallback_list: Vec<String> = fallback_str
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            spawn(async move {
                                let mut params = json!({
                                    "id": id,
                                    "name": name,
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
                                }
                                params["models"] = models_obj;
                                params["thinking"] = if reasoning_val.trim().is_empty() {
                                    serde_json::Value::Null
                                } else {
                                    json!(reasoning_val)
                                };
                                {
                                    let channels: Vec<String> = form_matrix_channels.read().iter().cloned().collect();
                                    if channels.is_empty() {
                                        params["matrix_auto_user_channels"] = serde_json::Value::Null;
                                    } else {
                                        params["matrix_auto_user_channels"] = json!(channels);
                                    }
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
                    class: "{TOOL_BTN} tool-btn--primary tool-btn--lg",
                    "Save"
                }
                button {
                    onclick: move |_| {
                        // Reload by bumping refresh
                        refresh_tick += 1;
                    },
                    class: "{TOOL_BTN} tool-btn--lg",
                    "Reload"
                }
                if is_dirty {
                    span { style: "font-size:12px;color:var(--accent);align-self:center;", "unsaved changes" }
                }
            }
        }

        // ── Danger Zone ──────────────────────────────────────────────
        if !entry_is_default {
            {
                let mut show_confirm = use_signal(|| false);
                let dz_ws = ws.clone();
                let dz_id = agent_id.clone();
                rsx! {
                    div { class: "danger-zone",
                        div { class: "danger-zone__title", "Danger Zone" }
                        div { class: "danger-zone__desc", "Permanently delete this agent and all its configuration. This action cannot be undone." }
                        if show_confirm() {
                            div { style: "display:flex;gap:8px;align-items:center;",
                                span { style: "font-size:13px;color:var(--danger);font-weight:500;", "Are you sure?" }
                                button {
                                    class: "danger-zone__btn",
                                    onclick: {
                                        let ws_del = dz_ws.clone();
                                        let id_del = dz_id.clone();
                                        move |_| {
                                            let ws = ws_del.clone();
                                            let id = id_del.clone();
                                            spawn(async move {
                                                match ws.call::<serde_json::Value>("agents.delete", Some(json!({ "id": id }))).await {
                                                    Ok(_) => {
                                                        toaster.success("Agent deleted");
                                                        selected_agent.set(None);
                                                        refresh_tick += 1;
                                                    }
                                                    Err(e) => toaster.error(format!("Delete failed: {e}")),
                                                }
                                            });
                                        }
                                    },
                                    "Yes, delete"
                                }
                                button {
                                    class: "cancel-btn-secondary",
                                    onclick: move |_| show_confirm.set(false),
                                    "Cancel"
                                }
                            }
                        } else {
                            button {
                                class: "danger-zone__btn",
                                onclick: move |_| show_confirm.set(true),
                                "Delete Agent"
                            }
                        }
                    }
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
                    span { style: "font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;", "Agent Files" }
                    div { style: "display:flex;gap:6px;margin-top:8px;",
                        input {
                            value: "{new_file_name}",
                            placeholder: "new file...",
                            oninput: move |e| new_file_name.set(e.value()),
                            style: "flex:1;padding:4px 8px;font-size:11px;background:var(--bg-primary);border:1px solid var(--border);border-radius:4px;color:var(--text-primary);font-family:var(--font-mono);",
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
                            class: "{TOOL_BTN} tool-btn--sm",
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
                                        span { style: "font-size:13px;font-family:var(--font-mono);", "{fname}" }
                                        if let Some(sz) = file_size {
                                            span { style: "font-size:10px;color:var(--text-muted);margin-left:6px;", "{format_file_size(sz)}" }
                                        }
                                    }
                                } else {
                                    div { style: "flex:1;",
                                        span { style: "font-size:13px;font-family:var(--font-mono);color:var(--text-muted);", "{fname}" }
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
                                        class: "{TOOL_BTN} tool-btn--sm",
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
                        span { style: "font-size:13px;font-weight:600;font-family:var(--font-mono);color:var(--text-secondary);",
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
                        class: "{INPUT}", style: "flex:1;resize:none;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
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
                            class: "{TOOL_BTN} tool-btn--primary tool-btn--md",
                            "Save"
                        }
                        button {
                            disabled: !file_dirty(),
                            onclick: move |_| {
                                file_content.set(original_content());
                                file_dirty.set(false);
                            },
                            class: "{TOOL_BTN} tool-btn--md",
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
                            class: "{TOOL_BTN} tool-btn--md tool-btn--danger",
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

#[derive(Clone)]
struct ToolCategory {
    name: &'static str,
    tools: Vec<ToolDefinition>,
}

#[derive(Clone, Copy)]
struct ToolDefinition {
    id: &'static str,
    label: &'static str,
    description: &'static str,
}

fn tool_categories() -> Vec<ToolCategory> {
    vec![
        ToolCategory {
            name: "Files",
            tools: vec![
                ToolDefinition {
                    id: "read_file",
                    label: "Read files",
                    description: "Read files from disk",
                },
                ToolDefinition {
                    id: "write_file",
                    label: "Write files",
                    description: "Write or create files",
                },
                ToolDefinition {
                    id: "list_dir",
                    label: "List directories",
                    description: "List directories and files",
                },
            ],
        },
        ToolCategory {
            name: "Runtime",
            tools: vec![
                ToolDefinition {
                    id: "shell",
                    label: "Shell",
                    description: "Execute shell commands",
                },
                ToolDefinition {
                    id: "grep_files",
                    label: "Search code",
                    description: "Search code in the workspace",
                },
            ],
        },
        ToolCategory {
            name: "Web",
            tools: vec![
                ToolDefinition {
                    id: "browser",
                    label: "Browser",
                    description: "Browser automation and page interaction",
                },
                ToolDefinition {
                    id: "web_fetch",
                    label: "Fetch URLs",
                    description: "Fetch URLs and inspect responses",
                },
                ToolDefinition {
                    id: "web_search",
                    label: "Web search",
                    description: "Search the web",
                },
            ],
        },
        ToolCategory {
            name: "Memory",
            tools: vec![ToolDefinition {
                id: "md_memory",
                label: "Markdown memory",
                description: "Markdown memory operations",
            }],
        },
        ToolCategory {
            name: "Media",
            tools: vec![ToolDefinition {
                id: "image_generate",
                label: "Generate images",
                description: "Generate images",
            }],
        },
    ]
}

fn empty_tool_states(categories: &[ToolCategory]) -> std::collections::HashMap<String, bool> {
    let mut states = std::collections::HashMap::new();
    for category in categories {
        for tool in &category.tools {
            states.insert(tool.id.to_string(), false);
        }
    }
    states
}

fn tool_profile_tools(profile: &str) -> Option<&'static [&'static str]> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "minimal" => Some(&["read_file", "list_dir", "grep_files", "md_memory"]),
        "coding" => Some(&[
            "read_file",
            "write_file",
            "list_dir",
            "shell",
            "grep_files",
            "md_memory",
        ]),
        "messaging" => Some(&[
            "read_file",
            "list_dir",
            "browser",
            "web_fetch",
            "web_search",
            "md_memory",
        ]),
        "full" | "unrestricted" => None, // all tools enabled
        _ => None,
    }
}

fn tool_states_for_profile(
    profile: &str,
    categories: &[ToolCategory],
) -> std::collections::HashMap<String, bool> {
    let normalized = profile.trim().to_ascii_lowercase();
    let all_enabled = matches!(normalized.as_str(), "full" | "unrestricted");
    let mut states: std::collections::HashMap<String, bool> = categories
        .iter()
        .flat_map(|cat| {
            cat.tools
                .iter()
                .map(move |t| (t.id.to_string(), all_enabled))
        })
        .collect();
    if !all_enabled {
        if let Some(preset) = tool_profile_tools(profile) {
            for tool_id in preset {
                states.insert((*tool_id).to_string(), true);
            }
        }
    }
    states
}

fn split_loaded_tools(
    categories: &[ToolCategory],
    enabled_tools: &[String],
) -> (
    std::collections::HashMap<String, bool>,
    std::collections::HashSet<String>,
) {
    let mut states = empty_tool_states(categories);
    let known: std::collections::HashSet<&str> = categories
        .iter()
        .flat_map(|category| category.tools.iter().map(|tool| tool.id))
        .collect();
    let mut extra = std::collections::HashSet::new();

    for tool in enabled_tools {
        if known.contains(tool.as_str()) {
            states.insert(tool.clone(), true);
        } else {
            extra.insert(tool.clone());
        }
    }

    (states, extra)
}

#[component]
fn AgentToolsTab(ws: WsRpc, mut refresh_tick: Signal<u32>, entry: AgentEntry) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();
    let agent_id = agent_ref(&entry);

    let profiles = ["minimal", "coding", "messaging", "full", "inherit"];
    let categories = tool_categories();
    let categories_for_load = categories.clone();
    let mut selected_profile = use_signal(|| "coding".to_string());
    let mut tool_states: Signal<std::collections::HashMap<String, bool>> =
        use_signal(|| empty_tool_states(&categories));
    let mut passthrough_tools: Signal<std::collections::HashSet<String>> =
        use_signal(std::collections::HashSet::new);
    let mut loaded_tools_key = use_signal(|| None::<String>);

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

    use_effect(move || {
        let loaded = tools_data.read().clone();
        let Some(Some(detail)) = loaded else {
            return;
        };

        // Extract profile and tool list from permission_policy.
        let (profile, enabled_tools) = if let Some(pp) = &detail.permission_policy {
            let prof = pp
                .get("_preset_id")
                .and_then(|v| v.as_str())
                .unwrap_or("coding")
                .to_string();
            let tools: Vec<String> = pp
                .get("tool_access")
                .and_then(|ta| ta.get("allowed"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (prof, tools)
        } else {
            ("coding".to_string(), Vec::new())
        };

        let sync_key =
            serde_json::to_string(&json!({ "profile": profile, "tools": enabled_tools }))
                .unwrap_or_default();
        if loaded_tools_key().as_deref() == Some(sync_key.as_str()) {
            return;
        }

        let (states, extra) = if enabled_tools.is_empty() {
            (
                tool_states_for_profile(&profile, &categories_for_load),
                std::collections::HashSet::new(),
            )
        } else {
            split_loaded_tools(&categories_for_load, &enabled_tools)
        };

        loaded_tools_key.set(Some(sync_key));
        selected_profile.set(profile);
        tool_states.set(states);
        passthrough_tools.set(extra);
    });

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            // Profile selector (segmented control)
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Tools Profile" }
                div { class: "cfg-segmented",
                    for profile in profiles.iter() {
                        {
                            let p = profile.to_string();
                            let display = {
                                let mut chars = p.chars();
                                match chars.next() {
                                    None => String::new(),
                                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                                }
                            };
                            let preset_categories = categories.clone();
                            let is_active = selected_profile() == p;
                            let class = if is_active { "cfg-segmented__btn active" } else { "cfg-segmented__btn" };
                            rsx! {
                                button {
                                    key: "{profile}",
                                    class: "{class}",
                                    onclick: move |_| {
                                        selected_profile.set(p.clone());
                                        if p != "inherit" {
                                            tool_states.set(tool_states_for_profile(&p, &preset_categories));
                                        }
                                    },
                                    "{display}"
                                }
                            }
                        }
                    }
                }
            }

            // Per-tool toggles grouped by category
            for cat in categories.iter() {
                {
                    let group_all_enabled = cat.tools.iter().all(|tool| {
                        tool_states
                            .read()
                            .get(tool.id)
                            .copied()
                            .unwrap_or(false)
                    });
                    rsx! {
                        div { class: "{SECTION_CARD}",
                            div { style: "display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:8px;",
                                h4 { class: "{SECTION_TITLE}", style: "margin-bottom:0;", "{cat.name}" }
                                button {
                                    class: "{TOOL_BTN} tool-btn--sm",
                                    onclick: {
                                        let next_value = !group_all_enabled;
                                        let tools = cat.tools.clone();
                                        move |_| {
                                            let mut states = tool_states.write();
                                            for tool in &tools {
                                                states.insert(tool.id.to_string(), next_value);
                                            }
                                        }
                                    },
                                    if group_all_enabled { "Disable All" } else { "Enable All" }
                                }
                            }
                            div { style: "display:flex;flex-direction:column;gap:4px;",
                                for tool in cat.tools.iter() {
                                    {
                                        let id = tool.id.to_string();
                                        let id_toggle = tool.id.to_string();
                                        let enabled = tool_states
                                            .read()
                                            .get(&id)
                                            .copied()
                                            .unwrap_or(false);
                                        rsx! {
                                            ToggleSwitch {
                                                key: "{tool.id}",
                                                label: tool.label.to_string(),
                                                description: Some(tool.description.to_string()),
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
                            let profile = selected_profile();
                            let permission_policy = if profile.eq_ignore_ascii_case("inherit") {
                                serde_json::Value::Null
                            } else {
                                let mut enabled: Vec<String> = tool_states
                                    .read()
                                    .iter()
                                    .filter_map(|(k, v)| if *v { Some(k.clone()) } else { None })
                                    .collect();
                                enabled.extend(passthrough_tools.read().iter().cloned());
                                enabled.sort();
                                enabled.dedup();
                                // Build a permission_policy with the selected preset and tools.
                                let sandbox = match profile.as_str() {
                                    "minimal" | "messaging" => "read-only",
                                    _ => "workspace-write",
                                };
                                let approval = "on-request";
                                json!({
                                    "_preset_id": profile,
                                    "sandbox": sandbox,
                                    "approval": approval,
                                    "tool_access": {
                                        "allowed": enabled,
                                        "denied": []
                                    }
                                })
                            };
                            spawn(async move {
                                let res = ws.call::<serde_json::Value>(
                                    "agents.update",
                                    Some(json!({
                                        "agent": id,
                                        "permission_policy": permission_policy,
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
                    class: "{TOOL_BTN} tool-btn--primary tool-btn--lg",
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

    // Only show skills that are globally installed AND enabled.
    let available_skills: Vec<SkillDetail> = all_skills
        .into_iter()
        .filter(|s| s.installed.unwrap_or(false) && s.enabled.unwrap_or(false))
        .collect();

    // Filter by search
    let query = search_query().to_lowercase();
    let filtered: Vec<SkillDetail> = if query.is_empty() {
        available_skills.clone()
    } else {
        available_skills
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
                SkeletonLines { count: 4 }
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
        div { class: "{SECTION_CARD}",
            h4 { class: "{SECTION_TITLE}", "{title} ({skills.len()})" }
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
// Helpers & Constants
// ---------------------------------------------------------------------------

const TOOL_BTN: &str = "tool-btn";
const INPUT: &str = "form-input";
const LABEL: &str = "form-label";
const SECTION_CARD: &str = "section-card";
const SECTION_TITLE: &str = "section-title";
const TH: &str = "th-cell";
const TD: &str = "td-cell";

#[cfg(test)]
mod tests {
    use super::{model_option_label, model_select_value};
    use crate::api::types::ModelInfo;

    fn test_model(id: &str, name: Option<&str>, model_slug: Option<&str>) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            name: name.map(str::to_string),
            provider: None,
            model_slug: model_slug.map(str::to_string),
            api_key: None,
            base_url: None,
            max_tokens: None,
            temperature: None,
            is_default: None,
            builtin: None,
            default_reasoning_level: None,
            supported_reasoning_levels: None,
            account_slug: None,
            provider_name: None,
            account_name: None,
        }
    }

    #[test]
    fn model_select_value_uses_canonical_id_for_case_insensitive_match() {
        let models = vec![test_model("openai/gpt-5", Some("GPT-5"), Some("gpt-5"))];

        assert_eq!(model_select_value(&models, "OpenAI/GPT-5"), "openai/gpt-5");
    }

    #[test]
    fn model_select_value_preserves_unknown_saved_model() {
        let models = vec![test_model("openai/gpt-5", Some("GPT-5"), Some("gpt-5"))];

        assert_eq!(
            model_select_value(&models, "custom-provider/custom-model"),
            "custom-provider/custom-model"
        );
    }

    #[test]
    fn model_option_label_prefers_name_then_slug_then_id() {
        assert_eq!(
            model_option_label(&test_model("openai/gpt-5", Some("GPT-5"), Some("gpt-5"))),
            "GPT-5"
        );
        assert_eq!(
            model_option_label(&test_model("openai/gpt-5", Some("  "), Some("gpt-5"))),
            "gpt-5"
        );
        assert_eq!(
            model_option_label(&test_model("openai/gpt-5", None, None)),
            "openai/gpt-5"
        );
    }
}
