//! Unified permission presets for both TUI and Gateway agent pages.
//!
//! Each preset maps to a complete [`PermissionPolicy`] that captures sandbox,
//! approval, and tool access settings in a single value.

use savfox_core::protocol::{AskForApproval, SandboxPolicy};
use savfox_protocol::protocol::{PermissionPolicy, ToolAccessPolicy};

/// A named permission preset pairing a display label with a full policy.
#[derive(Debug, Clone)]
pub struct PermissionPreset {
    /// Stable identifier for the preset (used in agent JSON, CLI args, etc.).
    pub id: &'static str,
    /// Display label shown in UIs.
    pub label: &'static str,
    /// Short human description shown next to the label.
    pub description: &'static str,
    /// The full permission policy this preset represents.
    pub policy: PermissionPolicy,
}

// ---------------------------------------------------------------------------
// TUI presets (3 coarse options)
// ---------------------------------------------------------------------------

/// Built-in TUI presets: Read Only, Default, Full Access.
///
/// These are the three options shown in the TUI permission popup and provide a
/// quick, coarse-grained way for users to set permissions.
pub fn builtin_tui_presets() -> Vec<PermissionPreset> {
    vec![
        PermissionPreset {
            id: "read-only",
            label: "Read Only",
            description: "Read files. Approval required for edits and internet access.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::ReadOnly,
                approval: AskForApproval::OnRequest,
                tool_access: Some(ToolAccessPolicy {
                    allowed: vec![
                        "read_file".into(),
                        "list_dir".into(),
                        "grep_files".into(),
                        "md_memory".into(),
                    ],
                    denied: vec![],
                    tool_approval_overrides: Default::default(),
                }),
            },
        },
        PermissionPreset {
            id: "auto",
            label: "Default",
            description: "Read/edit workspace files and run commands. Approval for internet/external.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::new_workspace_write_policy(),
                approval: AskForApproval::OnRequest,
                tool_access: None, // all tools available, sandbox enforces boundaries
            },
        },
        PermissionPreset {
            id: "full-access",
            label: "Full Access",
            description: "Full disk and internet access without approval prompts.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::DangerFullAccess,
                approval: AskForApproval::Never,
                tool_access: None,
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// Gateway agent presets (fine-grained options)
// ---------------------------------------------------------------------------

/// Built-in agent presets for the Gateway agent configuration page.
///
/// These provide fine-grained control over what each agent can do,
/// automatically setting the appropriate sandbox and approval policies.
pub fn builtin_agent_presets() -> Vec<PermissionPreset> {
    vec![
        PermissionPreset {
            id: "minimal",
            label: "Minimal",
            description: "Read-only access with memory.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::ReadOnly,
                approval: AskForApproval::OnRequest,
                tool_access: Some(ToolAccessPolicy {
                    allowed: vec![
                        "read_file".into(),
                        "list_dir".into(),
                        "grep_files".into(),
                        "md_memory".into(),
                    ],
                    denied: vec![],
                    tool_approval_overrides: Default::default(),
                }),
            },
        },
        PermissionPreset {
            id: "coding",
            label: "Coding",
            description: "File editing, shell, and code search.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::new_workspace_write_policy(),
                approval: AskForApproval::OnRequest,
                tool_access: Some(ToolAccessPolicy {
                    allowed: vec![
                        "read_file".into(),
                        "write_file".into(),
                        "list_dir".into(),
                        "shell".into(),
                        "grep_files".into(),
                        "md_memory".into(),
                    ],
                    denied: vec![],
                    tool_approval_overrides: Default::default(),
                }),
            },
        },
        PermissionPreset {
            id: "messaging",
            label: "Messaging",
            description: "Read files, browse, search web, memory.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::ReadOnly,
                approval: AskForApproval::OnRequest,
                tool_access: Some(ToolAccessPolicy {
                    allowed: vec![
                        "read_file".into(),
                        "list_dir".into(),
                        "browser".into(),
                        "web_fetch".into(),
                        "web_search".into(),
                        "md_memory".into(),
                    ],
                    denied: vec![],
                    tool_approval_overrides: Default::default(),
                }),
            },
        },
        PermissionPreset {
            id: "full",
            label: "Full",
            description: "All tools, workspace write, approval for dangerous operations.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::new_workspace_write_policy(),
                approval: AskForApproval::OnRequest,
                tool_access: None, // all tools
            },
        },
        PermissionPreset {
            id: "unrestricted",
            label: "Unrestricted",
            description: "All tools, full disk access, no approval prompts.",
            policy: PermissionPolicy {
                sandbox: SandboxPolicy::DangerFullAccess,
                approval: AskForApproval::Never,
                tool_access: None,
            },
        },
    ]
}

/// Look up an agent preset by its id.
pub fn find_agent_preset(id: &str) -> Option<PermissionPreset> {
    builtin_agent_presets().into_iter().find(|p| p.id == id)
}

/// Look up a TUI preset by its id.
pub fn find_tui_preset(id: &str) -> Option<PermissionPreset> {
    builtin_tui_presets().into_iter().find(|p| p.id == id)
}
