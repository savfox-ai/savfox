#![allow(dead_code)]
use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::api::types::{
    AgentDetail, AgentEntry, AgentIdleReplyConfig, AgentTerminalDelegateConfig, AvailableModel,
};
use crate::ui::{self, AppContext};
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
    let args = if config.get("mode").and_then(Value::as_str) == Some("claude_stream") {
        serde_json::to_string_pretty(config.get("claude").unwrap_or(&json!({}))).unwrap_or_default()
    } else {
        args
    };
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
        if config.mode.as_ref().map(|mode| mode.as_str()) == Some("claude_stream") {
            serde_json::to_string_pretty(&config.claude).unwrap_or_default()
        } else {
            terminal_args_value(config.args.as_ref())
        },
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

fn codex_extra_args(raw: &str) -> Vec<String> {
    let mut args = normalize_terminal_args(raw).into_iter();
    let mut extra = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--listen" {
            let _ = args.next();
        } else if arg != "app-server" && !arg.starts_with("--listen=") {
            extra.push(arg);
        }
    }
    extra
}

fn claude_adapter_settings() -> String {
    serde_json::to_string_pretty(&savfox_gateway_shared::ClaudeCodeConfig::default())
        .unwrap_or_else(|_| "{}".to_owned())
}

fn codex_app_server_args(raw: &str) -> Vec<String> {
    let mut args = vec![
        "app-server".to_string(),
        "--listen".to_string(),
        "stdio://".to_string(),
    ];
    args.extend(codex_extra_args(raw));
    args
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

#[allow(clippy::too_many_arguments)]
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

    if profile == "claude" && mode == "claude_stream" {
        payload["claude"] = serde_json::from_str::<Value>(args_raw)
            .unwrap_or_else(|_| Value::String(args_raw.to_owned()));
        payload.as_object_mut()?.remove("args");
        payload.as_object_mut()?.remove("stdin");
        payload.as_object_mut()?.remove("io_protocol");
    }

    if profile == "codex" && mode == "app_server" {
        payload["args"] = json!(codex_app_server_args(args_raw));
        payload.as_object_mut()?.remove("stdin");
        payload.as_object_mut()?.remove("io_protocol");
        payload["terminal_execution"] = json!("native_terminal");
        payload["savfox_approval_bridge"] = json!("disabled");
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

pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "agents.list", None);
    let search = ctx.scope.cage(String::new());
    let source = state.value;
    let selected = path
        .trim_matches('/')
        .split('/')
        .nth(1)
        .unwrap_or("")
        .to_owned();
    let selected_key = selected.clone();
    let rows = ctx.scope.bond(move || {
        let filter = search.get().to_lowercase();
        array(&source.get(), &["agents"])
            .into_iter()
            .filter(|agent| agent.to_string().to_lowercase().contains(&filter))
            .collect::<Vec<_>>()
    });
    let context = ctx.clone();
    let row_state = state.clone();
    let list = col().class("split-view__list agent-list-pane").fill((
        row().class("panel-header").fill((
            text()
                .class("panel-header__title page-title")
                .kind(TextKind::Heading(1))
                .content(ui::label_for(locale, "Agents")),
            row().class("toolbar-actions").fill((
                refresh(ctx, &state),
                nav(ctx, "Create agent", "/agents/new"),
            )),
        )),
        col().class("agent-list-search").fill(ui::field(
            ui::label_for(locale, "Search agents"),
            search,
            false,
        )),
        status(ctx, &state),
        col()
            .class("agent-list-scroll")
            .fill(each(rows, record_key, move |item, _| {
                let agent = item.get_untracked();
                let id = string(&agent, &["id", "name"]);
                let target = id.clone();
                let selected = selected_key == id;
                let open_context = context.clone();
                let default_context = context.clone();
                let delete_context = context.clone();
                let default_state = row_state.clone();
                let delete_state = row_state.clone();
                let default_id = id.clone();
                let delete_id = id.clone();
                let name = string(&agent, &["name", "id"]);
                let subtitle = if agent["kind"].as_str() == Some("terminal") {
                    string(&agent["terminal"], &["runtime", "command"])
                } else {
                    let native = &agent["native"];
                    {
                        let provider = string(native, &["provider"]);
                        let model = string(native, &["model"]);
                        if provider.is_empty() || model.starts_with(&format!("{provider}/")) {
                            model
                        } else {
                            format!("{provider}/{model}")
                        }
                    }
                };
                col()
                    .class(if selected {
                        "list-item list-item--active agent-list-item"
                    } else {
                        "list-item agent-list-item"
                    })
                    .fill((
                        sapid::button()
                            .unstyled()
                            .class("agent-select")
                            .label(name)
                            .on_press(move || open_context.navigate(&format!("/agents/{target}"))),
                        text().class("list-item__subtitle").content(subtitle),
                        row().class("list-item__actions").fill((
                            ui::action(ui::label_for(locale, "Use as default agent"), move || {
                                mutate(
                                    &default_context,
                                    &default_state,
                                    "agents.update",
                                    json!({"id":default_id,"is_default":true}),
                                )
                            }),
                            ui::action(ui::label_for(locale, "Delete"), move || {
                                if confirmed("Delete this agent?") {
                                    mutate(
                                        &delete_context,
                                        &delete_state,
                                        "agents.delete",
                                        json!({"id":delete_id}),
                                    );
                                }
                            }),
                        )),
                    ))
            })),
    ));
    let detail: Box<dyn Widget> = if selected == "new" {
        agent_form(ctx, Value::Null, true)
    } else if !selected.is_empty() {
        let detail = query(ctx, "agents.get", Some(json!({"id":selected})));
        let tab = path
            .trim_matches('/')
            .split('/')
            .nth(2)
            .unwrap_or("overview")
            .to_owned();
        let agent = selected.clone();
        let context = ctx.clone();
        let data = detail.value;
        let navs = [
            ("Overview", "overview"),
            ("Files", "files"),
            ("Tools", "tools"),
            ("Skills", "skills"),
        ]
        .into_iter()
        .map(|(label, tab)| nav(ctx, label, format!("/agents/{selected}/{tab}")))
        .collect::<Vec<_>>();
        Box::new(
            col().class("agent-detail-panel").fill((
                row().class("panel-header agent-detail-header").fill((
                    text()
                        .class("page-title")
                        .kind(TextKind::Heading(1))
                        .content(selected),
                    nav(ctx, "All agents", "/agents"),
                )),
                row().class("agent-tabs actions").fill(navs),
                status(ctx, &detail),
                when(data.map(|value| !value.is_null()), move || {
                    let detail = data.get_untracked();
                    let detail = detail.get("agent").cloned().unwrap_or(detail);
                    match tab.as_str() {
                        "files" => files(&context, &agent),
                        "tools" => tools(&context, &agent, detail),
                        "skills" => skills(&context, &agent),
                        _ => agent_form(&context, detail, false),
                    }
                }),
            )),
        )
    } else {
        Box::new(
            col().class("agent-empty-detail").fill(
                col().class("empty-state agent-empty-card").fill((
                    text()
                        .class("agent-empty-icon shell-icon shell-icon--bot")
                        .content(""),
                    text()
                        .class("empty-state__message")
                        .content(ui::label_for(locale, "Select an agent or create a new one")),
                    nav(ctx, "Create agent", "/agents/new"),
                )),
            ),
        )
    };
    Box::new(
        row()
            .class(if path == "/agents" {
                "gateway-page management-page agent-workspace split-view"
            } else {
                "gateway-page management-page agent-workspace split-view split-view--detail"
            })
            .fill((list, col().class("split-view__detail").fill(detail))),
    )
}

fn agent_form(ctx: &AppContext, detail: Value, creating: bool) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let id = ctx.scope.cage(string(&detail, &["id", "name"]));
    let name = ctx.scope.cage(string(&detail, &["name"]));
    let kind = ctx
        .scope
        .cage(if detail["kind"].as_str() == Some("terminal") {
            "terminal".to_owned()
        } else {
            "native".to_owned()
        });
    let prompt = ctx.scope.cage(string(&detail, &["system_prompt"]));
    let model = ctx.scope.cage(string(&detail["native"], &["model"]));
    let provider = ctx.scope.cage(string(&detail["native"], &["provider"]));
    let fallback = ctx
        .scope
        .cage(json_string_list(detail["native"].get("fallback_models")).join("\n"));
    let thinking = ctx.scope.cage(string(&detail["native"], &["thinking"]));
    let group_activation = ctx.scope.cage(string(&detail, &["group_activation"]));
    let group_keywords = ctx
        .scope
        .cage(json_string_list(detail.get("group_keywords")).join("\n"));
    let aliases = ctx
        .scope
        .cage(json_string_list(detail.get("agent_aliases")).join("\n"));
    let ingest = ctx.scope.cage(string(&detail, &["ingest_policy"]));
    let bots = ctx.scope.cage(string(&detail, &["external_bot_policy"]));
    let (idle_enabled, idle_delay, idle_hour, idle_prompt) =
        json_idle_reply_fields(detail.get("idle_reply"));
    let idle_enabled = ctx.scope.cage(idle_enabled);
    let idle_delay = ctx.scope.cage(idle_delay);
    let idle_hour = ctx.scope.cage(idle_hour);
    let idle_prompt = ctx.scope.cage(idle_prompt);
    let terminal = detail.get("terminal");
    let delegate = terminal.and_then(|v| v.get("delegate")).or(terminal);
    let (_, command, args, stdin, cwd, env, timeout, include_prompt) =
        json_terminal_delegate_fields(delegate);
    let (profile, mode, session_scope, io_protocol) = json_terminal_runtime_fields(delegate);
    let (execution, approval) = json_terminal_permission_fields(delegate);
    let (workspace, base, cleanup) = json_terminal_workspace_fields(delegate);
    let command = ctx.scope.cage(command);
    let args = ctx.scope.cage(args);
    let stdin = ctx.scope.cage(stdin);
    let cwd = ctx.scope.cage(cwd);
    let env = ctx.scope.cage(env);
    let timeout = ctx.scope.cage(timeout);
    let include_prompt = ctx.scope.cage(include_prompt);
    let profile = ctx.scope.cage(profile);
    let mode = ctx.scope.cage(mode);
    let session_scope = ctx.scope.cage(session_scope);
    let io_protocol = ctx.scope.cage(io_protocol);
    let execution = ctx.scope.cage(execution);
    let approval = ctx.scope.cage(approval);
    let workspace = ctx.scope.cage(workspace);
    let base = ctx.scope.cage(base);
    let cleanup = ctx.scope.cage(cleanup);
    let channels = ctx.scope.cage(
        detail
            .get("channel_replies")
            .cloned()
            .unwrap_or(json!({}))
            .to_string(),
    );
    let matrix = ctx
        .scope
        .cage(json_string_list(detail.get("matrix_auto_user_channels")).join("\n"));
    let state = query(ctx, "models.list", None);
    let model_options = state.value.map(|v| {
        array(v, &["models"])
            .into_iter()
            .map(|model| {
                (
                    string(&model, &["id"]),
                    string(&model, &["name", "model_slug", "id"]),
                )
            })
            .collect::<Vec<_>>()
    });
    let model_scope = ctx.scope.clone();
    let mut sections = vec![
        ui::field(ui::label_for(locale, "Agent ID"), id, false),
        ui::field(ui::label_for(locale, "Agent name"), name, false),
        choice_for(
            locale,
            "Agent kind",
            kind,
            &[("native", "Native agent"), ("terminal", "Terminal agent")],
        ),
        ui::field(ui::label_for(locale, "System prompt"), prompt, true),
    ];
    sections.push(Box::new(
        col()
            .class("agent-kind-section")
            .style(kind.map(|value| {
                Style::new().display(if value == "native" {
                    Display::Flex
                } else {
                    Display::None
                })
            }))
            .fill(card(
                ui::label_for(locale, "Native model"),
                vec![
                    ui::field(ui::label_for(locale, "Provider"), provider, false),
                    ui::field(ui::label_for(locale, "Model ID"), model, false),
                    ui::field(
                        ui::label_for(locale, "Fallback model IDs (one per line)"),
                        fallback,
                        true,
                    ),
                    choice_for(
                        locale,
                        "Thinking level",
                        thinking,
                        &[
                            ("", "Provider default"),
                            ("off", "Off"),
                            ("minimal", "Minimal"),
                            ("low", "Low"),
                            ("medium", "Medium"),
                            ("high", "High"),
                            ("xhigh", "Extra high"),
                        ],
                    ),
                    select_source(ctx, "Model", model, model_options),
                ],
            )),
    ));
    let _ = model_scope;
    sections.push(Box::new(
        col()
            .class("agent-kind-section")
            .style(kind.map(|value| {
                Style::new().display(if value == "terminal" {
                    Display::Flex
                } else {
                    Display::None
                })
            }))
            .fill(card(
                ui::label_for(locale, "Terminal runtime"),
                vec![
                    choice_for(
                        locale,
                        "Runtime",
                        profile,
                        &[("codex", "Codex"), ("claude", "Claude")],
                    ),
                    ui::field(ui::label_for(locale, "Executable"), command, false),
                    ui::field(
                        ui::label_for(locale, "Arguments (one per line or Claude JSON settings)"),
                        args,
                        true,
                    ),
                    ui::field(
                        ui::label_for(locale, "Standard input template"),
                        stdin,
                        true,
                    ),
                    ui::field(ui::label_for(locale, "Working directory"), cwd, false),
                    ui::field(
                        ui::label_for(locale, "Environment (KEY=value per line)"),
                        env,
                        true,
                    ),
                    ui::field(ui::label_for(locale, "Timeout seconds"), timeout, false),
                    toggle(
                        ui::label_for(locale, "Include system prompt"),
                        include_prompt,
                    ),
                    choice_for(
                        locale,
                        "Runtime mode",
                        mode,
                        &[
                            ("app_server", "Codex app server"),
                            ("claude_stream", "Claude stream"),
                            ("one_shot", "One shot"),
                            ("managed_pty", "Managed PTY"),
                        ],
                    ),
                    choice_for(
                        locale,
                        "Session scope",
                        session_scope,
                        &[
                            ("per_turn", "Per turn"),
                            ("per_session", "Per session"),
                            ("per_agent", "Per agent"),
                            ("manual", "Manual"),
                        ],
                    ),
                    choice_for(
                        locale,
                        "I/O protocol",
                        io_protocol,
                        &[
                            ("plain_text", "Plain text"),
                            ("jsonl", "JSONL"),
                            ("profile", "Profile"),
                            ("sentinel", "Sentinel"),
                        ],
                    ),
                    choice_for(
                        locale,
                        "Terminal execution",
                        execution,
                        &[
                            ("native_terminal", "Native terminal"),
                            ("managed_workspace", "Managed workspace"),
                        ],
                    ),
                    choice_for(
                        locale,
                        "Approval bridge",
                        approval,
                        &[("disabled", "Disabled"), ("required", "Required")],
                    ),
                    choice_for(
                        locale,
                        "Workspace mode",
                        workspace,
                        &[
                            ("shared", "Shared"),
                            ("read_only", "Read only"),
                            ("worktree", "Git worktree"),
                            ("patch_only", "Patch only"),
                        ],
                    ),
                    ui::field(ui::label_for(locale, "Workspace base"), base, false),
                    choice_for(
                        locale,
                        "Cleanup policy",
                        cleanup,
                        &[
                            ("per_turn", "Per turn"),
                            ("per_session", "Per session"),
                            ("manual", "Manual"),
                        ],
                    ),
                ],
            )),
    ));
    sections.push(card(
        ui::label_for(locale, "Channel behavior"),
        vec![
            choice_for(
                locale,
                "Group activation",
                group_activation,
                &[
                    ("", "Inherit"),
                    ("mention", "Mention"),
                    ("always", "Always"),
                    ("keyword", "Keyword"),
                ],
            ),
            ui::field(
                ui::label_for(locale, "Group keywords (one per line)"),
                group_keywords,
                true,
            ),
            ui::field(
                ui::label_for(locale, "Agent aliases (one per line)"),
                aliases,
                true,
            ),
            choice_for(
                locale,
                "Ingest policy",
                ingest,
                &[("", "Inherit"), ("mentions", "Mentions"), ("all", "All")],
            ),
            choice_for(
                locale,
                "External bot policy",
                bots,
                &[("", "Inherit"), ("ignore", "Ignore"), ("allow", "Allow")],
            ),
            ui::field(
                ui::label_for(locale, "Channel reply overrides (JSON object)"),
                channels,
                true,
            ),
            ui::field(
                ui::label_for(locale, "Matrix auto-user channel IDs (one per line)"),
                matrix,
                true,
            ),
            toggle(ui::label_for(locale, "Idle replies"), idle_enabled),
            ui::field(
                ui::label_for(locale, "Idle delay seconds"),
                idle_delay,
                false,
            ),
            ui::field(
                ui::label_for(locale, "Idle replies per hour"),
                idle_hour,
                false,
            ),
            ui::field(
                ui::label_for(locale, "Idle reply prompt"),
                idle_prompt,
                true,
            ),
        ],
    ));
    let context = ctx.clone();
    let save_state = state.clone();
    let initial = detail.clone();
    sections.push(status(ctx, &state));
    sections.push(Box::new(
        sapid::button()
            .label(if creating {
                "Create agent"
            } else {
                "Save agent"
            })
            .loading(state.busy)
            .on_press(move || {
                if id.get().trim().is_empty() || name.get().trim().is_empty() {
                    ui::set(
                        save_state.error,
                        "Agent ID and name are required".to_owned(),
                    );
                    return;
                }
                if kind.get() == "native"
                    && (provider.get().trim().is_empty() || model.get().trim().is_empty())
                {
                    ui::set(
                        save_state.error,
                        "Provider and model are required for native agents".to_owned(),
                    );
                    return;
                }
                if kind.get() == "terminal" && command.get().trim().is_empty() {
                    ui::set(
                        save_state.error,
                        "Executable is required for terminal agents".to_owned(),
                    );
                    return;
                }
                let Ok(channel_replies) = serde_json::from_str::<Value>(&channels.get()) else {
                    ui::set(
                        save_state.error,
                        "Channel reply overrides must be valid JSON".to_owned(),
                    );
                    return;
                };
                let mut payload = initial.clone();
                if !payload.is_object() {
                    payload = json!({});
                }
                payload[if creating { "id" } else { "agent" }] = json!(id.get().trim());
                payload["name"] = json!(name.get().trim());
                payload["kind"] = json!(kind.get());
                payload["system_prompt"] = json!(prompt.get());
                if kind.get() == "native" {
                    let mut native = payload["native"].clone();
                    if !native.is_object() {
                        native = json!({});
                    }
                    native["provider"] = json!(provider.get());
                    native["model"] = json!(model.get());
                    native["fallback_models"] = json!(normalize_multiline_list(&fallback.get()));
                    if thinking.get().is_empty() {
                        native
                            .as_object_mut()
                            .map(|object| object.remove("thinking"));
                    } else {
                        native["thinking"] = json!(thinking.get());
                    }
                    payload["native"] = native;
                    payload
                        .as_object_mut()
                        .map(|object| object.remove("terminal"));
                } else if let Some(mut terminal) = terminal_delegate_payload(
                    true,
                    &command.get(),
                    &args.get(),
                    &stdin.get(),
                    &cwd.get(),
                    &env.get(),
                    &timeout.get(),
                    include_prompt.get(),
                    "",
                    "",
                    &profile.get(),
                    &mode.get(),
                    &session_scope.get(),
                    &io_protocol.get(),
                    &execution.get(),
                    &approval.get(),
                    &workspace.get(),
                    &base.get(),
                    &cleanup.get(),
                ) {
                    terminal["runtime"] = json!(profile.get());
                    payload["terminal"] = terminal;
                }
                payload["group_activation"] = json!(group_activation.get());
                payload["group_keywords"] = json!(normalize_multiline_list(&group_keywords.get()));
                payload["agent_aliases"] = json!(normalize_multiline_list(&aliases.get()));
                payload["ingest_policy"] = json!(ingest.get());
                payload["external_bot_policy"] = json!(bots.get());
                payload["channel_replies"] = channel_replies;
                payload["matrix_auto_user_channels"] =
                    json!(normalize_multiline_list(&matrix.get()));
                payload["idle_reply"] = idle_reply_payload(
                    idle_enabled.get(),
                    &idle_delay.get(),
                    &idle_hour.get(),
                    &idle_prompt.get(),
                )
                .unwrap_or(Value::Null);
                let context_async = context.clone();
                let state = save_state.clone();
                ui::set(state.busy, true);
                ui::set(state.error, String::new());
                context.spawn(async move {
                    match context_async
                        .rpc
                        .call::<Value>(
                            if creating {
                                "agents.create"
                            } else {
                                "agents.update"
                            },
                            Some(payload),
                        )
                        .await
                    {
                        Ok(_) => {
                            if creating {
                                context_async.navigate("/agents");
                            } else {
                                ui::set(state.error, "Saved".to_owned());
                            }
                        }
                        Err(error) => ui::set(state.error, error),
                    }
                    ui::set(state.busy, false);
                });
            }),
    ));
    if creating {
        ui::page(
            ui::label_for(locale, "New agent"),
            ui::label_for(
                locale,
                "Configure models, terminal execution and channel behavior",
            ),
            sections,
        )
    } else {
        agent_section(
            "Agent settings",
            "Configure models, terminal execution and channel behavior",
            sections,
        )
    }
}

fn files(ctx: &AppContext, agent: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "agents.files.list", Some(json!({"agent_id":agent})));
    let path = ctx.scope.cage(String::new());
    let content = ctx.scope.cage(String::new());
    let original = ctx.scope.cage(String::new());
    let row_state = state.clone();
    let agent = agent.to_owned();
    let agent_list = agent.clone();
    let rows = records(ctx, &state, &["files"], move |file, context| {
        let name = string(&file, &["name", "path"]);
        let context = context.clone();
        let agent = agent_list.clone();
        let error = row_state.error;
        ui::action(name.clone(), move || {
            let context_async = context.clone();
            let agent = agent.clone();
            let name = name.clone();
            context.spawn(async move {
                match context_async
                    .rpc
                    .call::<Value>(
                        "agents.files.get",
                        Some(json!({"agent_id":agent,"path":name})),
                    )
                    .await
                {
                    Ok(file) => {
                        let text = string(&file, &["content"]);
                        ui::set(path, name);
                        ui::set(content, text.clone());
                        ui::set(original, text);
                    }
                    Err(message) => ui::set(error, message),
                }
            });
        })
    });
    let mut actions = vec![];
    for (label, method) in [
        ("Save file", "agents.files.set"),
        ("Delete file", "agents.files.delete"),
    ] {
        let context = ctx.clone();
        let state = state.clone();
        let agent = agent.clone();
        actions.push(ui::action(label, move || {
            if path.get().trim().is_empty() {
                ui::set(state.error, "File path is required".to_owned());
                return;
            }
            if method.ends_with("delete") && !confirmed("Delete this agent file?") {
                return;
            }
            mutate(
                &context,
                &state,
                method,
                json!({"agent_id":agent,"path":path.get(),"content":content.get()}),
            );
        }));
    }
    actions.push(ui::action(ui::label_for(locale, "Reset"), move || {
        ui::set(content, original.get())
    }));
    agent_section(
        "Agent files",
        "Read, create and edit agent workspace files",
        vec![
            status(ctx, &state),
            refresh(ctx, &state),
            rows,
            ui::field(ui::label_for(locale, "File path"), path, false),
            ui::field(ui::label_for(locale, "Content"), content, true),
            Box::new(row().class("actions").fill(actions)),
        ],
    )
}
fn tools(ctx: &AppContext, agent: &str, detail: Value) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "security.rules.list", None);
    let profile = ctx.scope.cage(
        detail
            .pointer("/permission_policy/preset_id")
            .and_then(Value::as_str)
            .unwrap_or("coding")
            .to_owned(),
    );
    let allowed = ctx.scope.cage(
        detail
            .pointer("/permission_policy/tool_access/allowed")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
    );
    let command = ctx.scope.cage(String::new());
    let decision = ctx.scope.cage("ask".to_owned());
    let simulation = ctx.scope.cage(Value::Null);
    let context = ctx.clone();
    let agent_id = agent.to_owned();
    let tool_state = state.clone();
    let save = ui::action(ui::label_for(locale, "Save permissions"), move || {
        let Some(boundary) = crate::api::types::agent_preset_boundary(&profile.get()) else {
            ui::set(
                tool_state.error,
                "Select a valid permission preset".to_owned(),
            );
            return;
        };
        mutate(
            &context,
            &tool_state,
            "agents.update",
            json!({"agent":agent_id,"permission_policy":{"preset_id":profile.get(),"sandbox":boundary.sandbox,"approval":boundary.approval,"tool_access":{"allowed":normalize_multiline_list(&allowed.get())}},"execution_policy":{"mode":boundary.execution_mode}}),
        );
    });
    let context = ctx.clone();
    let agent_id = agent.to_owned();
    let error = state.error;
    let simulate = ui::action(ui::label_for(locale, "Simulate command"), move || {
        let context_async = context.clone();
        let agent = agent_id.clone();
        let command = command.get();
        context.spawn(async move {
            match context_async
                .rpc
                .call::<Value>(
                    "security.policy.simulate",
                    Some(json!({"agent":agent,"command":command})),
                )
                .await
            {
                Ok(value) => ui::set(simulation, value),
                Err(message) => ui::set(error, message),
            }
        });
    });
    let context = ctx.clone();
    let add_state = state.clone();
    let add = ui::action(ui::label_for(locale, "Add rule"), move || {
        if command.get().trim().is_empty() {
            ui::set(add_state.error, "Command is required".to_owned());
            return;
        }
        mutate(
            &context,
            &add_state,
            "security.rules.add",
            json!({"command":command.get(),"decision":decision.get()}),
        );
    });
    let rules_state = state.clone();
    agent_section(
        "Tools and permissions",
        "Agent execution boundaries and core execpolicy rules",
        vec![
            status(ctx, &state),
            choice_for(
                locale,
                "Permission preset",
                profile,
                &[
                    ("minimal", "Minimal"),
                    ("messaging", "Messaging"),
                    ("coding", "Coding"),
                    ("full", "Full"),
                    ("unrestricted", "Unrestricted"),
                ],
            ),
            ui::field(
                ui::label_for(locale, "Allowed tools (one per line)"),
                allowed,
                true,
            ),
            save,
            ui::field(ui::label_for(locale, "Command"), command, false),
            simulate,
            ui::json_text(ctx, simulation),
            choice_for(
                locale,
                "Decision",
                decision,
                &[("allow", "Allow"), ("ask", "Ask"), ("deny", "Deny")],
            ),
            add,
            records(ctx, &state, &["rules"], move |rule, context| {
                let context = context.clone();
                let state = rules_state.clone();
                let params = json!({"command":rule["command"],"decision":rule["decision"]});
                card(
                    string(&rule, &["rule"]),
                    vec![
                        metadata_for(locale, &rule, &[("decision", "Decision")]),
                        ui::action(ui::label_for(locale, "Remove"), move || {
                            mutate(&context, &state, "security.rules.remove", params.clone())
                        }),
                    ],
                )
            }),
        ],
    )
}
fn skills(ctx: &AppContext, agent: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "skills.status", None);
    let enabled = query(ctx, "agents.skills.get", Some(json!({"agent":agent})));
    let agent = agent.to_owned();
    let row_state = enabled.clone();
    agent_section(
        "Agent skills",
        "Enable or disable installed skills for this agent",
        vec![
            status(ctx, &state),
            status(ctx, &enabled),
            refresh(ctx, &state),
            records(ctx, &state, &["skills"], move |skill, context| {
                let name = string(&skill, &["name"]);
                let selected_name = name.clone();
                let selected = row_state.value.map(move |value| {
                    array(value, &["skills", "enabled"])
                        .iter()
                        .any(|v| v.as_str() == Some(&selected_name))
                });
                let context = context.clone();
                let state = row_state.clone();
                let agent = agent.clone();
                card(
                    name.clone(),
                    vec![
                        metadata_for(
                            locale,
                            &skill,
                            &[
                                ("description", "Description"),
                                ("category", "Source"),
                                ("path", "Path"),
                            ],
                        ),
                        Box::new(
                            sapid::checkbox()
                                .label(ui::label_for(locale, "Enabled"))
                                .checked(selected)
                                .on_checked_change(move |enabled| {
                                    mutate(
                                        &context,
                                        &state,
                                        "agents.skills.set",
                                        json!({"agent":agent,"skill":name,"enabled":enabled}),
                                    )
                                }),
                        ),
                    ],
                )
            }),
        ],
    )
}

fn agent_section(
    title: &str,
    description: &str,
    mut children: Vec<Box<dyn Widget>>,
) -> Box<dyn Widget> {
    children.insert(0, paragraph(description));
    card(title, children)
}
#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        codex_app_server_args, codex_extra_args, detail_terminal_delegate_fields,
        json_terminal_delegate_fields, json_terminal_permission_fields,
        json_terminal_workspace_fields, model_option_label, model_select_value,
        terminal_delegate_payload,
    };
    use crate::api::types::{AgentTerminalDelegateConfig, AvailableModel};

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
    fn claude_stream_configuration_round_trips_without_prompt_arguments() {
        let settings = json!({"expected_version":"tested version","model":"sonnet","mcp_config":"{\"mcpServers\":{}}"});
        let payload = terminal_delegate_payload(
            true,
            "claude",
            &settings.to_string(),
            "{{prompt}}",
            "",
            "",
            "300",
            true,
            "",
            "",
            "claude",
            "claude_stream",
            "per_session",
            "jsonl",
            "native_terminal",
            "required",
            "shared",
            "",
            "per_session",
        )
        .unwrap();
        assert_eq!(payload["claude"], settings);
        assert!(payload.get("args").is_none());
        assert!(payload.get("stdin").is_none());
        let (_, _, args, ..) = json_terminal_delegate_fields(Some(&payload));
        assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), settings);
        // The edit form loads the typed agents.get response, not the raw import JSON.
        let detail: AgentTerminalDelegateConfig = serde_json::from_value(payload).unwrap();
        let (_, _, args, ..) = detail_terminal_delegate_fields(Some(&detail));
        let edited = serde_json::from_str::<Value>(&args).unwrap();
        assert_eq!(edited, serde_json::to_value(&detail.claude).unwrap());
        assert_eq!(edited["expected_version"], settings["expected_version"]);
        assert_eq!(edited["mcp_config"], settings["mcp_config"]);
    }

    #[test]
    fn codex_app_server_payload_manages_transport_and_preserves_custom_settings() {
        let payload = terminal_delegate_payload(
            true,
            "C:/tools/codex.exe",
            "app-server\n--listen\nws://127.0.0.1:9000\n-c\nmodel=\"custom-model\"",
            "{{prompt}}",
            "D:/repo",
            "CODEX_HOME=D:/codex",
            "120",
            true,
            "codex",
            "",
            "codex",
            "app_server",
            "per_session",
            "jsonl",
            "disabled",
            "required",
            "worktree",
            "D:/base",
            "manual",
        )
        .expect("Codex configuration");
        assert_eq!(
            payload["args"],
            json!([
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                "model=\"custom-model\""
            ])
        );
        assert!(payload.get("stdin").is_none());
        assert!(payload.get("io_protocol").is_none());
        assert_eq!(payload["savfox_approval_bridge"], json!("disabled"));
        assert_eq!(payload["command"], json!("C:/tools/codex.exe"));
        assert_eq!(payload["cwd"], json!("D:/repo"));
        assert_eq!(payload["env"]["CODEX_HOME"], json!("D:/codex"));
        assert_eq!(payload["workspace"]["mode"], json!("worktree"));
        assert_eq!(
            codex_app_server_args(""),
            vec!["app-server", "--listen", "stdio://"]
        );
        assert_eq!(
            codex_extra_args("app-server\n--listen=stdio://\n-c\nmodel=\"custom-model\""),
            vec!["-c", "model=\"custom-model\""]
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
