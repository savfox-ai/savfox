//! Agent-related wire types shared between gateway-server and the web /
//! Dioxus frontend.
//!
//! These structs are the JSON shapes returned by the `agents.*` family of
//! WS-RPC methods. The frontend uses them to render the agent picker and
//! the agent settings page; the backend persists the same shapes (with
//! some additional fields) under `<savfox_home>/agents/<id>.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Optional model selection block on an [`AgentEntry`] — `primary` is the
/// model used when the agent is invoked, `fallbacks` are tried in order
/// when the primary is unavailable (e.g. provider 5xx, rate limit).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentModels {
    pub primary: Option<String>,
    pub fallbacks: Option<Vec<String>>,
}

/// Per-agent "auto-reply when the user has been quiet" configuration.
///
/// Used by the gateway's idle-reply scheduler: when a chat session has had
/// no inbound message for `delay_secs`, the agent emits `prompt` once,
/// rate-limited to at most `max_per_hour` per session.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentIdleReplyConfig {
    /// Enable / disable the idle-reply scheduler for this agent.
    pub enabled: Option<bool>,
    /// Idle window after which a reply may be sent.
    pub delay_secs: Option<u64>,
    /// Hard cap on how many idle replies the scheduler emits per hour.
    pub max_per_hour: Option<u32>,
    /// The prompt the agent runs to compose the idle reply.
    pub prompt: Option<String>,
}

/// "Run a local CLI as the agent" configuration. Operators set this on
/// an `AgentEntry` to delegate the agent's turn to an external command
/// (e.g. `codex`, `claude`, a custom script) instead of the in-process
/// model client.
///
/// Two execution modes are supported:
///
/// * **One-shot delegate** — the gateway spawns `command` with `args`,
///   pipes the prompt to stdin, captures stdout/stderr, and returns the
///   captured output as the agent's reply. Use `enabled = true` plus the
///   `command` / `args` / `stdin` / `cwd` / `env` / `timeout_secs` fields.
/// * **Interactive launch** — `agent.terminal.launch` opens a system
///   terminal window running the CLI directly so the operator can
///   interact (login, multi-turn conversations, TUI). Override
///   `interactive_command` / `interactive_args` for tools that need a
///   different invocation in interactive mode.
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

/// Summary view of an agent as returned by `agents.list` and the agent
/// picker in the UI. Concrete on-disk shape (under
/// `<savfox_home>/agents/<id>.json`) carries strictly more fields; the
/// fields here are the subset the frontend needs to render a row.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentEntry {
    /// Agent identifier (file stem under `agents/`). `None` only for
    /// in-memory placeholders before the agent has been saved.
    pub id: Option<String>,
    /// Human-visible label. Required.
    pub name: String,
    /// Convenience field — single-model selection. Newer agents should
    /// prefer the structured [`AgentModels`] block.
    pub model: Option<String>,
    pub models: Option<AgentModels>,
    pub terminal_delegate: Option<AgentTerminalDelegateConfig>,
    pub system_prompt: Option<String>,
    /// Optional reasoning-effort hint passed to the model when the agent
    /// is invoked. Free-form string.
    pub thinking: Option<String>,
    /// Optional verbosity hint passed to the model.
    pub verbose: Option<String>,
    /// Operator-set status string (e.g. `"online"` / `"draft"` /
    /// `"archived"`). Surface only — not enforced by the runtime.
    pub status: Option<String>,
    /// RFC3339 creation timestamp set on first save.
    pub created_at: Option<String>,
    /// Whether the agent is currently flagged as the default for new
    /// sessions. At most one agent should carry `Some(true)`.
    pub is_default: Option<bool>,
}

/// `agents.list` envelope.
#[derive(Debug, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentEntry>,
}

/// One entry under `agents.files.list` — files the agent can attach to
/// its turn (system-prompt fragments, reference docs).
#[derive(Clone, Debug, Deserialize)]
pub struct AgentFile {
    /// File name relative to the agent's files directory. Accepts
    /// `path` as a serde alias for backward compatibility.
    #[serde(alias = "path")]
    pub name: String,
    pub size: Option<u64>,
    /// Inline contents — present on `agents.files.get`, omitted from
    /// the `list` response.
    pub content: Option<String>,
}

/// `agents.files.list` envelope.
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
