//! Agent-related wire types shared between gateway-server and the web /
//! Dioxus frontend.
//!
//! These structs are the JSON shapes returned by the `agents.*` family of
//! WS-RPC methods. The frontend uses them to render the agent picker and
//! the agent settings page; the backend persists the same shapes (with
//! some additional fields) under `<savfox_home>/agents/<id>.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            Other(String),
        }

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $value,)+
                    Self::Other(value) => value.as_str(),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($value => Self::$variant,)+
                    _ => Self::Other(value),
                })
            }
        }
    };
}

string_enum!(TerminalProfile {
    Codex => "codex",
    Claude => "claude",
});

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Native,
    Terminal,
}

impl AgentKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentRuntime {
    Codex,
    Claude,
}

impl TerminalAgentRuntime {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

string_enum!(TerminalMode {
    OneShot => "one_shot",
    ManagedPty => "managed_pty",
    InteractiveLaunch => "interactive_launch",
    JsonlAdapter => "jsonl_adapter",
});

string_enum!(TerminalSessionScope {
    PerTurn => "per_turn",
    PerSession => "per_session",
    PerAgent => "per_agent",
    Manual => "manual",
});

string_enum!(TerminalIoProtocol {
    PlainText => "plain_text",
    Jsonl => "jsonl",
    Profile => "profile",
    Sentinel => "sentinel",
});

string_enum!(TerminalExecution {
    NativeTerminal => "native_terminal",
    ManagedWorkspace => "managed_workspace",
    Disabled => "disabled",
});

string_enum!(SavfoxApprovalBridge {
    Disabled => "disabled",
    Prompt => "prompt",
    Required => "required",
});

string_enum!(TerminalWorkspaceMode {
    Shared => "shared",
    ReadOnly => "read_only",
    Worktree => "worktree",
    PatchOnly => "patch_only",
});

string_enum!(TerminalWorkspaceCleanupPolicy {
    PerTurn => "per_turn",
    PerSession => "per_session",
    Manual => "manual",
});

/// Declarative workspace policy for terminal agents. The runtime preserves
/// unknown string values so newer clients can safely round-trip future modes.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentTerminalWorkspaceConfig {
    /// Workspace mode. Expected values include `shared`, `read_only`,
    /// `worktree`, and `patch_only`.
    pub mode: Option<TerminalWorkspaceMode>,
    /// Optional base path or template for workspace creation / resolution.
    pub base: Option<String>,
    /// Cleanup lifecycle. Expected values include `per_turn`, `per_session`,
    /// and `manual`.
    pub cleanup_policy: Option<TerminalWorkspaceCleanupPolicy>,
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

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct NativeAgentConfig {
    pub provider: String,
    pub model: String,
    pub fallback_models: Option<Vec<String>>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TerminalAgentConfig {
    pub runtime: TerminalAgentRuntime,
    #[serde(flatten)]
    pub delegate: AgentTerminalDelegateConfig,
}

/// "Run a local CLI as the agent" configuration. Operators set this on
/// an `AgentEntry` to delegate the agent's turn to an external command
/// (e.g. `codex`, `claude`, a custom script) instead of the in-process
/// model client.
///
/// Two execution modes are supported:
///
/// * **One-shot delegate** — the gateway spawns `command` with `args`, pipes the prompt to stdin,
///   captures stdout/stderr, and returns the captured output as the agent's reply. Use `enabled =
///   true` plus the `command` / `args` / `stdin` / `cwd` / `env` / `timeout_secs` fields.
/// * **Interactive launch** — `agent.terminal.launch` opens a system terminal window running the
///   CLI directly so the operator can interact (login, multi-turn conversations, TUI). Override
///   `interactive_command` / `interactive_args` for tools that need a different invocation in
///   interactive mode.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentTerminalDelegateConfig {
    pub enabled: Option<bool>,
    /// Terminal agent profile. Expected values are `codex` and `claude`.
    pub profile: Option<TerminalProfile>,
    /// Runtime mode. Expected values include `one_shot`, `managed_pty`,
    /// `interactive_launch`, and `jsonl_adapter`.
    pub mode: Option<TerminalMode>,
    /// Session scoping strategy. Expected values include `per_turn`,
    /// `per_session`, `per_agent`, and `manual`.
    pub session_scope: Option<TerminalSessionScope>,
    /// How terminal output should be interpreted. Expected values include
    /// `plain_text`, `jsonl`, `profile`, and `sentinel`.
    pub io_protocol: Option<TerminalIoProtocol>,
    /// Optional command used only by `agent.terminal.health`. When unset,
    /// health checks use `command` plus the profile default version args.
    pub health_check_command: Option<String>,
    /// Optional arguments used only by `agent.terminal.health`. When unset,
    /// health checks use the profile default version args.
    pub health_check_args: Option<Vec<String>>,
    /// Permission boundary declaration for the terminal command. Expected
    /// values include `native_terminal`, `managed_workspace`, and `disabled`.
    pub terminal_execution: Option<TerminalExecution>,
    /// Whether Savfox should bridge vendor CLI approvals. Expected values
    /// include `disabled`, `prompt`, and `required`.
    pub savfox_approval_bridge: Option<SavfoxApprovalBridge>,
    /// Declarative workspace policy for the terminal runtime.
    pub workspace: Option<AgentTerminalWorkspaceConfig>,
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
    /// Canonical agent shape. `native` agents run through Savfox model
    /// providers; `terminal` agents delegate to a supported local CLI runtime.
    pub kind: AgentKind,
    pub native: Option<NativeAgentConfig>,
    pub terminal: Option<TerminalAgentConfig>,
    pub system_prompt: Option<String>,
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

string_enum!(AgentSandboxMode {
    ReadOnly => "read-only",
    WorkspaceWrite => "workspace-write",
    DangerFullAccess => "danger-full-access",
});

string_enum!(AgentApprovalMode {
    Untrusted => "untrusted",
    OnFailure => "on-failure",
    OnRequest => "on-request",
    Never => "never",
});

/// Fine-grained tool settings stored in an agent permission policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentToolAccessPolicy {
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_approval_overrides: BTreeMap<String, bool>,
}

/// Gateway representation of an agent's unified permission policy.
///
/// The sandbox representation intentionally remains the compact string form
/// persisted by the gateway rather than the richer core `SandboxPolicy` wire
/// shape.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentPermissionPolicy {
    #[serde(
        default,
        rename = "_preset_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub preset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<AgentSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<AgentApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_access: Option<AgentToolAccessPolicy>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentDetail {
    pub name: String,
    pub kind: AgentKind,
    pub native: Option<NativeAgentConfig>,
    pub terminal: Option<TerminalAgentConfig>,
    pub system_prompt: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub emoji: Option<String>,
    pub theme_color: Option<String>,
    pub is_default: Option<bool>,
    pub group_activation: Option<String>,
    pub channel_replies: Option<Vec<AgentChannelReplyConfig>>,
    pub group_keywords: Option<Vec<String>>,
    pub agent_aliases: Option<Vec<String>>,
    pub ingest_policy: Option<String>,
    pub external_bot_policy: Option<String>,
    pub idle_reply: Option<AgentIdleReplyConfig>,
    /// Unified permission policy (sandbox + approval + tool access).
    pub permission_policy: Option<AgentPermissionPolicy>,
    /// List of Matrix appservice channel config IDs for which to auto-create
    /// a virtual user for this agent.
    pub matrix_auto_user_channels: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentChannelReplyConfig {
    #[serde(alias = "route", alias = "channel")]
    pub channel_id: String,
    pub group_activation: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AgentApprovalMode, AgentPermissionPolicy, AgentSandboxMode, AgentTerminalDelegateConfig,
        SavfoxApprovalBridge, TerminalAgentConfig, TerminalAgentRuntime, TerminalExecution,
        TerminalIoProtocol, TerminalMode, TerminalProfile, TerminalSessionScope,
        TerminalWorkspaceCleanupPolicy, TerminalWorkspaceMode,
    };

    #[test]
    fn terminal_agent_config_round_trip() {
        let parsed: TerminalAgentConfig = serde_json::from_value(json!({
            "runtime": "codex",
            "enabled": true,
            "profile": "codex",
            "mode": "one_shot",
            "command": "codex",
            "args": ["exec", "{{prompt}}"]
        }))
        .expect("terminal agent config should deserialize");

        assert_eq!(parsed.runtime, TerminalAgentRuntime::Codex);
        assert_eq!(parsed.delegate.enabled, Some(true));
        assert_eq!(parsed.delegate.profile, Some(TerminalProfile::Codex));
        assert_eq!(parsed.delegate.mode, Some(TerminalMode::OneShot));
        assert_eq!(parsed.delegate.command.as_deref(), Some("codex"));

        let serialized = serde_json::to_value(&parsed).expect("serialize terminal agent config");
        assert_eq!(serialized["runtime"], "codex");
        assert_eq!(serialized["profile"], "codex");
        assert_eq!(serialized["command"], "codex");
    }

    #[test]
    fn terminal_delegate_v2_fields_round_trip() {
        let parsed: AgentTerminalDelegateConfig = serde_json::from_value(json!({
            "enabled": true,
            "profile": "claude",
            "mode": "one_shot",
            "session_scope": "per_session",
            "io_protocol": "plain_text",
            "terminal_execution": "native_terminal",
            "savfox_approval_bridge": "disabled",
            "workspace": {
                "mode": "worktree",
                "base": "{{workspace_dir}}",
                "cleanup_policy": "per_session"
            },
            "command": "claude"
        }))
        .expect("v2 terminal delegate config should deserialize");

        assert_eq!(parsed.profile, Some(TerminalProfile::Claude));
        assert_eq!(parsed.mode, Some(TerminalMode::OneShot));
        assert_eq!(parsed.session_scope, Some(TerminalSessionScope::PerSession));
        assert_eq!(parsed.io_protocol, Some(TerminalIoProtocol::PlainText));
        assert_eq!(
            parsed.terminal_execution,
            Some(TerminalExecution::NativeTerminal)
        );
        assert_eq!(
            parsed.savfox_approval_bridge,
            Some(SavfoxApprovalBridge::Disabled)
        );
        let workspace = parsed.workspace.as_ref().expect("workspace config");
        assert_eq!(workspace.mode, Some(TerminalWorkspaceMode::Worktree));
        assert_eq!(workspace.base.as_deref(), Some("{{workspace_dir}}"));
        assert_eq!(
            workspace.cleanup_policy,
            Some(TerminalWorkspaceCleanupPolicy::PerSession)
        );

        let serialized = serde_json::to_value(&parsed).expect("serialize terminal config");
        assert_eq!(serialized["profile"], "claude");
        assert_eq!(serialized["mode"], "one_shot");
        assert_eq!(serialized["session_scope"], "per_session");
        assert_eq!(serialized["io_protocol"], "plain_text");
        assert_eq!(serialized["terminal_execution"], "native_terminal");
        assert_eq!(serialized["savfox_approval_bridge"], "disabled");
        assert_eq!(serialized["workspace"]["mode"], "worktree");
        assert_eq!(serialized["workspace"]["base"], "{{workspace_dir}}");
        assert_eq!(serialized["workspace"]["cleanup_policy"], "per_session");
    }

    #[test]
    fn terminal_delegate_unknown_v2_values_are_preserved() {
        let parsed: AgentTerminalDelegateConfig = serde_json::from_value(json!({
            "profile": "future_cli",
            "mode": "streaming_subprocess",
            "session_scope": "per_team",
            "io_protocol": "vendor_frames",
            "terminal_execution": "remote_sandbox",
            "savfox_approval_bridge": "brokered",
            "workspace": {
                "mode": "shadow_copy",
                "base": "/tmp/future",
                "cleanup_policy": "after_review"
            }
        }))
        .expect("unknown terminal v2 values should deserialize");

        assert_eq!(
            parsed.profile,
            Some(TerminalProfile::Other("future_cli".to_owned()))
        );
        assert_eq!(
            parsed.mode,
            Some(TerminalMode::Other("streaming_subprocess".to_owned()))
        );
        assert_eq!(
            parsed.session_scope,
            Some(TerminalSessionScope::Other("per_team".to_owned()))
        );
        assert_eq!(
            parsed.io_protocol,
            Some(TerminalIoProtocol::Other("vendor_frames".to_owned()))
        );
        assert_eq!(
            parsed.terminal_execution,
            Some(TerminalExecution::Other("remote_sandbox".to_owned()))
        );
        assert_eq!(
            parsed.savfox_approval_bridge,
            Some(SavfoxApprovalBridge::Other("brokered".to_owned()))
        );
        let workspace = parsed.workspace.as_ref().expect("workspace config");
        assert_eq!(
            workspace.mode,
            Some(TerminalWorkspaceMode::Other("shadow_copy".to_owned()))
        );
        assert_eq!(workspace.base.as_deref(), Some("/tmp/future"));
        assert_eq!(
            workspace.cleanup_policy,
            Some(TerminalWorkspaceCleanupPolicy::Other(
                "after_review".to_owned()
            ))
        );

        let serialized = serde_json::to_value(&parsed).expect("serialize terminal config");
        assert_eq!(serialized["profile"], "future_cli");
        assert_eq!(serialized["mode"], "streaming_subprocess");
        assert_eq!(serialized["session_scope"], "per_team");
        assert_eq!(serialized["io_protocol"], "vendor_frames");
        assert_eq!(serialized["terminal_execution"], "remote_sandbox");
        assert_eq!(serialized["savfox_approval_bridge"], "brokered");
        assert_eq!(serialized["workspace"]["mode"], "shadow_copy");
        assert_eq!(serialized["workspace"]["base"], "/tmp/future");
        assert_eq!(serialized["workspace"]["cleanup_policy"], "after_review");
    }

    #[test]
    fn permission_policy_is_typed_and_preserves_future_modes() {
        let parsed: AgentPermissionPolicy = serde_json::from_value(json!({
            "_preset_id": "coding",
            "sandbox": "future-sandbox",
            "approval": "on-request",
            "tool_access": {
                "allowed": ["shell", "read_file"],
                "denied": ["browser"]
            }
        }))
        .expect("permission policy should deserialize");

        assert_eq!(parsed.preset_id.as_deref(), Some("coding"));
        assert_eq!(
            parsed.sandbox,
            Some(AgentSandboxMode::Other("future-sandbox".to_owned()))
        );
        assert_eq!(parsed.approval, Some(AgentApprovalMode::OnRequest));
        let tool_access = parsed
            .tool_access
            .as_ref()
            .expect("tool access should exist");
        assert_eq!(tool_access.allowed, ["shell", "read_file"]);

        let serialized = serde_json::to_value(parsed).expect("permission policy should serialize");
        assert_eq!(serialized["sandbox"], "future-sandbox");
        assert_eq!(serialized["tool_access"]["denied"], json!(["browser"]));
    }
}
