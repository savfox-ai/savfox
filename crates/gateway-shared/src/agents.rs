use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentModels {
    pub primary: Option<String>,
    pub fallbacks: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentIdleReplyConfig {
    pub enabled: Option<bool>,
    pub delay_secs: Option<u64>,
    pub max_per_hour: Option<u32>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentTerminalDelegateConfig {
    pub enabled: Option<bool>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub stdin: Option<String>,
    pub cwd: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
    pub timeout_secs: Option<u64>,
    pub include_system_prompt: Option<bool>,
    /// Override `command` when the agent is launched as an interactive
    /// terminal session (vs. the one-shot delegate flow). When unset, the
    /// launcher falls back to `command`.
    pub interactive_command: Option<String>,
    /// Override `args` for interactive launches. When unset, the launcher
    /// invokes the program with no arguments (suitable for TUI tools like
    /// `codex` that drop into an interactive shell by default).
    pub interactive_args: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentEntry {
    pub id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    pub models: Option<AgentModels>,
    pub terminal_delegate: Option<AgentTerminalDelegateConfig>,
    pub system_prompt: Option<String>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentFile {
    #[serde(alias = "path")]
    pub name: String,
    pub size: Option<u64>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentFilesResponse {
    pub files: Vec<AgentFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentDetail {
    pub name: String,
    pub model: Option<String>,
    pub terminal_delegate: Option<AgentTerminalDelegateConfig>,
    pub system_prompt: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
    pub emoji: Option<String>,
    pub theme_color: Option<String>,
    pub is_default: Option<bool>,
    pub fallback_models: Option<Vec<String>>,
    pub group_activation: Option<String>,
    pub group_keywords: Option<Vec<String>>,
    pub agent_aliases: Option<Vec<String>>,
    pub ingest_policy: Option<String>,
    pub external_bot_policy: Option<String>,
    pub idle_reply: Option<AgentIdleReplyConfig>,
    /// Unified permission policy (sandbox + approval + tool access).
    pub permission_policy: Option<serde_json::Value>,
    /// List of Matrix appservice channel config IDs for which to auto-create
    /// a virtual user for this agent.
    pub matrix_auto_user_channels: Option<Vec<String>>,
}
