use dioxus::prelude::*;
use lucide_dioxus::{ArrowLeft, Bot, Terminal, X};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{
    AgentDetail, AgentEntry, AgentExecutionPolicy, AgentFile, AgentFilesResponse,
    AgentIdleReplyConfig, AgentPermissionPolicy, AgentTerminalDelegateConfig,
    AgentToolAccessPolicy, AgentsResponse, AvailableModel, AvailableModelsResponse, SkillDetail,
    SkillsBinsResponse, SkillsStatusResponse, agent_preset_boundary,
};
use crate::api::ws::WsRpc;
use crate::components::icon::Icon;
use crate::components::model_selector::ModelSelector;
use crate::components::search_input::SearchInput;
use crate::components::skeleton::*;
use crate::components::tab_bar::{TabBar, TabItem};
use crate::components::toast::Toaster;
use crate::components::toggle_switch::ToggleSwitch;
use crate::utils::deep_link::replace_url;
use crate::utils::download::trigger_download;
use crate::utils::provider_registry::{canonical_provider_id, provider_display_name};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChannelConfigOption {
    id: String,
    kind: String,
    name: String,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChannelReplyFormRow {
    channel_id: String,
    group_activation: String,
}

// ---------------------------------------------------------------------------
// Agent status indicator helper
// ---------------------------------------------------------------------------

fn agent_status_color(entry: &AgentEntry) -> &'static str {
    match entry.status.as_deref() {
        Some("active") | Some("running") | Some("online") => "#22c55e", // green
        Some("error") | Some("failed") => "#ef4444",                    // red
        Some("idle") | Some("stopped") | Some("offline") => "#9ca3af",  // gray
        Some(_) => "#9ca3af",
        None => {
            // Derive status from config: if agent has no model configured, show gray (offline)
            if entry_has_terminal_delegate(entry)
                || !native_model_for_entry(entry).trim().is_empty()
            {
                "#22c55e"
            } else {
                "#9ca3af"
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
            if entry_has_terminal_delegate(entry)
                || !native_model_for_entry(entry).trim().is_empty()
            {
                "Online"
            } else {
                "Offline"
            }
        }
    }
}

fn entry_has_terminal_delegate(entry: &AgentEntry) -> bool {
    entry.kind.as_str() == "terminal"
        && entry
            .terminal
            .as_ref()
            .map(|terminal| &terminal.delegate)
            .and_then(|config| config.enabled)
            .unwrap_or(false)
        && entry
            .terminal
            .as_ref()
            .map(|terminal| &terminal.delegate)
            .and_then(|config| config.command.as_deref())
            .map(str::trim)
            .is_some_and(|command| !command.is_empty())
}

fn agent_kind_value(kind: &crate::api::types::AgentKind) -> &'static str {
    match kind.as_str() {
        "terminal" => "terminal",
        _ => "native",
    }
}

fn terminal_delegate_for_detail(detail: &AgentDetail) -> Option<&AgentTerminalDelegateConfig> {
    detail.terminal.as_ref().map(|terminal| &terminal.delegate)
}

fn terminal_config_raw(detail_raw: Option<&Value>) -> Option<&Value> {
    detail_raw.and_then(|detail| detail.get("terminal"))
}

fn native_model_for_entry(entry: &AgentEntry) -> String {
    entry
        .native
        .as_ref()
        .map(|native| native.model.clone())
        .unwrap_or_default()
}

fn native_provider_for_model(model: &str) -> String {
    model
        .split_once('/')
        .map(|(provider, _)| provider.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Agent templates
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

fn normalize_multiline_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split([',', '\n', '\r']) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn multiline_list_value(values: Option<&Vec<String>>) -> String {
    values.map(|items| items.join("\n")).unwrap_or_default()
}

fn json_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => {
            let joined = items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            normalize_multiline_list(&joined)
        }
        Some(Value::String(raw)) => normalize_multiline_list(raw),
        _ => Vec::new(),
    }
}

fn normalized_group_activation(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "keyword" => "keyword".to_string(),
        "always" => "always".to_string(),
        "command" => "command".to_string(),
        "off" => "off".to_string(),
        _ => "mention".to_string(),
    }
}

fn json_channel_reply_rows(value: Option<&Value>) -> Vec<ChannelReplyFormRow> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    items
        .iter()
        .filter_map(|item| {
            let channel_id = item
                .get("channel_id")
                .or_else(|| item.get("route"))
                .or_else(|| item.get("channel"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            let key = channel_id.to_ascii_lowercase();
            if !seen.insert(key) {
                return None;
            }
            let group_activation = item
                .get("group_activation")
                .or_else(|| item.get("activation"))
                .and_then(Value::as_str)
                .map(normalized_group_activation)
                .unwrap_or_else(|| "mention".to_string());
            Some(ChannelReplyFormRow {
                channel_id,
                group_activation,
            })
        })
        .collect()
}

fn channel_replies_payload(rows: &[ChannelReplyFormRow]) -> Option<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let payload = rows
        .iter()
        .filter_map(|row| {
            let channel_id = row.channel_id.trim();
            if channel_id.is_empty() {
                return None;
            }
            let key = channel_id.to_ascii_lowercase();
            if !seen.insert(key) {
                return None;
            }
            Some(json!({
                "channel_id": channel_id,
                "group_activation": normalized_group_activation(&row.group_activation),
            }))
        })
        .collect::<Vec<_>>();
    (!payload.is_empty()).then_some(Value::Array(payload))
}

fn channel_config_options(configs: &[Value]) -> Vec<ChannelConfigOption> {
    let mut options = configs
        .iter()
        .filter_map(|cfg| {
            let id = cfg
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            let kind = cfg
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let name = cfg
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let enabled = cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            Some(ChannelConfigOption {
                id,
                kind,
                name,
                enabled,
            })
        })
        .collect::<Vec<_>>();
    options.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    options
}

fn channel_option_label(option: &ChannelConfigOption) -> String {
    let kind = option.kind.trim();
    let name = option.name.trim();
    let base = if !kind.is_empty() && !name.is_empty() {
        format!("{kind} / {name}")
    } else if !name.is_empty() {
        name.to_string()
    } else if !kind.is_empty() {
        kind.to_string()
    } else {
        option.id.clone()
    };
    if base == option.id {
        base
    } else if option.enabled {
        format!("{base} ({})", option.id)
    } else {
        format!("{base} ({}, disabled)", option.id)
    }
}

fn matrix_appservice_channels(configs: &[Value]) -> Vec<(String, String, String, String)> {
    configs
        .iter()
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
        .collect()
}

fn idle_reply_delay_value(value: Option<u64>) -> String {
    value.unwrap_or(180).to_string()
}

fn json_idle_reply_fields(value: Option<&Value>) -> (bool, String, String, String) {
    let Some(config) = value.and_then(Value::as_object) else {
        return (
            false,
            idle_reply_delay_value(None),
            "1".to_string(),
            String::new(),
        );
    };

    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let delay = idle_reply_delay_value(config.get("delay_secs").and_then(Value::as_u64));
    let max_per_hour = config
        .get("max_per_hour")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .to_string();
    let prompt = config
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (enabled, delay, max_per_hour, prompt)
}

fn detail_idle_reply_fields(
    detail: Option<&AgentIdleReplyConfig>,
) -> (bool, String, String, String) {
    let Some(config) = detail else {
        return (
            false,
            idle_reply_delay_value(None),
            "1".to_string(),
            String::new(),
        );
    };
    (
        config.enabled.unwrap_or(false),
        idle_reply_delay_value(config.delay_secs),
        config.max_per_hour.unwrap_or(1).to_string(),
        config.prompt.clone().unwrap_or_default(),
    )
}

fn idle_reply_payload(
    enabled: bool,
    delay_raw: &str,
    max_per_hour_raw: &str,
    prompt_raw: &str,
) -> Option<Value> {
    let prompt = prompt_raw.trim();
    let delay_secs = delay_raw
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= 30)
        .unwrap_or(180);
    let max_per_hour = max_per_hour_raw
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1);
    if !enabled && delay_secs == 180 && max_per_hour == 1 && prompt.is_empty() {
        return None;
    }

    let mut payload = json!({
        "enabled": enabled,
        "delay_secs": delay_secs,
        "max_per_hour": max_per_hour,
    });
    if !prompt.is_empty() {
        payload["prompt"] = json!(prompt);
    }
    Some(payload)
}

fn terminal_args_value(args: Option<&Vec<String>>) -> String {
    args.map(|items| items.join("\n")).unwrap_or_default()
}

fn terminal_env_value(env: Option<&std::collections::BTreeMap<String, String>>) -> String {
    env.map(|items| {
        items
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
    .unwrap_or_default()
}

fn terminal_health_message(value: &Value) -> (bool, String) {
    let command = value
        .get("command")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let command_name = command
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("terminal");
    let available = command
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let version = command.get("version").and_then(Value::as_str).unwrap_or("");
    let error = command.get("error").and_then(Value::as_str).unwrap_or("");
    let cwd_ok = value
        .get("cwd_is_dir")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let root_ok = value
        .get("terminal_root_writable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ok = available && cwd_ok && root_ok;

    let detail = if available {
        if version.is_empty() {
            format!("{command_name} is available")
        } else {
            format!("{command_name}: {version}")
        }
    } else if error.is_empty() {
        format!("{command_name} is not available")
    } else {
        format!("{command_name}: {error}")
    };
    let dirs = match (cwd_ok, root_ok) {
        (true, true) => "paths OK",
        (false, true) => "cwd unavailable",
        (true, false) => "terminal root not writable",
        (false, false) => "cwd unavailable; terminal root not writable",
    };

    (ok, format!("{detail}; {dirs}"))
}

fn json_terminal_delegate_fields(
    value: Option<&Value>,
) -> (bool, String, String, String, String, String, String, bool) {
    let Some(config) = value.and_then(Value::as_object) else {
        return (
            false,
            String::new(),
            "{{prompt}}".to_string(),
            String::new(),
            String::new(),
            String::new(),
            "300".to_string(),
            true,
        );
    };

    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = config
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "{{prompt}}".to_string());
    let stdin = config
        .get("stdin")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = config
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let env = config
        .get("env")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| format!("{key}={value}")))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let timeout = config
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .to_string();
    let include_system_prompt = config
        .get("include_system_prompt")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    (
        enabled,
        command,
        args,
        stdin,
        cwd,
        env,
        timeout,
        include_system_prompt,
    )
}

fn json_terminal_runtime_fields(value: Option<&Value>) -> (String, String, String, String) {
    let Some(config) = value.and_then(Value::as_object) else {
        return (
            "codex".to_string(),
            "one_shot".to_string(),
            "per_session".to_string(),
            "plain_text".to_string(),
        );
    };

    (
        config
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("codex")
            .to_string(),
        config
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("one_shot")
            .to_string(),
        config
            .get("session_scope")
            .and_then(Value::as_str)
            .unwrap_or("per_session")
            .to_string(),
        config
            .get("io_protocol")
            .and_then(Value::as_str)
            .unwrap_or("plain_text")
            .to_string(),
    )
}

fn json_terminal_permission_fields(value: Option<&Value>) -> (String, String) {
    let Some(config) = value.and_then(Value::as_object) else {
        return ("native_terminal".to_string(), "disabled".to_string());
    };

    let bridge = config
        .get("savfox_approval_bridge")
        .and_then(|value| {
            value.as_str().map(ToString::to_string).or_else(|| {
                value.as_bool().map(|enabled| {
                    if enabled {
                        "prompt".to_string()
                    } else {
                        "disabled".to_string()
                    }
                })
            })
        })
        .unwrap_or_else(|| "disabled".to_string());
    let execution = config
        .get("terminal_execution")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if bridge != "disabled" {
                "managed_workspace".to_string()
            } else {
                "native_terminal".to_string()
            }
        });

    (execution, bridge)
}

fn json_terminal_workspace_fields(value: Option<&Value>) -> (String, String, String) {
    let workspace = value
        .and_then(Value::as_object)
        .and_then(|config| config.get("workspace"))
        .and_then(Value::as_object);

    let mode = workspace
        .and_then(|workspace| workspace.get("mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("shared")
        .to_string();
    let base = workspace
        .and_then(|workspace| workspace.get("base"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cleanup_policy = workspace
        .and_then(|workspace| workspace.get("cleanup_policy"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("per_session")
        .to_string();

    (mode, base, cleanup_policy)
}

fn detail_terminal_delegate_fields(
    detail: Option<&AgentTerminalDelegateConfig>,
) -> (bool, String, String, String, String, String, String, bool) {
    let Some(config) = detail else {
        return (
            false,
            String::new(),
            "{{prompt}}".to_string(),
            String::new(),
            String::new(),
            String::new(),
            "300".to_string(),
            true,
        );
    };

    (
        config.enabled.unwrap_or(false),
        config.command.clone().unwrap_or_default(),
        terminal_args_value(config.args.as_ref()),
        config.stdin.clone().unwrap_or_default(),
        config.cwd.clone().unwrap_or_default(),
        terminal_env_value(config.env.as_ref()),
        config.timeout_secs.unwrap_or(300).to_string(),
        config.include_system_prompt.unwrap_or(true),
    )
}

fn detail_terminal_runtime_fields(
    detail: Option<&AgentTerminalDelegateConfig>,
) -> (String, String, String, String) {
    let Some(config) = detail else {
        return (
            "codex".to_string(),
            "one_shot".to_string(),
            "per_session".to_string(),
            "plain_text".to_string(),
        );
    };

    (
        config
            .profile
            .as_ref()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| "codex".to_string()),
        config
            .mode
            .as_ref()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| "one_shot".to_string()),
        config
            .session_scope
            .as_ref()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| "per_session".to_string()),
        config
            .io_protocol
            .as_ref()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| "plain_text".to_string()),
    )
}

fn detail_terminal_interactive_fields(
    detail: Option<&AgentTerminalDelegateConfig>,
) -> (String, String) {
    let Some(config) = detail else {
        return (String::new(), String::new());
    };
    (
        config.interactive_command.clone().unwrap_or_default(),
        terminal_args_value(config.interactive_args.as_ref()),
    )
}

fn json_terminal_interactive_fields(value: Option<&Value>) -> (String, String) {
    let Some(config) = value.and_then(Value::as_object) else {
        return (String::new(), String::new());
    };
    let command = config
        .get("interactive_command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = config
        .get("interactive_args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    (command, args)
}

fn normalize_terminal_args(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_terminal_env(raw: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.insert(key.to_string(), value.trim().to_string());
    }
    out
}

fn terminal_launch_config_summary(
    command_raw: &str,
    interactive_command_raw: &str,
    cwd_raw: &str,
    env_raw: &str,
) -> (String, String, String) {
    let command = interactive_command_raw
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            command_raw
                .split_whitespace()
                .next()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("(not configured)")
        .to_string();
    let cwd = cwd_raw.trim();
    let cwd = if cwd.is_empty() {
        "Savfox cwd".to_string()
    } else {
        cwd.to_string()
    };
    let env_count = normalize_terminal_env(env_raw).len();
    let env = if env_count == 0 {
        "no env overrides".to_string()
    } else {
        format!("{env_count} env override(s)")
    };

    (command, cwd, env)
}

fn terminal_launch_result_summary(value: &Value) -> String {
    let terminal = value
        .get("terminal")
        .and_then(Value::as_str)
        .unwrap_or("terminal");
    let pid = value
        .get("pid")
        .and_then(Value::as_u64)
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    let mut summary = format!("terminal: {terminal}; pid: {pid}");
    if !command.is_empty() {
        summary.push_str(&format!("; command: {command}"));
    }
    if !cwd.is_empty() {
        summary.push_str(&format!("; cwd: {cwd}"));
    }
    summary
}

fn terminal_delegate_payload(
    enabled: bool,
    command_raw: &str,
    args_raw: &str,
    stdin_raw: &str,
    cwd_raw: &str,
    env_raw: &str,
    timeout_raw: &str,
    include_system_prompt: bool,
    interactive_command_raw: &str,
    interactive_args_raw: &str,
    profile_raw: &str,
    mode_raw: &str,
    session_scope_raw: &str,
    io_protocol_raw: &str,
    terminal_execution_raw: &str,
    savfox_approval_bridge_raw: &str,
    workspace_mode_raw: &str,
    workspace_base_raw: &str,
    workspace_cleanup_policy_raw: &str,
) -> Option<Value> {
    let command = command_raw.trim();
    let args = normalize_terminal_args(args_raw);
    let stdin = stdin_raw.trim();
    let cwd = cwd_raw.trim();
    let env = normalize_terminal_env(env_raw);
    let timeout_secs = timeout_raw
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= 5)
        .unwrap_or(300);
    let interactive_command = interactive_command_raw.trim();
    let interactive_args = normalize_terminal_args(interactive_args_raw);
    let profile = profile_raw.trim();
    let mode = mode_raw.trim();
    let session_scope = session_scope_raw.trim();
    let io_protocol = io_protocol_raw.trim();
    let terminal_execution = terminal_execution_raw.trim();
    let savfox_approval_bridge = savfox_approval_bridge_raw.trim();
    let workspace_mode = workspace_mode_raw.trim();
    let workspace_base = workspace_base_raw.trim();
    let workspace_cleanup_policy = workspace_cleanup_policy_raw.trim();

    if !enabled
        && command.is_empty()
        && args == vec!["{{prompt}}".to_string()]
        && stdin.is_empty()
        && cwd.is_empty()
        && env.is_empty()
        && timeout_secs == 300
        && include_system_prompt
        && interactive_command.is_empty()
        && interactive_args.is_empty()
        && matches!(profile, "" | "codex")
        && matches!(mode, "" | "one_shot")
        && matches!(session_scope, "" | "per_session")
        && matches!(io_protocol, "" | "plain_text")
        && matches!(terminal_execution, "" | "native_terminal")
        && matches!(savfox_approval_bridge, "" | "disabled")
        && matches!(workspace_mode, "" | "shared")
        && workspace_base.is_empty()
        && matches!(workspace_cleanup_policy, "" | "per_session")
    {
        return None;
    }

    let mut payload = json!({
        "enabled": enabled,
        "timeout_secs": timeout_secs,
        "include_system_prompt": include_system_prompt,
    });
    if !command.is_empty() {
        payload["command"] = json!(command);
    }
    if !args.is_empty() {
        payload["args"] = json!(args);
    }
    if !stdin.is_empty() {
        payload["stdin"] = json!(stdin);
    }
    if !cwd.is_empty() {
        payload["cwd"] = json!(cwd);
    }
    if !env.is_empty() {
        payload["env"] = json!(env);
    }
    if !interactive_command.is_empty() {
        payload["interactive_command"] = json!(interactive_command);
    }
    if !interactive_args.is_empty() {
        payload["interactive_args"] = json!(interactive_args);
    }
    if !profile.is_empty() {
        payload["profile"] = json!(profile);
    }
    if !mode.is_empty() {
        payload["mode"] = json!(mode);
    }
    if !session_scope.is_empty() {
        payload["session_scope"] = json!(session_scope);
    }
    if !io_protocol.is_empty() {
        payload["io_protocol"] = json!(io_protocol);
    }
    payload["terminal_execution"] = json!(if terminal_execution.is_empty() {
        "native_terminal"
    } else {
        terminal_execution
    });
    payload["savfox_approval_bridge"] = json!(if savfox_approval_bridge.is_empty() {
        "disabled"
    } else {
        savfox_approval_bridge
    });
    payload["workspace"] = json!({
        "mode": if workspace_mode.is_empty() {
            "shared"
        } else {
            workspace_mode
        },
        "cleanup_policy": if workspace_cleanup_policy.is_empty() {
            "per_session"
        } else {
            workspace_cleanup_policy
        },
    });
    if !workspace_base.is_empty() {
        payload["workspace"]["base"] = json!(workspace_base);
    }

    Some(payload)
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

fn normalized_reasoning_presets(model: Option<&AvailableModel>) -> Vec<(String, String)> {
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
        let Some(effort) = normalized_reasoning_level(preset.effort.as_wire_str()) else {
            continue;
        };
        if presets.iter().any(|(value, _)| value == &effort) {
            continue;
        }
        presets.push((effort, preset.description.clone()));
    }
    presets
}

fn model_default_reasoning_level(model: Option<&AvailableModel>) -> Option<String> {
    model.and_then(|model| {
        model
            .default_reasoning_level
            .map(|effort| effort.as_wire_str())
            .and_then(normalized_reasoning_level)
    })
}

fn selected_model_info<'a>(
    models: &'a [AvailableModel],
    model_id: &str,
) -> Option<&'a AvailableModel> {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return None;
    }

    models
        .iter()
        .find(|model| model.id.eq_ignore_ascii_case(trimmed))
}

fn model_option_label(model: &AvailableModel) -> String {
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

/// The agent's primary model, normalized the same way the editor seeds and
/// compares it: an unset/blank model falls back to the `"default"` sentinel.
/// Both the form's initial value and the dirty-check baseline must go through
/// this, otherwise an agent with no explicit model reads as perpetually dirty.
fn normalized_primary_model(entry: &AgentEntry) -> String {
    let model = native_model_for_entry(entry);
    if model.trim().is_empty() {
        "default".to_string()
    } else {
        model
    }
}

fn model_select_value(models: &[AvailableModel], model_id: &str) -> String {
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
    DetailTab(String, String),
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

#[component]
pub fn AgentsDetailTab(agent_id: String, tab: String) -> Element {
    agents_inner(AgentDeepLink::DetailTab(agent_id, tab))
}

fn agents_inner(deep_link: AgentDeepLink) -> Element {
    let is_routed = !matches!(deep_link, AgentDeepLink::None);
    let nav = use_navigator();

    let initial_selected = match &deep_link {
        AgentDeepLink::Detail(id) | AgentDeepLink::DetailTab(id, _) => Some(id.clone()),
        _ => Option::None,
    };
    let initial_tab = match &deep_link {
        AgentDeepLink::DetailTab(_, tab) => tab.clone(),
        _ => "overview".to_string(),
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
    let mut active_tab = use_signal(move || initial_tab);

    // Sync URL with current view state for deep linking
    use_effect(move || {
        let selected = selected_agent();
        let creating = show_create();
        let tab = active_tab();

        if let Some(ref id) = selected {
            if tab != "overview" {
                replace_url(&format!("/agents/{id}/{tab}"));
            } else {
                replace_url(&format!("/agents/{id}"));
            }
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
    let mut new_agent_kind = use_signal(|| "native".to_string());
    let mut new_provider = use_signal(String::new);
    let mut new_model = use_signal(String::new);
    let mut new_prompt = use_signal(String::new);
    let mut new_group_activation = use_signal(String::new);
    let mut new_group_keywords = use_signal(String::new);
    let mut new_agent_aliases = use_signal(String::new);
    let mut new_ingest_policy = use_signal(String::new);
    let mut new_external_bot_policy = use_signal(String::new);
    let mut new_idle_reply_enabled = use_signal(|| false);
    let mut new_idle_reply_delay = use_signal(|| "180".to_string());
    let mut new_idle_reply_max_per_hour = use_signal(|| "1".to_string());
    let mut new_idle_reply_prompt = use_signal(String::new);
    let mut new_terminal_enabled = use_signal(|| false);
    let mut new_terminal_command = use_signal(String::new);
    let mut new_terminal_args = use_signal(|| "{{prompt}}".to_string());
    let mut new_terminal_stdin = use_signal(String::new);
    let mut new_terminal_cwd = use_signal(String::new);
    let mut new_terminal_env = use_signal(String::new);
    let mut new_terminal_timeout = use_signal(|| "300".to_string());
    let mut new_terminal_include_system_prompt = use_signal(|| true);
    let mut new_terminal_profile = use_signal(|| "codex".to_string());
    let mut new_terminal_mode = use_signal(|| "one_shot".to_string());
    let mut new_terminal_session_scope = use_signal(|| "per_session".to_string());
    let mut new_terminal_io_protocol = use_signal(|| "plain_text".to_string());
    let mut new_terminal_execution = use_signal(|| "native_terminal".to_string());
    let mut new_terminal_approval_bridge = use_signal(|| "disabled".to_string());
    let mut new_terminal_workspace_mode = use_signal(|| "shared".to_string());
    let mut new_terminal_workspace_base = use_signal(String::new);
    let mut new_terminal_workspace_cleanup_policy = use_signal(|| "per_session".to_string());

    // Fetch agent list
    let ws_list = ws.clone();
    let agents_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_list.clone();
        async move {
            ws.wait_connected().await;
            ws.call::<AgentsResponse>("agents.list", None)
                .await
                .map(|r| r.agents)
        }
    });

    let agents_state = agents_data.read().as_ref().cloned();
    let agents: Vec<AgentEntry> = agents_state
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .cloned()
        .unwrap_or_default();
    let agents_error = agents_state
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let is_loading = agents_state.is_none();

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
                        detail
                            .permission_policy
                            .as_ref()
                            .and_then(|policy| policy.tool_access.as_ref())
                            .map(|tool_access| tool_access.allowed.len())
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        }
    });

    // Fetch skills count via lightweight status endpoint instead of loading all bins
    let ws_skills_count = ws.clone();
    let skills_count_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_skills_count.clone();
        async move {
            ws.call::<SkillsStatusResponse>("skills.status", None)
                .await
                .map(|r| r.installed_count.unwrap_or(0) as usize)
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
        div { class: "split-view {detail_class}",
            // ── Left sidebar: agent list (~30%) ──
            div { class: "split-view__list",
                div { class: "panel-header",
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
                        // Import agent from JSON file
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
                                            let kind = parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("native").to_string();
                                            let native = parsed.get("native").cloned();
                                            let terminal = parsed.get("terminal").cloned();
                                            let description = parsed.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let group_activation = parsed.get("group_activation").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let channel_replies = json_channel_reply_rows(parsed.get("channel_replies"));
                                            let group_keywords = json_string_list(parsed.get("group_keywords"));
                                            let agent_aliases = json_string_list(parsed.get("agent_aliases"));
                                            let ingest_policy = parsed.get("ingest_policy").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let external_bot_policy = parsed.get("external_bot_policy").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                            let (idle_reply_enabled, idle_reply_delay, idle_reply_max_per_hour, idle_reply_prompt) =
                                                json_idle_reply_fields(parsed.get("idle_reply"));
                                            let (
                                                terminal_enabled,
                                                terminal_command,
                                                terminal_args,
                                                terminal_stdin,
                                                terminal_cwd,
                                                terminal_env,
                                                terminal_timeout,
                                                terminal_include_system_prompt,
                                            ) = json_terminal_delegate_fields(parsed.get("terminal"));
                                            let (
                                                terminal_interactive_command,
                                                terminal_interactive_args,
                                            ) = json_terminal_interactive_fields(parsed.get("terminal"));
                                            let (
                                                terminal_profile,
                                                terminal_mode,
                                                terminal_session_scope,
                                                terminal_io_protocol,
                                            ) = json_terminal_runtime_fields(parsed.get("terminal"));
                                            let (
                                                terminal_execution,
                                                terminal_approval_bridge,
                                            ) = json_terminal_permission_fields(parsed.get("terminal"));
                                            let (
                                                terminal_workspace_mode,
                                                terminal_workspace_base,
                                                terminal_workspace_cleanup_policy,
                                            ) = json_terminal_workspace_fields(parsed.get("terminal"));

                                            let id_slug: String = name.chars()
                                                .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
                                                .collect();

                                            let ws_spawn = ws_inner.clone();
                                            spawn(async move {
                                                let mut payload = json!({
                                                    "id": id_slug,
                                                    "name": name,
                                                    "kind": if kind == "terminal" { "terminal" } else { "native" },
                                                    "system_prompt": if description.is_empty() { system_prompt.clone() } else { description },
                                                });
                                                if !system_prompt.is_empty() {
                                                    payload["system_prompt"] = json!(system_prompt);
                                                }
                                                if kind == "terminal" {
                                                    if let Some(terminal) = terminal {
                                                        payload["terminal"] = terminal;
                                                    }
                                                } else if let Some(native) = native {
                                                    payload["native"] = native;
                                                }
                                                if !group_activation.is_empty() {
                                                    payload["group_activation"] = json!(group_activation);
                                                }
                                                if let Some(channel_replies) = channel_replies_payload(&channel_replies) {
                                                    payload["channel_replies"] = channel_replies;
                                                }
                                                if !group_keywords.is_empty() {
                                                    payload["group_keywords"] = json!(group_keywords);
                                                }
                                                if !agent_aliases.is_empty() {
                                                    payload["agent_aliases"] = json!(agent_aliases);
                                                }
                                                if !ingest_policy.is_empty() {
                                                    payload["ingest_policy"] = json!(ingest_policy);
                                                }
                                                if !external_bot_policy.is_empty() {
                                                    payload["external_bot_policy"] = json!(external_bot_policy);
                                                }
                                                if let Some(idle_reply) = idle_reply_payload(
                                                    idle_reply_enabled,
                                                    &idle_reply_delay,
                                                    &idle_reply_max_per_hour,
                                                    &idle_reply_prompt,
                                                ) {
                                                    payload["idle_reply"] = idle_reply;
                                                }
                                                if kind == "terminal" && payload.get("terminal").is_none()
                                                    && let Some(mut terminal) = terminal_delegate_payload(
                                                    terminal_enabled,
                                                    &terminal_command,
                                                    &terminal_args,
                                                    &terminal_stdin,
                                                    &terminal_cwd,
                                                    &terminal_env,
                                                    &terminal_timeout,
                                                    terminal_include_system_prompt,
                                                    &terminal_interactive_command,
                                                    &terminal_interactive_args,
                                                    &terminal_profile,
                                                    &terminal_mode,
                                                    &terminal_session_scope,
                                                    &terminal_io_protocol,
                                                    &terminal_execution,
                                                    &terminal_approval_bridge,
                                                    &terminal_workspace_mode,
                                                    &terminal_workspace_base,
                                                    &terminal_workspace_cleanup_policy,
                                                ) {
                                                    terminal["runtime"] = json!(terminal_profile);
                                                    payload["terminal"] = terminal;
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
                                new_agent_kind.set("native".to_string());
                                new_provider.set(String::new());
                                new_model.set(String::new());
                                new_prompt.set(String::new());
                                new_group_activation.set(String::new());
                                new_group_keywords.set(String::new());
                                new_agent_aliases.set(String::new());
                                new_ingest_policy.set(String::new());
                                new_external_bot_policy.set(String::new());
                                new_idle_reply_enabled.set(false);
                                new_idle_reply_delay.set("180".to_string());
                                new_idle_reply_max_per_hour.set("1".to_string());
                                new_idle_reply_prompt.set(String::new());
                                new_terminal_enabled.set(false);
                                new_terminal_command.set(String::new());
                                new_terminal_args.set("{{prompt}}".to_string());
                                new_terminal_stdin.set(String::new());
                                new_terminal_cwd.set(String::new());
                                new_terminal_env.set(String::new());
                                new_terminal_timeout.set("300".to_string());
                                new_terminal_include_system_prompt.set(true);
                                new_terminal_profile.set("codex".to_string());
                                new_terminal_mode.set("one_shot".to_string());
                                new_terminal_session_scope.set("per_session".to_string());
                                new_terminal_io_protocol.set("plain_text".to_string());
                                new_terminal_execution.set("native_terminal".to_string());
                                new_terminal_approval_bridge.set("disabled".to_string());
                                new_terminal_workspace_mode.set("shared".to_string());
                                new_terminal_workspace_base.set(String::new());
                                new_terminal_workspace_cleanup_policy.set("per_session".to_string());
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
                    } else if let Some(ref error) = agents_error {
                        p {
                            style: "padding:16px;color:var(--danger);font-size:14px;line-height:1.5;",
                            "Failed to load agents: {error}"
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
                                                        // Status indicator dot
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
                                                        span { class: "list-item__title", "{agent.name}" }
                                                        if is_builtin_default_agent(agent) {
                                                            span {
                                                                class: "agent-default-badge",
                                                                "Default"
                                                            }
                                                        }
                                                    }
                                                }
                                                if entry_has_terminal_delegate(agent) {
                                                    {
                                                        let command = agent
                                                            .terminal
                                                            .as_ref()
                                                            .map(|terminal| &terminal.delegate)
                                                            .and_then(|config| config.command.as_deref())
                                                            .unwrap_or("terminal");
                                                        rsx! {
                                                            div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;", "Terminal Agent: {command}" }
                                                        }
                                                    }
                                                } else {
                                                    {
                                                        let model = native_model_for_entry(agent);
                                                        if !model.trim().is_empty() {
                                                            rsx! {
                                                                div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;", "{model}" }
                                                            }
                                                        } else {
                                                            rsx! {}
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
            }

            // ── Right panel: detail / create (~70%) ──
            div { class: "split-view__detail",
                button {
                    class: "split-view__back",
                    onclick: move |_| show_detail.set(false),
                    ArrowLeft { size: 14 }
                    " Back"
                }
                if show_create() {
                    AgentCreateForm {
                        agents: agents.clone(),
                        ws: ws.clone(),
                        refresh_tick,
                        show_create,
                        new_id,
                        new_name,
                        new_agent_kind,
                        new_provider,
                        new_model,
                        new_prompt,
                        new_group_activation,
                        new_group_keywords,
                        new_agent_aliases,
                        new_ingest_policy,
                        new_external_bot_policy,
                        new_idle_reply_enabled,
                        new_idle_reply_delay,
                        new_idle_reply_max_per_hour,
                        new_idle_reply_prompt,
                        new_terminal_enabled,
                        new_terminal_command,
                        new_terminal_args,
                        new_terminal_stdin,
                        new_terminal_cwd,
                        new_terminal_env,
                        new_terminal_timeout,
                        new_terminal_include_system_prompt,
                        new_terminal_profile,
                        new_terminal_mode,
                        new_terminal_session_scope,
                        new_terminal_io_protocol,
                        new_terminal_execution,
                        new_terminal_approval_bridge,
                        new_terminal_workspace_mode,
                        new_terminal_workspace_base,
                        new_terminal_workspace_cleanup_policy,
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
                        div { class: "empty-state",
                            div { class: "empty-state__icon",
                                Bot { size: 24 }
                            }
                            p { class: "empty-state__message",
                                "Select an agent or create a new one"
                            }
                            button {
                                class: "empty-state__action",
                                onclick: move |_| {
                                    show_create.set(true);
                                    show_settings.set(false);
                                    show_detail.set(true);
                                    selected_agent.set(None);
                                    new_id.set(String::new());
                                    new_name.set(String::new());
                                    new_agent_kind.set("native".to_string());
                                    new_provider.set(String::new());
                                    new_model.set(String::new());
                                    new_prompt.set(String::new());
                                    new_group_activation.set(String::new());
                                    new_group_keywords.set(String::new());
                                    new_agent_aliases.set(String::new());
                                    new_ingest_policy.set(String::new());
                                    new_external_bot_policy.set(String::new());
                                    new_idle_reply_enabled.set(false);
                                    new_idle_reply_delay.set("180".to_string());
                                    new_idle_reply_max_per_hour.set("1".to_string());
                                    new_idle_reply_prompt.set(String::new());
                                    new_terminal_enabled.set(false);
                                    new_terminal_command.set(String::new());
                                    new_terminal_args.set("{{prompt}}".to_string());
                                    new_terminal_stdin.set(String::new());
                                    new_terminal_cwd.set(String::new());
                                    new_terminal_env.set(String::new());
                                    new_terminal_timeout.set("300".to_string());
                                    new_terminal_include_system_prompt.set(true);
                                    new_terminal_profile.set("codex".to_string());
                                    new_terminal_mode.set("one_shot".to_string());
                                    new_terminal_session_scope.set("per_session".to_string());
                                    new_terminal_io_protocol.set("plain_text".to_string());
                                    new_terminal_execution.set("native_terminal".to_string());
                                    new_terminal_approval_bridge.set("disabled".to_string());
                                    new_terminal_workspace_mode.set("shared".to_string());
                                    new_terminal_workspace_base.set(String::new());
                                    new_terminal_workspace_cleanup_policy.set("per_session".to_string());
                                },
                                "Create Agent"
                            }
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
    mut new_agent_kind: Signal<String>,
    mut new_provider: Signal<String>,
    mut new_model: Signal<String>,
    mut new_prompt: Signal<String>,
    mut new_group_activation: Signal<String>,
    mut new_group_keywords: Signal<String>,
    mut new_agent_aliases: Signal<String>,
    mut new_ingest_policy: Signal<String>,
    mut new_external_bot_policy: Signal<String>,
    mut new_idle_reply_enabled: Signal<bool>,
    mut new_idle_reply_delay: Signal<String>,
    mut new_idle_reply_max_per_hour: Signal<String>,
    mut new_idle_reply_prompt: Signal<String>,
    mut new_terminal_enabled: Signal<bool>,
    mut new_terminal_command: Signal<String>,
    mut new_terminal_args: Signal<String>,
    mut new_terminal_stdin: Signal<String>,
    mut new_terminal_cwd: Signal<String>,
    mut new_terminal_env: Signal<String>,
    mut new_terminal_timeout: Signal<String>,
    mut new_terminal_include_system_prompt: Signal<bool>,
    mut new_terminal_profile: Signal<String>,
    mut new_terminal_mode: Signal<String>,
    mut new_terminal_session_scope: Signal<String>,
    mut new_terminal_io_protocol: Signal<String>,
    mut new_terminal_execution: Signal<String>,
    mut new_terminal_approval_bridge: Signal<String>,
    mut new_terminal_workspace_mode: Signal<String>,
    mut new_terminal_workspace_base: Signal<String>,
    mut new_terminal_workspace_cleanup_policy: Signal<String>,
) -> Element {
    let mut toaster = use_context::<Toaster>();
    let ws_connected = use_context::<Signal<bool>>();

    // Fetch configured models so we can derive available providers.
    let ws_models = ws.clone();
    let models_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_models.clone();
        async move {
            ws.call::<AvailableModelsResponse>("models.list", None)
                .await
                .map(|r| r.models)
                .unwrap_or_default()
        }
    });
    // Derive available providers + per-provider model options. Recomputed
    // only when the configured models resource changes.
    let provider_data = use_memo(move || {
        let models: Vec<AvailableModel> = models_data.read().as_ref().cloned().unwrap_or_default();
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

        let providers: Vec<String> = set.into_iter().collect();
        (providers, normalized)
    });
    let providers = provider_data.read().0.clone();
    let selected_provider = new_provider();
    let is_terminal_agent = new_agent_kind() == "terminal";
    let model_options = provider_data
        .read()
        .1
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

            // Template selection cards
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
                    label { class: "{LABEL}", "Agent Type" }
                    div { style: "display:flex;gap:8px;flex-wrap:wrap;",
                        button {
                            class: if is_terminal_agent { TOOL_BTN } else { "tool-btn tool-btn--primary" },
                            onclick: move |_| {
                                new_agent_kind.set("native".to_string());
                                new_terminal_enabled.set(false);
                            },
                            "Native Agent"
                        }
                        button {
                            class: if is_terminal_agent { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                            onclick: move |_| {
                                new_agent_kind.set("terminal".to_string());
                                new_terminal_enabled.set(true);
                                if new_terminal_profile() == "claude" {
                                    new_terminal_command.set("claude".to_string());
                                    new_terminal_args.set("-p\n{{prompt}}".to_string());
                                } else {
                                    new_terminal_profile.set("codex".to_string());
                                    new_terminal_command.set("codex".to_string());
                                    new_terminal_args.set("exec\n{{prompt}}".to_string());
                                }
                            },
                            "Terminal Agent"
                        }
                    }
                }
                if !is_terminal_agent {
                    div {
                        label { class: "{LABEL}", "Model Provider" }
                        select {
                            value: "{new_provider}",
                            onchange: move |e| {
                                new_provider.set(e.value());
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
                }
                if is_terminal_agent {
                div { class: "{SECTION_CARD}", style: "padding:12px;margin-top:4px;",
                    h4 { class: "{SECTION_TITLE}", "Terminal Agent Runtime" }
                    div { style: "display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;",
                        button {
                            class: if new_terminal_profile() == "claude" { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                            onclick: move |_| {
                                new_terminal_enabled.set(true);
                                new_terminal_command.set("claude".to_string());
                                new_terminal_args.set("-p\n{{prompt}}".to_string());
                                new_terminal_stdin.set(String::new());
                                new_terminal_profile.set("claude".to_string());
                                new_terminal_mode.set("one_shot".to_string());
                                new_terminal_session_scope.set("per_session".to_string());
                                new_terminal_io_protocol.set("plain_text".to_string());
                                new_terminal_execution.set("native_terminal".to_string());
                                new_terminal_approval_bridge.set("disabled".to_string());
                            },
                            "Claude"
                        }
                        button {
                            class: if new_terminal_profile() == "codex" { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                            onclick: move |_| {
                                new_terminal_enabled.set(true);
                                new_terminal_command.set("codex".to_string());
                                new_terminal_args.set("exec\n{{prompt}}".to_string());
                                new_terminal_stdin.set(String::new());
                                new_terminal_profile.set("codex".to_string());
                                new_terminal_mode.set("one_shot".to_string());
                                new_terminal_session_scope.set("per_session".to_string());
                                new_terminal_io_protocol.set("plain_text".to_string());
                                new_terminal_execution.set("native_terminal".to_string());
                                new_terminal_approval_bridge.set("disabled".to_string());
                            },
                            "Codex"
                        }
                    }
                    div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;margin-top:12px;",
                        div {
                            label { class: "{LABEL}", "Runtime" }
                            select {
                                value: "{new_terminal_profile}",
                                onchange: move |e| {
                                    let value = e.value();
                                    if value == "claude" {
                                        new_terminal_command.set("claude".to_string());
                                        new_terminal_args.set("-p\n{{prompt}}".to_string());
                                    } else {
                                        new_terminal_command.set("codex".to_string());
                                        new_terminal_args.set("exec\n{{prompt}}".to_string());
                                    }
                                    new_terminal_profile.set(value);
                                },
                                class: "{INPUT}",
                                option { value: "codex", "Codex" }
                                option { value: "claude", "Claude" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Mode" }
                            select {
                                value: "{new_terminal_mode}",
                                onchange: move |e| new_terminal_mode.set(e.value()),
                                class: "{INPUT}",
                                option { value: "one_shot", "One-shot" }
                                option { value: "interactive_launch", "Interactive Launch" }
                                option { value: "managed_pty", "Managed PTY" }
                                option { value: "jsonl_adapter", "JSONL Adapter" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Session Scope" }
                            select {
                                value: "{new_terminal_session_scope}",
                                onchange: move |e| new_terminal_session_scope.set(e.value()),
                                class: "{INPUT}",
                                option { value: "per_session", "Per session" }
                                option { value: "per_turn", "Per turn" }
                                option { value: "per_agent", "Per agent" }
                                option { value: "manual", "Manual" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "I/O Protocol" }
                            select {
                                value: "{new_terminal_io_protocol}",
                                onchange: move |e| new_terminal_io_protocol.set(e.value()),
                                class: "{INPUT}",
                                option { value: "plain_text", "Plain Text" }
                                option { value: "jsonl", "JSONL" }
                                option { value: "profile", "Profile Default" }
                                option { value: "sentinel", "Sentinel" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Terminal Execution" }
                            select {
                                value: "{new_terminal_execution}",
                                onchange: move |e| new_terminal_execution.set(e.value()),
                                class: "{INPUT}",
                                option { value: "native_terminal", "Native terminal" }
                                option { value: "managed_workspace", "Managed workspace" }
                                option { value: "disabled", "Disabled" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Approval Bridge" }
                            select {
                                value: "{new_terminal_approval_bridge}",
                                onchange: move |e| new_terminal_approval_bridge.set(e.value()),
                                class: "{INPUT}",
                                option { value: "disabled", "Disabled" }
                                option { value: "prompt", "Prompt" }
                                option { value: "required", "Required" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Workspace Mode" }
                            select {
                                value: "{new_terminal_workspace_mode}",
                                onchange: move |e| new_terminal_workspace_mode.set(e.value()),
                                class: "{INPUT}",
                                option { value: "shared", "Shared" }
                                option { value: "read_only", "Read only" }
                                option { value: "worktree", "Worktree" }
                                option { value: "patch_only", "Patch only" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Workspace Cleanup" }
                            select {
                                value: "{new_terminal_workspace_cleanup_policy}",
                                onchange: move |e| new_terminal_workspace_cleanup_policy.set(e.value()),
                                class: "{INPUT}",
                                option { value: "per_session", "Per session" }
                                option { value: "per_turn", "Per turn" }
                                option { value: "manual", "Manual" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Workspace Base" }
                            input {
                                value: "{new_terminal_workspace_base}",
                                oninput: move |e| new_terminal_workspace_base.set(e.value()),
                                placeholder: "Use runtime default",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Command" }
                            input {
                                value: "{new_terminal_command}",
                                oninput: move |e| new_terminal_command.set(e.value()),
                                placeholder: "claude",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Timeout (seconds)" }
                            input {
                                value: "{new_terminal_timeout}",
                                oninput: move |e| new_terminal_timeout.set(e.value()),
                                placeholder: "300",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Working Directory" }
                            input {
                                value: "{new_terminal_cwd}",
                                oninput: move |e| new_terminal_cwd.set(e.value()),
                                placeholder: "Use Savfox cwd",
                                class: "{INPUT}",
                            }
                        }
                    }
                    p { style: "font-size:12px;color:var(--danger);margin:12px 0 0 0;line-height:1.5;",
                        "Native terminal delegates run the vendor CLI directly. Vendor approval prompts, file writes, and network access are not intercepted step by step by Savfox."
                    }
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Arguments" }
                        textarea {
                            value: "{new_terminal_args}",
                            oninput: move |e| new_terminal_args.set(e.value()),
                            placeholder: "One argument per line; use {{prompt}}",
                            rows: 4,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Stdin Template" }
                        textarea {
                            value: "{new_terminal_stdin}",
                            oninput: move |e| new_terminal_stdin.set(e.value()),
                            placeholder: "Optional; use {{prompt}} to pass the request via stdin",
                            rows: 3,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Environment" }
                        textarea {
                            value: "{new_terminal_env}",
                            oninput: move |e| new_terminal_env.set(e.value()),
                            placeholder: "KEY=value",
                            rows: 3,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                    div { style: "margin-top:12px;",
                        ToggleSwitch {
                            label: "Include system prompt".to_string(),
                            description: Some("Prepend this agent's instructions to {{prompt}} before invoking the CLI.".to_string()),
                            checked: new_terminal_include_system_prompt(),
                            on_toggle: move |value| new_terminal_include_system_prompt.set(value),
                        }
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
                div { class: "{SECTION_CARD}", style: "padding:12px;margin-top:4px;",
                    h4 { class: "{SECTION_TITLE}", "Chat Trigger Policy" }
                    p { style: "font-size:12px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                        "Optional overrides for how this agent reacts in shared rooms."
                    }
                    div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;",
                        div {
                            label { class: "{LABEL}", "Group Activation" }
                            select {
                                value: "{new_group_activation}",
                                onchange: move |e| new_group_activation.set(e.value()),
                                class: "{INPUT}",
                                option { value: "", "Use runtime default" }
                                option { value: "mention", "Mention only" }
                                option { value: "keyword", "Mention or keyword" }
                                option { value: "always", "Always reply" }
                                option { value: "command", "Commands only" }
                                option { value: "off", "Never reply in groups" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Ingest Policy" }
                            select {
                                value: "{new_ingest_policy}",
                                onchange: move |e| new_ingest_policy.set(e.value()),
                                class: "{INPUT}",
                                option { value: "", "Default runtime behavior" }
                                option { value: "none", "Ignore non-replies" }
                                option { value: "reply_only", "Only keep replied messages" }
                                option { value: "targeted_only", "Only keep targeted messages" }
                                option { value: "all_human_messages", "Keep all human messages" }
                                option { value: "all_non_bot_messages", "Keep all non-bot messages" }
                                option { value: "all_messages", "Keep all messages" }
                            }
                        }
                        div {
                            label { class: "{LABEL}", "External Bot Policy" }
                            select {
                                value: "{new_external_bot_policy}",
                                onchange: move |e| new_external_bot_policy.set(e.value()),
                                class: "{INPUT}",
                                option { value: "", "Default (ignore)" }
                                option { value: "ignore", "Ignore" }
                                option { value: "ingest_only", "Ingest only" }
                                option { value: "reply_allowed", "Allow reply" }
                            }
                        }
                    }
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Group Keywords" }
                        textarea {
                            value: "{new_group_keywords}",
                            oninput: move |e| new_group_keywords.set(e.value()),
                            placeholder: "One keyword per line, or comma-separated",
                            rows: 3,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                    div { style: "margin-top:12px;",
                        label { class: "{LABEL}", "Agent Aliases" }
                        textarea {
                            value: "{new_agent_aliases}",
                            oninput: move |e| new_agent_aliases.set(e.value()),
                            placeholder: "reviewer\ncode-reviewer",
                            rows: 3,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                    div { style: "margin-top:12px;padding-top:12px;border-top:1px solid var(--border);",
                        ToggleSwitch {
                            label: "Idle Fallback Reply".to_string(),
                            description: Some("If a buffered room message stays quiet for a while, let this agent step in once.".to_string()),
                            checked: new_idle_reply_enabled(),
                            on_toggle: move |value| new_idle_reply_enabled.set(value),
                        }
                    }
                    div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;margin-top:12px;",
                        div {
                            label { class: "{LABEL}", "Idle Delay (seconds)" }
                            input {
                                value: "{new_idle_reply_delay}",
                                oninput: move |e| new_idle_reply_delay.set(e.value()),
                                placeholder: "180",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Idle Max / Hour" }
                            input {
                                value: "{new_idle_reply_max_per_hour}",
                                oninput: move |e| new_idle_reply_max_per_hour.set(e.value()),
                                placeholder: "1",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Idle Prompt Override" }
                            textarea {
                                value: "{new_idle_reply_prompt}",
                                oninput: move |e| new_idle_reply_prompt.set(e.value()),
                                placeholder: "Optional custom instruction used when the delayed fallback fires",
                                rows: 3,
                                class: "{INPUT}",
                                style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                            }
                        }
                    }
                }
                div { style: "display:flex;gap:8px;",
                    button {
                        onclick: move |_| {
                            let id = new_id().trim().to_string();
                            let name = new_name().trim().to_string();
                            let agent_kind = if new_agent_kind() == "terminal" {
                                "terminal".to_string()
                            } else {
                                "native".to_string()
                            };
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
                            let group_activation = new_group_activation().trim().to_string();
                            let group_keywords = normalize_multiline_list(&new_group_keywords());
                            let agent_aliases = normalize_multiline_list(&new_agent_aliases());
                            let ingest_policy = new_ingest_policy().trim().to_string();
                            let external_bot_policy =
                                new_external_bot_policy().trim().to_string();
                            let idle_reply_enabled = new_idle_reply_enabled();
                            let idle_reply_delay = new_idle_reply_delay();
                            let idle_reply_max_per_hour = new_idle_reply_max_per_hour();
                            let idle_reply_prompt = new_idle_reply_prompt();
                            let terminal_enabled = agent_kind == "terminal";
                            let terminal_command = new_terminal_command();
                            let terminal_args = new_terminal_args();
                            let terminal_stdin = new_terminal_stdin();
                            let terminal_cwd = new_terminal_cwd();
                            let terminal_env = new_terminal_env();
                            let terminal_timeout = new_terminal_timeout();
                            let terminal_include_system_prompt =
                                new_terminal_include_system_prompt();
                            let terminal_profile = new_terminal_profile();
                            let terminal_mode = new_terminal_mode();
                            let terminal_session_scope = new_terminal_session_scope();
                            let terminal_io_protocol = new_terminal_io_protocol();
                            let terminal_execution = new_terminal_execution();
                            let terminal_approval_bridge = new_terminal_approval_bridge();
                            let terminal_workspace_mode = new_terminal_workspace_mode();
                            let terminal_workspace_base = new_terminal_workspace_base();
                            let terminal_workspace_cleanup_policy =
                                new_terminal_workspace_cleanup_policy();
                            if id.is_empty() {
                                toaster.error("Agent ID is required");
                                return;
                            }
                            if name.is_empty() {
                                toaster.error("Agent name is required");
                                return;
                            }
                            if agent_kind == "native" && provider.is_empty() {
                                toaster.error("Model provider is required for native agents");
                                return;
                            }
                            if agent_kind == "native" && model.is_empty() {
                                toaster.error("Model is required for native agents");
                                return;
                            }
                            if terminal_enabled
                                && !matches!(terminal_profile.as_str(), "codex" | "claude")
                            {
                                toaster.error("Terminal agent runtime must be Codex or Claude");
                                return;
                            }
                            if terminal_enabled && terminal_command.trim().is_empty() {
                                toaster.error("Terminal command is required for terminal agents");
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
                                    "kind": agent_kind.clone(),
                                    "system_prompt": prompt,
                                });
                                if agent_kind == "native" {
                                    payload["native"] = json!({
                                        "provider": provider,
                                        "model": model,
                                    });
                                }
                                if !group_activation.is_empty() {
                                    payload["group_activation"] = json!(group_activation);
                                }
                                if !group_keywords.is_empty() {
                                    payload["group_keywords"] = json!(group_keywords);
                                }
                                if !agent_aliases.is_empty() {
                                    payload["agent_aliases"] = json!(agent_aliases);
                                }
                                if !ingest_policy.is_empty() {
                                    payload["ingest_policy"] = json!(ingest_policy);
                                }
                                if !external_bot_policy.is_empty() {
                                    payload["external_bot_policy"] = json!(external_bot_policy);
                                }
                                if let Some(idle_reply) = idle_reply_payload(
                                    idle_reply_enabled,
                                    &idle_reply_delay,
                                    &idle_reply_max_per_hour,
                                    &idle_reply_prompt,
                                ) {
                                    payload["idle_reply"] = idle_reply;
                                }
                                if agent_kind == "terminal" {
                                    if let Some(mut terminal) = terminal_delegate_payload(
                                        true,
                                        &terminal_command,
                                        &terminal_args,
                                        &terminal_stdin,
                                        &terminal_cwd,
                                        &terminal_env,
                                        &terminal_timeout,
                                        terminal_include_system_prompt,
                                        "",
                                        "",
                                        &terminal_profile,
                                        &terminal_mode,
                                        &terminal_session_scope,
                                        &terminal_io_protocol,
                                        &terminal_execution,
                                        &terminal_approval_bridge,
                                        &terminal_workspace_mode,
                                        &terminal_workspace_base,
                                        &terminal_workspace_cleanup_policy,
                                    ) {
                                        terminal["runtime"] = json!(terminal_profile);
                                        payload["terminal"] = terminal;
                                    }
                                }
                                let res = ws.call::<serde_json::Value>("agents.create", Some(payload)).await;
                                match res {
                                    Ok(_) => {
                                        toaster.success("Agent created");
                                        show_create.set(false);
                                        new_id.set(String::new());
                                        new_name.set(String::new());
                                        new_agent_kind.set("native".to_string());
                                        new_provider.set(String::new());
                                        new_model.set(String::new());
                                        new_prompt.set(String::new());
                                        new_group_activation.set(String::new());
                                        new_group_keywords.set(String::new());
                                        new_agent_aliases.set(String::new());
                                        new_ingest_policy.set(String::new());
                                        new_external_bot_policy.set(String::new());
                                        new_idle_reply_enabled.set(false);
                                        new_idle_reply_delay.set("180".to_string());
                                        new_idle_reply_max_per_hour.set("1".to_string());
                                        new_idle_reply_prompt.set(String::new());
                                        new_terminal_enabled.set(false);
                                        new_terminal_command.set(String::new());
                                        new_terminal_args.set("{{prompt}}".to_string());
                                        new_terminal_stdin.set(String::new());
                                        new_terminal_cwd.set(String::new());
                                        new_terminal_env.set(String::new());
                                        new_terminal_timeout.set("300".to_string());
                                        new_terminal_include_system_prompt.set(true);
                                        new_terminal_profile.set("codex".to_string());
                                        new_terminal_mode.set("one_shot".to_string());
                                        new_terminal_session_scope.set("per_session".to_string());
                                        new_terminal_io_protocol.set("plain_text".to_string());
                                        new_terminal_execution.set("native_terminal".to_string());
                                        new_terminal_approval_bridge.set("disabled".to_string());
                                        new_terminal_workspace_mode.set("shared".to_string());
                                        new_terminal_workspace_base.set(String::new());
                                        new_terminal_workspace_cleanup_policy
                                            .set("per_session".to_string());
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
                        onclick: move |_| {
                            show_create.set(false);
                            new_agent_kind.set("native".to_string());
                            new_group_activation.set(String::new());
                            new_group_keywords.set(String::new());
                            new_agent_aliases.set(String::new());
                            new_ingest_policy.set(String::new());
                            new_external_bot_policy.set(String::new());
                            new_idle_reply_enabled.set(false);
                            new_idle_reply_delay.set("180".to_string());
                            new_idle_reply_max_per_hour.set("1".to_string());
                            new_idle_reply_prompt.set(String::new());
                            new_terminal_enabled.set(false);
                            new_terminal_command.set(String::new());
                            new_terminal_args.set("{{prompt}}".to_string());
                            new_terminal_stdin.set(String::new());
                            new_terminal_cwd.set(String::new());
                            new_terminal_env.set(String::new());
                            new_terminal_timeout.set("300".to_string());
                            new_terminal_include_system_prompt.set(true);
                            new_terminal_profile.set("codex".to_string());
                            new_terminal_mode.set("one_shot".to_string());
                            new_terminal_session_scope.set("per_session".to_string());
                            new_terminal_io_protocol.set("plain_text".to_string());
                            new_terminal_execution.set("native_terminal".to_string());
                            new_terminal_approval_bridge.set("disabled".to_string());
                            new_terminal_workspace_mode.set("shared".to_string());
                            new_terminal_workspace_base.set(String::new());
                            new_terminal_workspace_cleanup_policy.set("per_session".to_string());
                        },
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
        .unwrap_or_else(|| "Savvy fox".to_string());
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

#[component]
fn ChannelRepliesEditor(
    channel_options: Vec<ChannelConfigOption>,
    mut default_activation: Signal<String>,
    mut rows: Signal<Vec<ChannelReplyFormRow>>,
) -> Element {
    let current_rows = rows();
    let used_ids: std::collections::BTreeSet<String> = current_rows
        .iter()
        .map(|row| row.channel_id.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    let add_candidate = channel_options
        .iter()
        .find(|option| !used_ids.contains(&option.id.to_ascii_lowercase()))
        .cloned();
    let add_candidate_for_click = add_candidate.clone();

    rsx! {
        div { style: "display:flex;flex-direction:column;gap:10px;",
            div { style: "display:grid;grid-template-columns:minmax(180px,1fr) minmax(190px,240px);gap:10px;align-items:end;padding:10px 12px;border:1px solid var(--border);border-radius:var(--radius);background:var(--bg-tertiary);",
                div {
                    div { style: "font-size:13px;font-weight:600;color:var(--text-primary);", "Default route" }
                    div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;", "Used when no specific channel route override matches." }
                }
                div {
                    label { class: "{LABEL}", "Reply Mode" }
                    select {
                        value: "{default_activation}",
                        onchange: move |e| default_activation.set(e.value()),
                        class: "{INPUT}",
                        option { value: "mention", "Mention only" }
                        option { value: "keyword", "Mention or keyword" }
                        option { value: "always", "Always reply" }
                        option { value: "command", "Commands only" }
                        option { value: "off", "Never reply in groups" }
                    }
                }
            }

            for (idx, row) in current_rows.iter().cloned().enumerate() {
                {
                    let remove_idx = idx;
                    let route_idx = idx;
                    let mode_idx = idx;
                    let row_channel = row.channel_id.clone();
                    let row_mode = row.group_activation.clone();
                    let row_label = channel_options
                        .iter()
                        .find(|option| option.id == row_channel)
                        .map(channel_option_label)
                        .unwrap_or_else(|| row_channel.clone());
                    rsx! {
                        div {
                            key: "{idx}-{row_channel}",
                            style: "display:grid;grid-template-columns:minmax(180px,1fr) minmax(190px,240px) auto;gap:10px;align-items:end;padding:10px 12px;border:1px solid var(--border);border-radius:var(--radius);background:var(--bg-tertiary);",
                            div {
                                label { class: "{LABEL}", "Channel Route" }
                                select {
                                    value: "{row_channel}",
                                    onchange: move |e| {
                                        if let Some(row) = rows.write().get_mut(route_idx) {
                                            row.channel_id = e.value();
                                        }
                                    },
                                    class: "{INPUT}",
                                    if !channel_options.iter().any(|option| option.id == row_channel) {
                                        option { value: "{row_channel}", "{row_label}" }
                                    }
                                    for option in channel_options.iter().filter(|option| {
                                        option.id == row_channel
                                            || !used_ids.contains(&option.id.to_ascii_lowercase())
                                    }) {
                                        {
                                            let option_id = option.id.clone();
                                            let option_label = channel_option_label(option);
                                            rsx! {
                                                option {
                                                    key: "{option_id}",
                                                    value: "{option_id}",
                                                    "{option_label}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div {
                                label { class: "{LABEL}", "Reply Mode" }
                                select {
                                    value: "{row_mode}",
                                    onchange: move |e| {
                                        if let Some(row) = rows.write().get_mut(mode_idx) {
                                            row.group_activation = e.value();
                                        }
                                    },
                                    class: "{INPUT}",
                                    option { value: "mention", "Mention only" }
                                    option { value: "keyword", "Mention or keyword" }
                                    option { value: "always", "Always reply" }
                                    option { value: "command", "Commands only" }
                                    option { value: "off", "Never reply in groups" }
                                }
                            }
                            button {
                                class: "{TOOL_BTN} tool-btn--icon",
                                title: "Delete channel reply route",
                                aria_label: "Delete channel reply route",
                                onclick: move |_| {
                                    let mut current = rows.write();
                                    if remove_idx < current.len() {
                                        current.remove(remove_idx);
                                    }
                                },
                                X { size: 14 }
                            }
                        }
                    }
                }
            }

            div { style: "display:flex;align-items:center;gap:10px;flex-wrap:wrap;",
                button {
                    class: "{TOOL_BTN}",
                    disabled: add_candidate.is_none(),
                    onclick: move |_| {
                        if let Some(option) = add_candidate_for_click.clone() {
                            rows.write().push(ChannelReplyFormRow {
                                channel_id: option.id,
                                group_activation: "mention".to_string(),
                            });
                        }
                    },
                    "+ Add Channel Route"
                }
                if channel_options.is_empty() {
                    span { style: "font-size:12px;color:var(--text-muted);", "No saved channels are configured yet." }
                } else if add_candidate.is_none() {
                    span { style: "font-size:12px;color:var(--text-muted);", "All saved channel routes already have overrides." }
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
    let initial_agent_kind = agent_kind_value(&entry.kind).to_string();
    let mut form_agent_kind = use_signal(move || initial_agent_kind.clone());

    let initial_prompt = entry.system_prompt.clone().unwrap_or_default();
    let mut form_desc = use_signal(move || initial_prompt.clone());

    let initial_model = normalized_primary_model(&entry);
    let mut form_model = use_signal(move || initial_model.clone());
    let initial_reasoning = entry
        .native
        .as_ref()
        .and_then(|native| native.thinking.as_deref())
        .and_then(normalized_reasoning_level)
        .unwrap_or_default();
    let mut form_reasoning = use_signal(move || initial_reasoning.clone());
    let mut form_group_activation = use_signal(|| "mention".to_string());
    let mut form_channel_replies = use_signal(Vec::<ChannelReplyFormRow>::new);
    let mut form_group_keywords = use_signal(String::new);
    let mut form_agent_aliases = use_signal(String::new);
    let mut form_ingest_policy = use_signal(String::new);
    let mut form_external_bot_policy = use_signal(String::new);
    let mut form_idle_reply_enabled = use_signal(|| false);
    let mut form_idle_reply_delay = use_signal(|| "180".to_string());
    let mut form_idle_reply_max_per_hour = use_signal(|| "1".to_string());
    let mut form_idle_reply_prompt = use_signal(String::new);
    let mut form_terminal_enabled = use_signal(|| false);
    let mut form_terminal_command = use_signal(String::new);
    let mut form_terminal_args = use_signal(|| "{{prompt}}".to_string());
    let mut form_terminal_stdin = use_signal(String::new);
    let mut form_terminal_cwd = use_signal(String::new);
    let mut form_terminal_env = use_signal(String::new);
    let mut form_terminal_timeout = use_signal(|| "300".to_string());
    let mut form_terminal_include_system_prompt = use_signal(|| true);
    let mut form_terminal_interactive_command = use_signal(String::new);
    let mut form_terminal_interactive_args = use_signal(String::new);
    let mut form_terminal_profile = use_signal(|| "codex".to_string());
    let mut form_terminal_mode = use_signal(|| "one_shot".to_string());
    let mut form_terminal_session_scope = use_signal(|| "per_session".to_string());
    let mut form_terminal_io_protocol = use_signal(|| "plain_text".to_string());
    let mut form_terminal_execution = use_signal(|| "native_terminal".to_string());
    let mut form_terminal_approval_bridge = use_signal(|| "disabled".to_string());
    let mut form_terminal_workspace_mode = use_signal(|| "shared".to_string());
    let mut form_terminal_workspace_base = use_signal(String::new);
    let mut form_terminal_workspace_cleanup_policy = use_signal(|| "per_session".to_string());
    let mut launching_terminal = use_signal(|| false);
    let mut terminal_launch_result = use_signal(|| Option::<String>::None);
    let mut checking_terminal_health = use_signal(|| false);
    let mut terminal_health_result = use_signal(|| Option::<(bool, String)>::None);

    let mut form_fallbacks = use_signal(Vec::<String>::new);
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
            let raw = ws
                .call::<Value>("agents.get", Some(json!({ "id": id })))
                .await
                .ok()?;
            let detail = serde_json::from_value::<AgentDetail>(raw.clone()).ok()?;
            Some((detail, raw))
        }
    });

    let detail_snapshot = detail_data
        .read()
        .as_ref()
        .and_then(|detail| detail.as_ref().map(|(detail, _)| detail.clone()));
    let detail_raw_snapshot = detail_data
        .read()
        .as_ref()
        .and_then(|detail| detail.as_ref().map(|(_, raw)| raw.clone()));
    let detail_terminal_delegate_raw = detail_raw_snapshot
        .as_ref()
        .and_then(|detail| detail.get("terminal"));
    if !detail_seeded() {
        if let Some(ref detail) = detail_snapshot {
            let detail_agent_kind =
                if detail.kind.as_str() == "terminal" || detail.terminal.is_some() {
                    "terminal"
                } else {
                    "native"
                };
            form_agent_kind.set(detail_agent_kind.to_string());
            let detail_model = detail
                .native
                .as_ref()
                .map(|native| native.model.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let fallbacks = detail
                .native
                .as_ref()
                .and_then(|native| native.fallback_models.clone())
                .unwrap_or_default();
            if form_model().trim().is_empty() {
                if let Some(detail_model) = detail_model {
                    form_model.set(detail_model);
                }
            }
            form_fallbacks.set(fallbacks);
            form_matrix_channels.set(
                detail
                    .matrix_auto_user_channels
                    .as_ref()
                    .map(|ids| ids.iter().cloned().collect())
                    .unwrap_or_default(),
            );
            form_reasoning.set(
                detail
                    .native
                    .as_ref()
                    .and_then(|native| native.thinking.as_deref())
                    .and_then(normalized_reasoning_level)
                    .unwrap_or_default(),
            );
            form_group_activation.set(
                detail
                    .group_activation
                    .clone()
                    .unwrap_or_else(|| "mention".to_string()),
            );
            form_channel_replies.set(json_channel_reply_rows(
                detail_raw_snapshot
                    .as_ref()
                    .and_then(|detail| detail.get("channel_replies")),
            ));
            form_group_keywords.set(multiline_list_value(detail.group_keywords.as_ref()));
            form_agent_aliases.set(multiline_list_value(detail.agent_aliases.as_ref()));
            form_ingest_policy.set(detail.ingest_policy.clone().unwrap_or_default());
            form_external_bot_policy.set(detail.external_bot_policy.clone().unwrap_or_default());
            let (idle_enabled, idle_delay, idle_max_per_hour, idle_prompt) =
                detail_idle_reply_fields(detail.idle_reply.as_ref());
            form_idle_reply_enabled.set(idle_enabled);
            form_idle_reply_delay.set(idle_delay);
            form_idle_reply_max_per_hour.set(idle_max_per_hour);
            form_idle_reply_prompt.set(idle_prompt);
            let (
                terminal_enabled,
                terminal_command,
                terminal_args,
                terminal_stdin,
                terminal_cwd,
                terminal_env,
                terminal_timeout,
                terminal_include_system_prompt,
            ) = detail_terminal_delegate_fields(terminal_delegate_for_detail(detail));
            form_terminal_enabled.set(terminal_enabled);
            form_terminal_command.set(terminal_command);
            form_terminal_args.set(terminal_args);
            form_terminal_stdin.set(terminal_stdin);
            form_terminal_cwd.set(terminal_cwd);
            form_terminal_env.set(terminal_env);
            form_terminal_timeout.set(terminal_timeout);
            form_terminal_include_system_prompt.set(terminal_include_system_prompt);
            let (interactive_command, interactive_args) =
                detail_terminal_interactive_fields(terminal_delegate_for_detail(detail));
            form_terminal_interactive_command.set(interactive_command);
            form_terminal_interactive_args.set(interactive_args);
            let (profile, mode, session_scope, io_protocol) =
                detail_terminal_runtime_fields(terminal_delegate_for_detail(detail));
            form_terminal_profile.set(profile);
            form_terminal_mode.set(mode);
            form_terminal_session_scope.set(session_scope);
            form_terminal_io_protocol.set(io_protocol);
            let (terminal_execution, terminal_approval_bridge) =
                json_terminal_permission_fields(detail_terminal_delegate_raw);
            form_terminal_execution.set(terminal_execution);
            form_terminal_approval_bridge.set(terminal_approval_bridge);
            let (workspace_mode, workspace_base, workspace_cleanup_policy) =
                json_terminal_workspace_fields(detail_terminal_delegate_raw);
            form_terminal_workspace_mode.set(workspace_mode);
            form_terminal_workspace_base.set(workspace_base);
            form_terminal_workspace_cleanup_policy.set(workspace_cleanup_policy);
            detail_seeded.set(true);
        }
    }

    // Fetch channel configs for Channel Replies and Matrix Identity sections.
    let ws_channels = ws.clone();
    let channel_configs_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_channels.clone();
        async move {
            ws.call::<serde_json::Value>("channels.config.list", None)
                .await
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        }
    });
    let channel_configs = channel_configs_data
        .read()
        .as_ref()
        .cloned()
        .unwrap_or_default();
    let channel_options = channel_config_options(&channel_configs);

    // Fetch available models for dropdown
    let ws_models = ws.clone();
    let models_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_models.clone();
        async move {
            ws.call::<AvailableModelsResponse>("models.list", None)
                .await
                .map(|r| r.models)
                .unwrap_or_default()
        }
    });
    let models: Vec<AvailableModel> = models_data.read().as_ref().cloned().unwrap_or_default();
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
        let orig_agent_kind = detail_snapshot
            .as_ref()
            .map(|detail| {
                if detail.kind.as_str() == "terminal" || detail.terminal.is_some() {
                    "terminal".to_string()
                } else {
                    "native".to_string()
                }
            })
            .unwrap_or_else(|| agent_kind_value(&entry.kind).to_string());
        let orig_model = normalized_primary_model(&entry);
        let orig_desc = entry.system_prompt.clone().unwrap_or_default();
        let current_model = model_select_value(&models, &form_model());
        let original_model = model_select_value(&models, &orig_model);
        let orig_reasoning = detail_snapshot
            .as_ref()
            .and_then(|detail| {
                detail
                    .native
                    .as_ref()
                    .and_then(|native| native.thinking.as_deref())
            })
            .and_then(normalized_reasoning_level)
            .or_else(|| {
                entry
                    .native
                    .as_ref()
                    .and_then(|native| native.thinking.as_deref())
                    .and_then(normalized_reasoning_level)
            })
            .unwrap_or_default();
        let orig_fallbacks: Vec<String> = detail_snapshot
            .as_ref()
            .and_then(|detail| {
                detail
                    .native
                    .as_ref()
                    .and_then(|native| native.fallback_models.clone())
            })
            .unwrap_or_default();
        let orig_group_activation = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.group_activation.clone())
            .unwrap_or_else(|| "mention".to_string());
        let orig_channel_replies = json_channel_reply_rows(
            detail_raw_snapshot
                .as_ref()
                .and_then(|detail| detail.get("channel_replies")),
        );
        let orig_group_keywords = detail_snapshot
            .as_ref()
            .map(|detail| multiline_list_value(detail.group_keywords.as_ref()))
            .unwrap_or_default();
        let orig_agent_aliases = detail_snapshot
            .as_ref()
            .map(|detail| multiline_list_value(detail.agent_aliases.as_ref()))
            .unwrap_or_default();
        let orig_ingest_policy = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.ingest_policy.clone())
            .unwrap_or_default();
        let orig_external_bot_policy = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.external_bot_policy.clone())
            .unwrap_or_default();
        let (
            orig_idle_reply_enabled,
            orig_idle_reply_delay,
            orig_idle_reply_max_per_hour,
            orig_idle_reply_prompt,
        ) = detail_idle_reply_fields(
            detail_snapshot
                .as_ref()
                .and_then(|detail| detail.idle_reply.as_ref()),
        );
        let (
            orig_terminal_enabled,
            orig_terminal_command,
            orig_terminal_args,
            orig_terminal_stdin,
            orig_terminal_cwd,
            orig_terminal_env,
            orig_terminal_timeout,
            orig_terminal_include_system_prompt,
        ) = detail_terminal_delegate_fields(
            detail_snapshot
                .as_ref()
                .and_then(terminal_delegate_for_detail),
        );
        let (orig_terminal_interactive_command, orig_terminal_interactive_args) =
            detail_terminal_interactive_fields(
                detail_snapshot
                    .as_ref()
                    .and_then(terminal_delegate_for_detail),
            );
        let (
            orig_terminal_profile,
            orig_terminal_mode,
            orig_terminal_session_scope,
            orig_terminal_io_protocol,
        ) = detail_terminal_runtime_fields(
            detail_snapshot
                .as_ref()
                .and_then(terminal_delegate_for_detail),
        );
        let (orig_terminal_execution, orig_terminal_approval_bridge) =
            json_terminal_permission_fields(detail_terminal_delegate_raw);
        let (
            orig_terminal_workspace_mode,
            orig_terminal_workspace_base,
            orig_terminal_workspace_cleanup_policy,
        ) = json_terminal_workspace_fields(detail_terminal_delegate_raw);
        let orig_matrix_channels: std::collections::HashSet<String> = detail_snapshot
            .as_ref()
            .and_then(|detail| detail.matrix_auto_user_channels.as_ref())
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default();

        form_name() != orig_name
            || form_agent_kind() != orig_agent_kind
            || current_model != original_model
            || form_desc() != orig_desc
            || form_reasoning() != orig_reasoning
            || form_group_activation() != orig_group_activation
            || form_channel_replies() != orig_channel_replies
            || form_group_keywords() != orig_group_keywords
            || form_agent_aliases() != orig_agent_aliases
            || form_ingest_policy() != orig_ingest_policy
            || form_external_bot_policy() != orig_external_bot_policy
            || form_idle_reply_enabled() != orig_idle_reply_enabled
            || form_idle_reply_delay() != orig_idle_reply_delay
            || form_idle_reply_max_per_hour() != orig_idle_reply_max_per_hour
            || form_idle_reply_prompt() != orig_idle_reply_prompt
            || form_terminal_enabled() != orig_terminal_enabled
            || form_terminal_command() != orig_terminal_command
            || form_terminal_args() != orig_terminal_args
            || form_terminal_stdin() != orig_terminal_stdin
            || form_terminal_cwd() != orig_terminal_cwd
            || form_terminal_env() != orig_terminal_env
            || form_terminal_timeout() != orig_terminal_timeout
            || form_terminal_include_system_prompt() != orig_terminal_include_system_prompt
            || form_terminal_interactive_command() != orig_terminal_interactive_command
            || form_terminal_interactive_args() != orig_terminal_interactive_args
            || form_terminal_profile() != orig_terminal_profile
            || form_terminal_mode() != orig_terminal_mode
            || form_terminal_session_scope() != orig_terminal_session_scope
            || form_terminal_io_protocol() != orig_terminal_io_protocol
            || form_terminal_execution() != orig_terminal_execution
            || form_terminal_approval_bridge() != orig_terminal_approval_bridge
            || form_terminal_workspace_mode() != orig_terminal_workspace_mode
            || form_terminal_workspace_base() != orig_terminal_workspace_base
            || form_terminal_workspace_cleanup_policy() != orig_terminal_workspace_cleanup_policy
            || *form_fallbacks.read() != orig_fallbacks
            || *form_matrix_channels.read() != orig_matrix_channels
    };
    let (launch_summary_command, launch_summary_cwd, launch_summary_env) =
        terminal_launch_config_summary(
            &form_terminal_command(),
            &form_terminal_interactive_command(),
            &form_terminal_cwd(),
            &form_terminal_env(),
        );
    let form_is_terminal_agent = form_agent_kind() == "terminal";

    let entry_id = agent_id.clone();
    let ws_save = ws.clone();

    // Clone button state
    let ws_clone = ws.clone();
    let clone_entry = entry.clone();
    let clone_detail = detail_snapshot.clone();
    let clone_detail_raw = detail_raw_snapshot.clone();
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
                                let detail_raw = clone_detail_raw.clone();
                                let agents = clone_agents.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let entry = entry.clone();
                                    let detail = detail.clone();
                                    let detail_raw = detail_raw.clone();
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

                                        let prompt_val = entry.system_prompt.clone().unwrap_or_default();
                                        let agent_kind = if entry.kind.as_str() == "terminal"
                                            || entry.terminal.is_some()
                                        {
                                            "terminal"
                                        } else {
                                            "native"
                                        };

                                        let mut payload = json!({
                                            "id": clone_id,
                                            "name": clone_name,
                                            "kind": agent_kind,
                                            "system_prompt": prompt_val,
                                        });
                                        if agent_kind == "terminal" {
                                            if let Some(terminal) = detail_raw
                                                .as_ref()
                                                .and_then(|detail| detail.get("terminal"))
                                            {
                                                payload["terminal"] = terminal.clone();
                                            } else if let Some(ref terminal) = entry.terminal {
                                                payload["terminal"] = json!(terminal);
                                            }
                                        } else if let Some(ref detail) = detail {
                                            if let Some(ref native) = detail.native {
                                                payload["native"] = json!(native);
                                            }
                                            if payload.get("native").is_none()
                                                && let Some(ref native) = entry.native
                                            {
                                                payload["native"] = json!(native);
                                            }
                                        } else if let Some(ref native) = entry.native {
                                            payload["native"] = json!(native);
                                        }
                                        // Include fallback models from detail if available
                                        if let Some(ref detail) = detail {
                                            if let Some(ref group_activation) = detail.group_activation {
                                                payload["group_activation"] = json!(group_activation);
                                            }
                                            if let Some(ref channel_replies) = detail.channel_replies
                                                && !channel_replies.is_empty()
                                            {
                                                payload["channel_replies"] = json!(channel_replies);
                                            }
                                            if let Some(ref group_keywords) = detail.group_keywords
                                                && !group_keywords.is_empty()
                                            {
                                                payload["group_keywords"] = json!(group_keywords);
                                            }
                                            if let Some(ref agent_aliases) = detail.agent_aliases
                                                && !agent_aliases.is_empty()
                                            {
                                                payload["agent_aliases"] = json!(agent_aliases);
                                            }
                                            if let Some(ref ingest_policy) = detail.ingest_policy {
                                                payload["ingest_policy"] = json!(ingest_policy);
                                            }
                                            if let Some(ref external_bot_policy) = detail.external_bot_policy {
                                                payload["external_bot_policy"] = json!(external_bot_policy);
                                            }
                                            if let Some(ref idle_reply) = detail.idle_reply {
                                                payload["idle_reply"] = json!(idle_reply);
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
                    // Export agent as JSON
                    button {
                        onclick: {
                            let export_entry = entry.clone();
                            let export_detail = detail_snapshot.clone();
                            let export_detail_raw = detail_raw_snapshot.clone();
                            move |_| {
                                let agent_kind = if export_entry.kind.as_str() == "terminal"
                                    || export_entry.terminal.is_some()
                                {
                                    "terminal"
                                } else {
                                    "native"
                                };
                                let mut export_data = json!({
                                    "name": export_entry.name,
                                    "kind": agent_kind,
                                });
                                if agent_kind == "terminal" {
                                    if let Some(terminal) = export_detail_raw
                                        .as_ref()
                                        .and_then(|detail| detail.get("terminal"))
                                    {
                                        export_data["terminal"] = terminal.clone();
                                    } else if let Some(ref terminal) = export_entry.terminal {
                                        export_data["terminal"] = json!(terminal);
                                    }
                                } else if let Some(ref detail) = export_detail {
                                    if let Some(ref native) = detail.native {
                                        export_data["native"] = json!(native);
                                    }
                                    if export_data.get("native").is_none()
                                        && let Some(ref native) = export_entry.native
                                    {
                                        export_data["native"] = json!(native);
                                    }
                                } else if let Some(ref native) = export_entry.native {
                                    export_data["native"] = json!(native);
                                }
                                if let Some(ref prompt) = export_entry.system_prompt {
                                    export_data["system_prompt"] = json!(prompt);
                                }
                                if let Some(ref detail) = export_detail {
                                    if let Some(ref group_activation) = detail.group_activation {
                                        export_data["group_activation"] = json!(group_activation);
                                    }
                                    if let Some(ref channel_replies) = detail.channel_replies
                                        && !channel_replies.is_empty()
                                    {
                                        export_data["channel_replies"] = json!(channel_replies);
                                    }
                                    if let Some(ref group_keywords) = detail.group_keywords
                                        && !group_keywords.is_empty()
                                    {
                                        export_data["group_keywords"] = json!(group_keywords);
                                    }
                                    if let Some(ref agent_aliases) = detail.agent_aliases
                                        && !agent_aliases.is_empty()
                                    {
                                        export_data["agent_aliases"] = json!(agent_aliases);
                                    }
                                    if let Some(ref ingest_policy) = detail.ingest_policy {
                                        export_data["ingest_policy"] = json!(ingest_policy);
                                    }
                                    if let Some(ref external_bot_policy) = detail.external_bot_policy {
                                        export_data["external_bot_policy"] = json!(external_bot_policy);
                                    }
                                    if let Some(ref idle_reply) = detail.idle_reply {
                                        export_data["idle_reply"] = json!(idle_reply);
                                    }
                                    if let Some(ref pp) = detail.permission_policy {
                                        if let Some(tool_access) = pp.tool_access.as_ref()
                                        {
                                            export_data["tools"] = json!(tool_access.allowed);
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

            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Agent Type" }
                div { style: "display:flex;gap:8px;flex-wrap:wrap;",
                    button {
                        class: if form_is_terminal_agent { TOOL_BTN } else { "tool-btn tool-btn--primary" },
                        onclick: move |_| {
                            form_agent_kind.set("native".to_string());
                            form_terminal_enabled.set(false);
                        },
                        "Native Agent"
                    }
                    button {
                        class: if form_is_terminal_agent { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                        onclick: move |_| {
                            form_agent_kind.set("terminal".to_string());
                            form_terminal_enabled.set(true);
                            if form_terminal_profile() == "claude" {
                                form_terminal_command.set("claude".to_string());
                                form_terminal_args.set("-p\n{{prompt}}".to_string());
                                form_terminal_interactive_command.set("claude".to_string());
                            } else {
                                form_terminal_profile.set("codex".to_string());
                                form_terminal_command.set("codex".to_string());
                                form_terminal_args.set("exec\n{{prompt}}".to_string());
                                form_terminal_interactive_command.set("codex".to_string());
                            }
                        },
                        "Terminal Agent"
                    }
                }
            }

            // Primary model selector
            if !form_is_terminal_agent {
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Primary Model" }
                div { class: "agent-model-selector",
                    ModelSelector {
                        value: form_model(),
                        on_change: move |model: String| form_model.set(model),
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
            }

            if form_is_terminal_agent {
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Terminal Agent Runtime" }
                div { style: "display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;",
                    button {
                        class: if form_terminal_profile() == "claude" { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                        onclick: move |_| {
                            form_terminal_enabled.set(true);
                            form_terminal_command.set("claude".to_string());
                            form_terminal_args.set("-p\n{{prompt}}".to_string());
                            form_terminal_stdin.set(String::new());
                            form_terminal_interactive_command.set("claude".to_string());
                            form_terminal_interactive_args.set(String::new());
                            form_terminal_profile.set("claude".to_string());
                            form_terminal_mode.set("one_shot".to_string());
                            form_terminal_session_scope.set("per_session".to_string());
                            form_terminal_io_protocol.set("plain_text".to_string());
                            form_terminal_execution.set("native_terminal".to_string());
                            form_terminal_approval_bridge.set("disabled".to_string());
                        },
                        "Claude"
                    }
                    button {
                        class: if form_terminal_profile() == "codex" { "tool-btn tool-btn--primary" } else { TOOL_BTN },
                        onclick: move |_| {
                            form_terminal_enabled.set(true);
                            form_terminal_command.set("codex".to_string());
                            form_terminal_args.set("exec\n{{prompt}}".to_string());
                            form_terminal_stdin.set(String::new());
                            form_terminal_interactive_command.set("codex".to_string());
                            form_terminal_interactive_args.set(String::new());
                            form_terminal_profile.set("codex".to_string());
                            form_terminal_mode.set("one_shot".to_string());
                            form_terminal_session_scope.set("per_session".to_string());
                            form_terminal_io_protocol.set("plain_text".to_string());
                            form_terminal_execution.set("native_terminal".to_string());
                            form_terminal_approval_bridge.set("disabled".to_string());
                        },
                        "Codex"
                    }
                }
                div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;margin-top:12px;",
                    div {
                        label { class: "{LABEL}", "Runtime" }
                        select {
                            value: "{form_terminal_profile}",
                            onchange: move |e| {
                                let value = e.value();
                                if value == "claude" {
                                    form_terminal_command.set("claude".to_string());
                                    form_terminal_args.set("-p\n{{prompt}}".to_string());
                                    form_terminal_interactive_command.set("claude".to_string());
                                } else {
                                    form_terminal_command.set("codex".to_string());
                                    form_terminal_args.set("exec\n{{prompt}}".to_string());
                                    form_terminal_interactive_command.set("codex".to_string());
                                }
                                form_terminal_profile.set(value);
                            },
                            class: "{INPUT}",
                            option { value: "codex", "Codex" }
                            option { value: "claude", "Claude" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Mode" }
                        select {
                            value: "{form_terminal_mode}",
                            onchange: move |e| form_terminal_mode.set(e.value()),
                            class: "{INPUT}",
                            option { value: "one_shot", "One-shot" }
                            option { value: "interactive_launch", "Interactive Launch" }
                            option { value: "managed_pty", "Managed PTY" }
                            option { value: "jsonl_adapter", "JSONL Adapter" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Session Scope" }
                        select {
                            value: "{form_terminal_session_scope}",
                            onchange: move |e| form_terminal_session_scope.set(e.value()),
                            class: "{INPUT}",
                            option { value: "per_session", "Per session" }
                            option { value: "per_turn", "Per turn" }
                            option { value: "per_agent", "Per agent" }
                            option { value: "manual", "Manual" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "I/O Protocol" }
                        select {
                            value: "{form_terminal_io_protocol}",
                            onchange: move |e| form_terminal_io_protocol.set(e.value()),
                            class: "{INPUT}",
                            option { value: "plain_text", "Plain Text" }
                            option { value: "jsonl", "JSONL" }
                            option { value: "profile", "Profile Default" }
                            option { value: "sentinel", "Sentinel" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Terminal Execution" }
                        select {
                            value: "{form_terminal_execution}",
                            onchange: move |e| form_terminal_execution.set(e.value()),
                            class: "{INPUT}",
                            option { value: "native_terminal", "Native terminal" }
                            option { value: "managed_workspace", "Managed workspace" }
                            option { value: "disabled", "Disabled" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Approval Bridge" }
                        select {
                            value: "{form_terminal_approval_bridge}",
                            onchange: move |e| form_terminal_approval_bridge.set(e.value()),
                            class: "{INPUT}",
                            option { value: "disabled", "Disabled" }
                            option { value: "prompt", "Prompt" }
                            option { value: "required", "Required" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Workspace Mode" }
                        select {
                            value: "{form_terminal_workspace_mode}",
                            onchange: move |e| form_terminal_workspace_mode.set(e.value()),
                            class: "{INPUT}",
                            option { value: "shared", "Shared" }
                            option { value: "read_only", "Read only" }
                            option { value: "worktree", "Worktree" }
                            option { value: "patch_only", "Patch only" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Workspace Cleanup" }
                        select {
                            value: "{form_terminal_workspace_cleanup_policy}",
                            onchange: move |e| form_terminal_workspace_cleanup_policy.set(e.value()),
                            class: "{INPUT}",
                            option { value: "per_session", "Per session" }
                            option { value: "per_turn", "Per turn" }
                            option { value: "manual", "Manual" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Workspace Base" }
                        input {
                            value: "{form_terminal_workspace_base}",
                            oninput: move |e| form_terminal_workspace_base.set(e.value()),
                            placeholder: "Use runtime default",
                            class: "{INPUT}",
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Command" }
                        input {
                            value: "{form_terminal_command}",
                            oninput: move |e| form_terminal_command.set(e.value()),
                            placeholder: "claude",
                            class: "{INPUT}",
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Timeout (seconds)" }
                        input {
                            value: "{form_terminal_timeout}",
                            oninput: move |e| form_terminal_timeout.set(e.value()),
                            placeholder: "300",
                            class: "{INPUT}",
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Working Directory" }
                        input {
                            value: "{form_terminal_cwd}",
                            oninput: move |e| form_terminal_cwd.set(e.value()),
                            placeholder: "Use Savfox cwd",
                            class: "{INPUT}",
                        }
                    }
                }
                p { style: "font-size:12px;color:var(--danger);margin:12px 0 0 0;line-height:1.5;",
                    "Native terminal delegates run the vendor CLI directly. Vendor approval prompts, file writes, and network access are not intercepted step by step by Savfox."
                }
                div { style: "margin-top:12px;",
                    label { class: "{LABEL}", "Arguments" }
                    textarea {
                        value: "{form_terminal_args}",
                        oninput: move |e| form_terminal_args.set(e.value()),
                        placeholder: "One argument per line; use {{prompt}}",
                        rows: 4,
                        class: "{INPUT}",
                        style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                    }
                }
                div { style: "margin-top:12px;",
                    label { class: "{LABEL}", "Stdin Template" }
                    textarea {
                        value: "{form_terminal_stdin}",
                        oninput: move |e| form_terminal_stdin.set(e.value()),
                        placeholder: "Optional; use {{prompt}} to pass the request via stdin",
                        rows: 3,
                        class: "{INPUT}",
                        style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                    }
                }
                div { style: "margin-top:12px;",
                    label { class: "{LABEL}", "Environment" }
                    textarea {
                        value: "{form_terminal_env}",
                        oninput: move |e| form_terminal_env.set(e.value()),
                        placeholder: "KEY=value",
                        rows: 3,
                        class: "{INPUT}",
                        style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                    }
                }
                div { style: "margin-top:12px;",
                    ToggleSwitch {
                        label: "Include system prompt".to_string(),
                        description: Some("Prepend this agent's instructions to {{prompt}} before invoking the CLI.".to_string()),
                        checked: form_terminal_include_system_prompt(),
                        on_toggle: move |value| form_terminal_include_system_prompt.set(value),
                    }
                }

                // ── Interactive launcher ──────────────────────────────────
                div {
                    style: "margin-top:18px;padding-top:16px;border-top:1px solid var(--border);",
                    h5 {
                        style: "margin:0 0 6px 0;font-size:13px;font-weight:600;color:var(--text-primary);",
                        "Interactive Launch"
                    }
                    p {
                        style: "margin:0 0 12px 0;font-size:12px;color:var(--text-muted);line-height:1.5;",
                        "Open the configured CLI in a system terminal window so you can interact with it directly. Login flows, TUIs, and multi-turn input all run in the OS terminal — Savfox just spawns it. Useful for tools like ",
                        code { "codex" }
                        ", ",
                        code { "claude" }
                        ", or any CLI that needs your keyboard."
                    }
                    div {
                        style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:8px;margin-bottom:12px;",
                        div { style: "padding:8px 10px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                            div { style: "font-size:11px;color:var(--text-muted);margin-bottom:2px;", "Command" }
                            div { style: "font-family:var(--font-mono);font-size:12px;color:var(--text-primary);word-break:break-word;", "{launch_summary_command}" }
                        }
                        div { style: "padding:8px 10px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                            div { style: "font-size:11px;color:var(--text-muted);margin-bottom:2px;", "CWD" }
                            div { style: "font-family:var(--font-mono);font-size:12px;color:var(--text-primary);word-break:break-word;", "{launch_summary_cwd}" }
                        }
                        div { style: "padding:8px 10px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                            div { style: "font-size:11px;color:var(--text-muted);margin-bottom:2px;", "Env" }
                            div { style: "font-family:var(--font-mono);font-size:12px;color:var(--text-primary);word-break:break-word;", "{launch_summary_env}" }
                        }
                    }
                    div { style: "display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin-bottom:12px;",
                        button {
                            class: "{TOOL_BTN}",
                            disabled: checking_terminal_health(),
                            onclick: {
                                let ws = ws.clone();
                                let agent_id = agent_id.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let agent_id = agent_id.clone();
                                    checking_terminal_health.set(true);
                                    terminal_health_result.set(None);
                                    spawn(async move {
                                        let res = ws
                                            .call::<serde_json::Value>(
                                                "agent.terminal.health",
                                                Some(json!({ "agent": agent_id })),
                                            )
                                            .await;
                                        checking_terminal_health.set(false);
                                        match res {
                                            Ok(value) => {
                                                let summary = terminal_health_message(&value);
                                                if summary.0 {
                                                    toaster.success(summary.1.clone());
                                                } else {
                                                    toaster.error(summary.1.clone());
                                                }
                                                terminal_health_result.set(Some(summary));
                                            }
                                            Err(err) => {
                                                let msg = format!("Health check failed: {err}");
                                                toaster.error(msg.clone());
                                                terminal_health_result.set(Some((false, msg)));
                                            }
                                        }
                                    });
                                }
                            },
                            if checking_terminal_health() { "Checking..." } else { "Health Check" }
                        }
                        button {
                            class: "{TOOL_BTN}",
                            disabled: launching_terminal(),
                            onclick: {
                                let ws = ws.clone();
                                let agent_id = agent_id.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let agent_id = agent_id.clone();
                                    launching_terminal.set(true);
                                    spawn(async move {
                                        let res = ws
                                            .call::<serde_json::Value>(
                                                "agent.terminal.launch",
                                                Some(json!({ "agent": agent_id })),
                                            )
                                            .await;
                                        launching_terminal.set(false);
                                        match res {
                                            Ok(value) => {
                                                let summary = terminal_launch_result_summary(&value);
                                                toaster.success(format!("Launched: {summary}"));
                                                terminal_launch_result.set(Some(summary));
                                            }
                                            Err(err) => {
                                                let msg = format!("Launch failed: {err}");
                                                terminal_launch_result.set(Some(msg.clone()));
                                                toaster.error(msg);
                                            }
                                        }
                                    });
                                }
                            },
                            Terminal { size: 14 }
                            if launching_terminal() { "Launching…" } else { "Launch Terminal" }
                        }
                        span {
                            style: "font-size:11px;color:var(--text-muted);",
                            "Save the agent first to apply config changes."
                        }
                    }
                    if let Some((ok, ref message)) = terminal_health_result() {
                        div {
                            style: if ok { "font-size:12px;color:var(--success);margin-bottom:12px;" } else { "font-size:12px;color:var(--danger);margin-bottom:12px;" },
                            "{message}"
                        }
                    }
                    if let Some(ref message) = terminal_launch_result() {
                        div {
                            style: "font-size:12px;color:var(--text-muted);margin-bottom:12px;",
                            "{message}"
                        }
                    }
                    div { style: "display:grid;grid-template-columns:1fr;gap:12px;",
                        div {
                            label { class: "{LABEL}", "Interactive Command (override)" }
                            input {
                                value: "{form_terminal_interactive_command}",
                                oninput: move |e| form_terminal_interactive_command.set(e.value()),
                                placeholder: "Leave blank to reuse Command",
                                class: "{INPUT}",
                            }
                        }
                        div {
                            label { class: "{LABEL}", "Interactive Args" }
                            textarea {
                                value: "{form_terminal_interactive_args}",
                                oninput: move |e| form_terminal_interactive_args.set(e.value()),
                                placeholder: "One argument per line. Leave blank for a bare TUI launch (e.g. `codex`).",
                                rows: 3,
                                class: "{INPUT}",
                                style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                            }
                        }
                    }
                }
            }
            }

            // Model fallback list
            if !form_is_terminal_agent {
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Model Fallbacks" }
                p { style: "font-size:11px;color:var(--text-muted);margin-bottom:8px;",
                    "Fallback models tried in order when the primary model is unavailable."
                }
                {
                    let fallbacks = form_fallbacks();
                    rsx! {
                        div { class: "agent-fallback-list",
                            for (i, fb) in fallbacks.iter().enumerate() {
                                div { key: "fb-{i}", class: "agent-fallback-item",
                                    span { class: "agent-fallback-index", "#{i}" }
                                    div { class: "agent-model-selector agent-fallback-selector",
                                        ModelSelector {
                                            value: fb.clone(),
                                            on_change: move |model: String| {
                                                let mut list = form_fallbacks.write();
                                                if i < list.len() {
                                                    list[i] = model;
                                                }
                                            },
                                        }
                                    }
                                    button {
                                        class: "agent-fallback-remove",
                                        title: "Remove fallback",
                                        onclick: move |_| {
                                            let mut list = form_fallbacks.write();
                                            if i < list.len() {
                                                list.remove(i);
                                            }
                                        },
                                        X { size: 14 }
                                    }
                                }
                            }
                            button {
                                class: "agent-fallback-add",
                                onclick: move |_| {
                                    form_fallbacks.write().push("default".to_string());
                                },
                                "+ Add Fallback"
                            }
                        }
                    }
                }
            }
            }

            // Matrix Identity (appservice auto-user)
            {
                let appservice_channels = matrix_appservice_channels(&channel_configs);
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

            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Channel Replies" }
                p { style: "font-size:12px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                    "Set how this agent replies for the default route, then add overrides for specific saved channel routes."
                }
                ChannelRepliesEditor {
                    channel_options: channel_options.clone(),
                    default_activation: form_group_activation,
                    rows: form_channel_replies,
                }
            }

            // Chat trigger policy
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Chat Trigger Policy" }
                p { style: "font-size:12px;color:var(--text-muted);margin-bottom:12px;line-height:1.5;",
                    "Control which passive messages are buffered into ambient context, and which aliases explicitly target this agent."
                }
                div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;",
                    div {
                        label { class: "{LABEL}", "Ingest Policy" }
                        select {
                            value: "{form_ingest_policy}",
                            onchange: move |e| form_ingest_policy.set(e.value()),
                            class: "{INPUT}",
                            option { value: "", "Default runtime behavior" }
                            option { value: "none", "Ignore non-replies" }
                            option { value: "reply_only", "Only keep replied messages" }
                            option { value: "targeted_only", "Only keep targeted messages" }
                            option { value: "all_human_messages", "Keep all human messages" }
                            option { value: "all_non_bot_messages", "Keep all non-bot messages" }
                            option { value: "all_messages", "Keep all messages" }
                        }
                    }
                    div {
                        label { class: "{LABEL}", "External Bot Policy" }
                        select {
                            value: "{form_external_bot_policy}",
                            onchange: move |e| form_external_bot_policy.set(e.value()),
                            class: "{INPUT}",
                            option { value: "", "Default (ignore)" }
                            option { value: "ignore", "Ignore" }
                            option { value: "ingest_only", "Ingest only" }
                            option { value: "reply_allowed", "Allow reply" }
                        }
                    }
                }
                div { style: "margin-top:12px;",
                    label { class: "{LABEL}", "Group Keywords" }
                    textarea {
                        value: "{form_group_keywords}",
                        oninput: move |e| form_group_keywords.set(e.value()),
                        placeholder: "One keyword per line, or comma-separated",
                        rows: 4,
                        class: "{INPUT}",
                        style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                    }
                    p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                        "Used when group activation is set to keyword mode."
                    }
                }
                div { style: "margin-top:12px;",
                    label { class: "{LABEL}", "Agent Aliases" }
                    textarea {
                        value: "{form_agent_aliases}",
                        oninput: move |e| form_agent_aliases.set(e.value()),
                        placeholder: "reviewer\ncode-reviewer",
                        rows: 4,
                        class: "{INPUT}",
                        style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                    }
                    p { style: "font-size:11px;color:var(--text-muted);margin-top:4px;",
                        "Leading targets like 'reviewer: inspect this' will route to this agent."
                    }
                }
                div { style: "margin-top:12px;padding-top:12px;border-top:1px solid var(--border);",
                    ToggleSwitch {
                        label: "Idle Fallback Reply".to_string(),
                        description: Some("Let this agent step in once if a buffered room message stays quiet for a while.".to_string()),
                        checked: form_idle_reply_enabled(),
                        on_toggle: move |value| form_idle_reply_enabled.set(value),
                    }
                }
                div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;align-items:start;margin-top:12px;",
                    div {
                        label { class: "{LABEL}", "Idle Delay (seconds)" }
                        input {
                            value: "{form_idle_reply_delay}",
                            oninput: move |e| form_idle_reply_delay.set(e.value()),
                            placeholder: "180",
                            class: "{INPUT}",
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Idle Max / Hour" }
                        input {
                            value: "{form_idle_reply_max_per_hour}",
                            oninput: move |e| form_idle_reply_max_per_hour.set(e.value()),
                            placeholder: "1",
                            class: "{INPUT}",
                        }
                    }
                    div {
                        label { class: "{LABEL}", "Idle Prompt Override" }
                        textarea {
                            value: "{form_idle_reply_prompt}",
                            oninput: move |e| form_idle_reply_prompt.set(e.value()),
                            placeholder: "Optional custom instruction used when the delayed fallback fires",
                            rows: 3,
                            class: "{INPUT}",
                            style: "resize:vertical;font-family:var(--font-mono);font-size:13px;line-height:1.5;",
                        }
                    }
                }
            }

            // Test connectivity
            if !form_is_terminal_agent {
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
                            let agent_kind_val = if form_agent_kind() == "terminal" {
                                "terminal".to_string()
                            } else {
                                "native".to_string()
                            };
                            let model_val = selected_model_value.clone();
                            let desc_val = form_desc();
                            let reasoning_val = form_reasoning();
                            let group_activation_val = form_group_activation();
                            let channel_replies_val = form_channel_replies();
                            let group_keywords_val = form_group_keywords();
                            let agent_aliases_val = form_agent_aliases();
                            let ingest_policy_val = form_ingest_policy();
                            let external_bot_policy_val = form_external_bot_policy();
                            let idle_reply_enabled_val = form_idle_reply_enabled();
                            let idle_reply_delay_val = form_idle_reply_delay();
                            let idle_reply_max_per_hour_val = form_idle_reply_max_per_hour();
                            let idle_reply_prompt_val = form_idle_reply_prompt();
                            let terminal_enabled_val = agent_kind_val == "terminal";
                            let terminal_command_val = form_terminal_command();
                            let terminal_args_val = form_terminal_args();
                            let terminal_stdin_val = form_terminal_stdin();
                            let terminal_cwd_val = form_terminal_cwd();
                            let terminal_env_val = form_terminal_env();
                            let terminal_timeout_val = form_terminal_timeout();
                            let terminal_include_system_prompt_val =
                                form_terminal_include_system_prompt();
                            let terminal_interactive_command_val =
                                form_terminal_interactive_command();
                            let terminal_interactive_args_val =
                                form_terminal_interactive_args();
                            let terminal_profile_val = form_terminal_profile();
                            let terminal_mode_val = form_terminal_mode();
                            let terminal_session_scope_val = form_terminal_session_scope();
                            let terminal_io_protocol_val = form_terminal_io_protocol();
                            let terminal_execution_val = form_terminal_execution();
                            let terminal_approval_bridge_val = form_terminal_approval_bridge();
                            let terminal_workspace_mode_val = form_terminal_workspace_mode();
                            let terminal_workspace_base_val = form_terminal_workspace_base();
                            let terminal_workspace_cleanup_policy_val =
                                form_terminal_workspace_cleanup_policy();
                            let fallback_list: Vec<String> = form_fallbacks()
                                .iter()
                                .filter(|s| !s.trim().is_empty() && *s != "default")
                                .cloned()
                                .collect();
                            if agent_kind_val == "native" && model_val.trim().is_empty() {
                                toaster.error("Primary model is required for native agents");
                                return;
                            }
                            if terminal_enabled_val
                                && !matches!(terminal_profile_val.as_str(), "codex" | "claude")
                            {
                                toaster.error("Terminal agent runtime must be Codex or Claude");
                                return;
                            }
                            if terminal_enabled_val && terminal_command_val.trim().is_empty() {
                                toaster.error("Terminal command is required for terminal agents");
                                return;
                            }
                            spawn(async move {
                                let keyword_list = normalize_multiline_list(&group_keywords_val);
                                let alias_list = normalize_multiline_list(&agent_aliases_val);
                                let mut params = json!({
                                    "id": id,
                                    "name": name,
                                    "kind": agent_kind_val.clone(),
                                    "system_prompt": desc_val,
                                });
                                if agent_kind_val == "native" {
                                    let mut native = json!({
                                        "provider": native_provider_for_model(&model_val),
                                        "model": model_val.clone(),
                                    });
                                    if !fallback_list.is_empty() {
                                        native["fallback_models"] = json!(fallback_list.clone());
                                    }
                                    if !reasoning_val.trim().is_empty() {
                                        native["thinking"] = json!(reasoning_val);
                                    }
                                    params["native"] = native;
                                }
                                params["group_activation"] = if group_activation_val.trim().is_empty()
                                {
                                    serde_json::Value::Null
                                } else {
                                    json!(group_activation_val)
                                };
                                params["channel_replies"] =
                                    channel_replies_payload(&channel_replies_val)
                                        .unwrap_or(serde_json::Value::Null);
                                params["group_keywords"] = if keyword_list.is_empty() {
                                    serde_json::Value::Null
                                } else {
                                    json!(keyword_list)
                                };
                                params["agent_aliases"] = if alias_list.is_empty() {
                                    serde_json::Value::Null
                                } else {
                                    json!(alias_list)
                                };
                                params["ingest_policy"] = if ingest_policy_val.trim().is_empty() {
                                    serde_json::Value::Null
                                } else {
                                    json!(ingest_policy_val)
                                };
                                params["external_bot_policy"] =
                                    if external_bot_policy_val.trim().is_empty() {
                                        serde_json::Value::Null
                                    } else {
                                        json!(external_bot_policy_val)
                                    };
                                params["idle_reply"] = if let Some(idle_reply) = idle_reply_payload(
                                    idle_reply_enabled_val,
                                    &idle_reply_delay_val,
                                    &idle_reply_max_per_hour_val,
                                    &idle_reply_prompt_val,
                                ) {
                                    idle_reply
                                } else {
                                    serde_json::Value::Null
                                };
                                if agent_kind_val == "terminal"
                                    && let Some(mut terminal) = terminal_delegate_payload(
                                    true,
                                    &terminal_command_val,
                                    &terminal_args_val,
                                    &terminal_stdin_val,
                                    &terminal_cwd_val,
                                    &terminal_env_val,
                                    &terminal_timeout_val,
                                    terminal_include_system_prompt_val,
                                    &terminal_interactive_command_val,
                                    &terminal_interactive_args_val,
                                    &terminal_profile_val,
                                    &terminal_mode_val,
                                    &terminal_session_scope_val,
                                    &terminal_io_protocol_val,
                                    &terminal_execution_val,
                                    &terminal_approval_bridge_val,
                                    &terminal_workspace_mode_val,
                                    &terminal_workspace_base_val,
                                    &terminal_workspace_cleanup_policy_val,
                                ) {
                                    terminal["runtime"] = json!(terminal_profile_val);
                                    params["terminal"] = terminal;
                                }
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

/// Static tool catalog. Built once (the literal contains no runtime data) and
/// cloned per call so callers keep their owned `Vec<ToolCategory>`.
static TOOL_CATEGORIES: std::sync::LazyLock<Vec<ToolCategory>> = std::sync::LazyLock::new(|| {
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
});

fn tool_categories() -> Vec<ToolCategory> {
    TOOL_CATEGORIES.clone()
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
    let mut simulation_command = use_signal(String::new);
    let mut simulation_result = use_signal(|| None::<serde_json::Value>);
    let mut simulation_pending = use_signal(|| false);
    let mut rule_command = use_signal(String::new);
    let mut rule_decision = use_signal(|| "allow".to_owned());
    let mut security_refresh = use_signal(|| 0_u64);

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
    let rules_ws = ws.clone();
    let rules_data = use_resource(move || {
        let _c = ws_connected();
        let _r = security_refresh();
        let ws = rules_ws.clone();
        async move {
            ws.call::<serde_json::Value>("security.rules.list", None)
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
            let prof = pp.preset_id.as_deref().unwrap_or("coding").to_string();
            let tools = pp
                .tool_access
                .as_ref()
                .map(|tool_access| tool_access.allowed.clone())
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

    let effective_security = tools_data
        .read()
        .as_ref()
        .and_then(|detail| detail.as_ref())
        .and_then(|detail| detail.effective_security.clone());

    rsx! {
        div { style: "padding:16px;display:flex;flex-direction:column;gap:16px;max-width:720px;",
            if let Some(security) = effective_security {
                div { class: "{SECTION_CARD}",
                    h4 { class: "{SECTION_TITLE}", "Effective Enforcement" }
                    div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;",
                        div {
                            div { style: "font-size:11px;color:var(--text-muted);", "Execution" }
                            div { style: "font-size:13px;font-weight:600;", "{security.execution_mode}" }
                        }
                        div {
                            div { style: "font-size:11px;color:var(--text-muted);", "Sandbox Policy" }
                            div { style: "font-size:13px;font-weight:600;", "{security.sandbox_policy}" }
                        }
                        div {
                            div { style: "font-size:11px;color:var(--text-muted);", "Backend" }
                            div { style: "font-size:13px;font-weight:600;", "{security.sandbox_enforcement}" }
                        }
                    }
                    if let Some(reason) = security.fallback_reason {
                        div {
                            style: "margin-top:10px;padding:8px 10px;border-radius:6px;background:rgba(245,158,11,.12);color:#b45309;font-size:12px;",
                            "{reason}"
                        }
                    }
                }
            }
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Policy Simulator" }
                div { style: "display:flex;gap:8px;align-items:center;",
                    input {
                        style: "flex:1;min-width:0;",
                        placeholder: "Enter a command without running it",
                        value: "{simulation_command}",
                        oninput: move |event| simulation_command.set(event.value()),
                    }
                    button {
                        class: "{TOOL_BTN}",
                        disabled: simulation_pending(),
                        onclick: {
                            let ws = ws.clone();
                            let agent = agent_id.clone();
                            move |_| {
                                let ws = ws.clone();
                                let agent = agent.clone();
                                let command = simulation_command();
                                if command.trim().is_empty() {
                                    return;
                                }
                                simulation_pending.set(true);
                                spawn(async move {
                                    match ws.call::<serde_json::Value>(
                                        "security.policy.simulate",
                                        Some(json!({ "agent": agent, "command": command })),
                                    ).await {
                                        Ok(value) => simulation_result.set(Some(value)),
                                        Err(error) => toaster.error(format!("Simulation failed: {error}")),
                                    }
                                    simulation_pending.set(false);
                                });
                            }
                        },
                        if simulation_pending() { "Checking..." } else { "Simulate" }
                    }
                }
                if let Some(result) = simulation_result() {
                    div {
                        style: "margin-top:10px;padding:10px;border-radius:6px;background:var(--surface-muted);font-size:12px;",
                        div { style: "font-weight:700;", "Decision: {result.get(\"decision\").and_then(|v| v.as_str()).unwrap_or(\"unknown\")}" }
                        div { "Mode: {result.get(\"execution_mode\").and_then(|v| v.as_str()).unwrap_or(\"unknown\")} · Sandbox: {result.get(\"sandbox\").and_then(|v| v.as_str()).unwrap_or(\"unknown\")}" }
                        if let Some(reason) = result.get("reason").and_then(|v| v.as_str()) {
                            div { style: "margin-top:4px;color:var(--text-muted);", "{reason}" }
                        }
                    }
                }
            }
            div { class: "{SECTION_CARD}",
                h4 { class: "{SECTION_TITLE}", "Core Execpolicy Rules" }
                div { style: "display:flex;gap:8px;align-items:center;margin-bottom:10px;",
                    input {
                        style: "flex:1;min-width:0;",
                        placeholder: "Command prefix, e.g. git status",
                        value: "{rule_command}",
                        oninput: move |event| rule_command.set(event.value()),
                    }
                    select {
                        value: "{rule_decision}",
                        onchange: move |event| rule_decision.set(event.value()),
                        option { value: "allow", "Allow" }
                        option { value: "ask", "Ask" }
                        option { value: "deny", "Deny" }
                    }
                    button {
                        class: "{TOOL_BTN}",
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                let ws = ws.clone();
                                let command = rule_command();
                                let decision = rule_decision();
                                if command.trim().is_empty() {
                                    return;
                                }
                                spawn(async move {
                                    match ws.call::<serde_json::Value>(
                                        "security.rules.add",
                                        Some(json!({ "command": command, "decision": decision })),
                                    ).await {
                                        Ok(_) => {
                                            rule_command.set(String::new());
                                            security_refresh += 1;
                                        }
                                        Err(error) => toaster.error(format!("Could not add rule: {error}")),
                                    }
                                });
                            }
                        },
                        "Add"
                    }
                }
                if let Some(Some(data)) = rules_data.read().as_ref() {
                    div { style: "display:flex;flex-direction:column;gap:6px;",
                        for rule in data.get("rules").and_then(|value| value.as_array()).cloned().unwrap_or_default() {
                            {
                                let command = rule.get("command").cloned().unwrap_or(serde_json::Value::Null);
                                let decision = rule.get("decision").and_then(|value| value.as_str()).unwrap_or("unknown").to_owned();
                                let rule_text = rule.get("rule").and_then(|value| value.as_str()).unwrap_or("").to_owned();
                                let removable = command.is_array() && decision != "unknown";
                                rsx! {
                                    div { style: "display:flex;align-items:center;gap:8px;padding:7px 8px;border-radius:6px;background:var(--surface-muted);",
                                        code { style: "flex:1;font-size:11px;overflow-wrap:anywhere;", "{rule_text}" }
                                        if removable {
                                            button {
                                                class: "{TOOL_BTN} tool-btn--sm",
                                                onclick: {
                                                    let ws = ws.clone();
                                                    let command = command.clone();
                                                    let decision = decision.clone();
                                                    move |_| {
                                                        let ws = ws.clone();
                                                        let command = command.clone();
                                                        let decision = decision.clone();
                                                        spawn(async move {
                                                            match ws.call::<serde_json::Value>(
                                                                "security.rules.remove",
                                                                Some(json!({ "command": command, "decision": decision })),
                                                            ).await {
                                                                Ok(_) => security_refresh += 1,
                                                                Err(error) => toaster.error(format!("Could not remove rule: {error}")),
                                                            }
                                                        });
                                                    }
                                                },
                                                "Remove"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
                            let (permission_policy, execution_policy) = if profile
                                .eq_ignore_ascii_case("inherit")
                            {
                                (None, None)
                            } else {
                                let mut enabled: Vec<String> = tool_states
                                    .read()
                                    .iter()
                                    .filter_map(|(k, v)| if *v { Some(k.clone()) } else { None })
                                    .collect();
                                enabled.extend(passthrough_tools.read().iter().cloned());
                                enabled.sort();
                                enabled.dedup();
                                let Some(boundary) = agent_preset_boundary(&profile) else {
                                    toaster.error(format!(
                                        "Unknown permission preset: {profile}"
                                    ));
                                    return;
                                };
                                (
                                    Some(AgentPermissionPolicy {
                                        preset_id: Some(profile),
                                        sandbox: Some(boundary.sandbox),
                                        approval: Some(boundary.approval),
                                        tool_access: Some(AgentToolAccessPolicy {
                                            allowed: enabled,
                                            ..AgentToolAccessPolicy::default()
                                        }),
                                        ..AgentPermissionPolicy::default()
                                    }),
                                    Some(AgentExecutionPolicy {
                                        mode: boundary.execution_mode,
                                    }),
                                )
                            };
                            spawn(async move {
                                let res = ws.call::<serde_json::Value>(
                                    "agents.update",
                                    Some(json!({
                                        "agent": id,
                                        "permission_policy": permission_policy,
                                        "execution_policy": execution_policy,
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
                                flock: None,
                                path: None,
                                conflicts: None,
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

    // Show all globally installed AND enabled skills (including name conflicts).
    let available_skills: Vec<SkillDetail> = all_skills
        .into_iter()
        .filter(|s| s.installed.unwrap_or(false) && s.enabled.unwrap_or(false))
        .collect();

    // Build winner map: for each skill name, determine the highest-priority category.
    // This is used as the default "selected source" for name-conflicting skills.
    let mut winner_categories: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for s in available_skills.iter() {
        let cat = s.category.as_deref().unwrap_or("");
        let rank = match cat {
            "workspace" => 0u8,
            "installed" => 1,
            "built-in" => 2,
            "extra" => 3,
            _ => 10,
        };
        let is_new_winner = winner_categories.get(&s.name).map_or(true, |existing_cat| {
            let existing_rank = match existing_cat.as_str() {
                "workspace" => 0u8,
                "installed" => 1,
                "built-in" => 2,
                "extra" => 3,
                _ => 10,
            };
            rank < existing_rank
        });
        if is_new_winner {
            winner_categories.insert(s.name.clone(), cat.to_string());
        }
    }

    // Track which names have conflicts (more than one source).
    let mut name_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for s in available_skills.iter() {
        *name_counts.entry(s.name.clone()).or_insert(0) += 1;
    }
    let conflict_names: std::collections::HashSet<String> = name_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect();

    // Track user's selected source per conflicting name. Defaults to winner (highest priority).
    let mut selected_source = use_signal(|| std::collections::HashMap::<String, String>::new());

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
        div { class: "agent-skills-panel",
            div { class: "agent-skills-panel__header",
                h4 { class: "agent-skills-panel__title", "Agent Skills" }
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
                p { class: "agent-skills-panel__empty", "No skills match your search" }
            } else {
                // Workspace group
                if !workspace.is_empty() {
                    { render_skill_group_section("Workspace", &workspace, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster, &conflict_names, &winner_categories, selected_source) }
                }
                // Built-in group
                if !builtin.is_empty() {
                    { render_skill_group_section("Built-in", &builtin, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster, &conflict_names, &winner_categories, selected_source) }
                }
                // Third-party group
                if !third_party.is_empty() {
                    { render_skill_group_section("Third-party", &third_party, &agent_enabled_skills, ws.clone(), refresh_tick, agent_id.clone(), toaster, &conflict_names, &winner_categories, selected_source) }
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
    conflict_names: &std::collections::HashSet<String>,
    winner_categories: &std::collections::HashMap<String, String>,
    mut selected_source: Signal<std::collections::HashMap<String, String>>,
) -> Element {
    rsx! {
        div { class: "{SECTION_CARD}",
            h4 { class: "{SECTION_TITLE}", "{title} ({skills.len()})" }
            div { class: "agent-skills-group",
                for skill in skills.iter() {
                    {
                        let has_conflict = conflict_names.contains(&skill.name);
                        let skill_cat = skill.category.as_deref().unwrap_or("").to_string();

                        // For conflicting skills, determine which source is "selected".
                        // User selection takes precedence, otherwise default to highest priority.
                        let is_enabled = if has_conflict {
                            let active_cat = selected_source.read()
                                .get(&skill.name)
                                .cloned()
                                .unwrap_or_else(|| winner_categories.get(&skill.name).cloned().unwrap_or_default());
                            enabled_skills.contains(&skill.name) && skill_cat == active_cat
                        } else {
                            enabled_skills.contains(&skill.name)
                        };

                        let skill_name = skill.name.clone();
                        let skill_cat_toggle = skill_cat.clone();
                        let ws_toggle = ws.clone();
                        let agent = agent_id.clone();
                        let has_conflict_toggle = has_conflict;
                        rsx! {
                            div {
                                key: "{skill.name}-{skill.category.as_deref().unwrap_or(\"\")}",
                                class: "agent-skill-row",
                                div { class: "agent-skill-row__body",
                                    div { class: "agent-skill-row__meta",
                                        span { class: "agent-skill-row__name", "{skill.name}" }
                                        if let Some(ref ver) = skill.version {
                                            span { class: "agent-skill-row__version", "v{ver}" }
                                        }
                                        if let Some(ref cat) = skill.category {
                                            span { class: "agent-skill-row__badge agent-skill-row__badge--category", "{cat}" }
                                        }
                                        if has_conflict {
                                            span {
                                                class: "agent-skill-row__badge agent-skill-row__badge--conflict",
                                                title: skill.conflicts.as_ref().map(|c| {
                                                    c.iter()
                                                        .map(|s| format!("{} ({})", s.category, s.path.as_deref().unwrap_or("?")))
                                                        .collect::<Vec<_>>()
                                                        .join(", ")
                                                }).unwrap_or_default(),
                                                "Name Conflict"
                                            }
                                        }
                                    }
                                    if let Some(ref desc) = skill.description {
                                        p { class: "agent-skill-row__desc", "{desc}" }
                                    }
                                    if let Some(ref path) = skill.path {
                                        p { class: "agent-skill-row__path", "{path}" }
                                    }
                                }
                                ToggleSwitch {
                                    label: String::new(),
                                    checked: is_enabled,
                                    on_toggle: move |enabled: bool| {
                                        let ws = ws_toggle.clone();
                                        let name = skill_name.clone();
                                        let agent = agent.clone();
                                        let cat = skill_cat_toggle.clone();
                                        spawn(async move {
                                            // For conflicting skills: enabling this source sets it
                                            // as the selected one and disables others with same name.
                                            if has_conflict_toggle && enabled {
                                                selected_source.write().insert(name.clone(), cat);
                                            }
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
    use serde_json::json;

    use super::{
        json_terminal_permission_fields, json_terminal_workspace_fields, model_option_label,
        model_select_value, terminal_delegate_payload,
    };
    use crate::api::types::AvailableModel;

    fn test_model(id: &str, name: Option<&str>, model_slug: Option<&str>) -> AvailableModel {
        AvailableModel {
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

    #[test]
    fn terminal_delegate_payload_preserves_runtime_permission_and_workspace_fields() {
        let payload = terminal_delegate_payload(
            true,
            "codex",
            "exec\n{{prompt}}",
            "",
            "D:/repo",
            "FOO=bar\nBAZ = qux\nmalformed",
            "2",
            true,
            "codex",
            "resume\n--last",
            "codex",
            "managed_pty",
            "per_session",
            "sentinel",
            "managed_workspace",
            "required",
            "worktree",
            "D:/base",
            "delete_on_success",
        )
        .expect("enabled delegate should produce payload");

        assert_eq!(payload["enabled"], json!(true));
        assert_eq!(payload["command"], json!("codex"));
        assert_eq!(payload["args"], json!(["exec", "{{prompt}}"]));
        assert_eq!(payload["cwd"], json!("D:/repo"));
        assert_eq!(payload["env"], json!({"BAZ": "qux", "FOO": "bar"}));
        assert_eq!(payload["timeout_secs"], json!(300));
        assert_eq!(payload["interactive_command"], json!("codex"));
        assert_eq!(payload["interactive_args"], json!(["resume", "--last"]));
        assert_eq!(payload["profile"], json!("codex"));
        assert_eq!(payload["mode"], json!("managed_pty"));
        assert_eq!(payload["session_scope"], json!("per_session"));
        assert_eq!(payload["io_protocol"], json!("sentinel"));
        assert_eq!(payload["terminal_execution"], json!("managed_workspace"));
        assert_eq!(payload["savfox_approval_bridge"], json!("required"));
        assert_eq!(
            payload["workspace"],
            json!({
                "mode": "worktree",
                "base": "D:/base",
                "cleanup_policy": "delete_on_success"
            })
        );
    }

    #[test]
    fn terminal_permission_fields_accept_legacy_bridge_booleans() {
        assert_eq!(
            json_terminal_permission_fields(Some(&json!({
                "savfox_approval_bridge": true
            }))),
            ("managed_workspace".to_string(), "prompt".to_string())
        );
        assert_eq!(
            json_terminal_permission_fields(Some(&json!({
                "savfox_approval_bridge": false
            }))),
            ("native_terminal".to_string(), "disabled".to_string())
        );
        assert_eq!(
            json_terminal_permission_fields(Some(&json!({
                "terminal_execution": "native_terminal",
                "savfox_approval_bridge": "required"
            }))),
            ("native_terminal".to_string(), "required".to_string())
        );
    }

    #[test]
    fn terminal_workspace_fields_default_to_shared_per_session() {
        assert_eq!(
            json_terminal_workspace_fields(Some(&json!({}))),
            (
                "shared".to_string(),
                String::new(),
                "per_session".to_string()
            )
        );
        assert_eq!(
            json_terminal_workspace_fields(Some(&json!({
                "workspace": {
                    "mode": "patch_only",
                    "base": "D:/repo",
                    "cleanup_policy": "manual"
                }
            }))),
            (
                "patch_only".to_string(),
                "D:/repo".to_string(),
                "manual".to_string()
            )
        );
    }
}
