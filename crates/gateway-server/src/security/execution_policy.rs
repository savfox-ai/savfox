//! Shared execution-security policy resolution for every gateway entry point.
//!
//! Agent permissions describe the capability boundary (sandbox and tools).
//! [`ExecutionMode`] independently describes what an entry point does when a
//! request crosses that boundary. Keeping these dimensions separate prevents
//! "unattended" from becoming an accidental synonym for full host access.

use std::path::{Path, PathBuf};

use savfox_core::config::Config;
use savfox_core::protocol::{AskForApproval, SandboxPolicy};
use savfox_gateway_shared::{
    AgentApprovalMode, AgentExecutionMode, AgentExecutionPolicy, AgentPermissionPolicy,
    AgentSandboxMode,
};
use savfox_protocol::protocol::GranularApprovalConfig;
use savfox_protocol::protocol::ToolAccessPolicy;
use savfox_utils::home_dir::AGENTS_SUBDIR;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::path_safety::safe_join;

/// Behavior when a tool request crosses the configured permission boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionMode {
    /// Ask a correlated, authenticated client and wait for its decision.
    Interactive,
    /// Never wait for a human; boundary requests are denied immediately.
    Unattended,
    /// Reserved for the reviewed-automation phase. Until a reviewer is wired,
    /// this resolves fail-closed to [`ExecutionMode::Unattended`].
    AutoReview,
}

impl ExecutionMode {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Unattended => "unattended",
            Self::AutoReview => "auto-review",
        }
    }
}

/// Approval features supported by an authenticated gateway entry point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApprovalClientCapabilities {
    pub(crate) supports_interactive_approvals: bool,
    pub(crate) supports_request_ids: bool,
    pub(crate) supports_session_grants: bool,
    pub(crate) supports_persisted_rules: bool,
}

impl ApprovalClientCapabilities {
    /// Fully interactive first-party gateway client.
    #[must_use]
    pub(crate) fn interactive() -> Self {
        Self {
            supports_interactive_approvals: true,
            supports_request_ids: true,
            supports_session_grants: true,
            supports_persisted_rules: true,
        }
    }

    /// Capabilities of a text channel adapter. Unknown and one-way adapters are
    /// intentionally non-interactive.
    #[must_use]
    pub(crate) fn for_channel(platform: &str) -> Self {
        let interactive = matches!(
            platform.trim().to_ascii_lowercase().as_str(),
            "arkret"
                | "dingtalk"
                | "discord"
                | "feishu"
                | "googlechat"
                | "irc"
                | "lark"
                | "line"
                | "matrix"
                | "mattermost"
                | "msteams"
                | "qq"
                | "signal"
                | "slack"
                | "telegram"
                | "wechat"
                | "whatsapp"
                | "zalo"
        );
        Self {
            supports_interactive_approvals: interactive,
            supports_request_ids: interactive,
            supports_session_grants: interactive,
            supports_persisted_rules: interactive,
        }
    }

    /// Capabilities for a concrete inbound message. Interactive approval is
    /// disabled when the adapter did not provide an authenticated peer id.
    #[must_use]
    pub(crate) fn for_channel_message(platform: &str, peer_id: Option<&str>) -> Self {
        let has_authenticated_peer = peer_id
            .map(str::trim)
            .is_some_and(|peer_id| !peer_id.is_empty());
        if has_authenticated_peer {
            Self::for_channel(platform)
        } else {
            Self::default()
        }
    }

    #[must_use]
    fn can_interact(self) -> bool {
        self.supports_interactive_approvals && self.supports_request_ids
    }
}

/// Immutable security metadata associated with one gateway invocation.
#[derive(Clone, Debug)]
pub(crate) struct ExecutionSecurityContext {
    pub(crate) agent_id: String,
    pub(crate) mode: ExecutionMode,
    pub(crate) requested_mode: ExecutionMode,
    pub(crate) capabilities: ApprovalClientCapabilities,
    pub(crate) policy_fingerprint: String,
    pub(crate) policy_source: Option<PathBuf>,
    pub(crate) fallback_reason: Option<String>,
    pub(crate) sandbox_enforcement: &'static str,
    pub(crate) effective_sandbox: &'static str,
}

/// Effective core configuration plus its immutable gateway security context.
#[derive(Clone)]
pub(crate) struct ResolvedExecutionSecurity {
    pub(crate) config: Config,
    pub(crate) context: ExecutionSecurityContext,
}

fn sanitized_agent_file_stem(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            _ => ch,
        };
        out.push(mapped);
    }

    let out = out.trim_matches([' ', '.']).to_owned();
    if out.is_empty() || matches!(out.as_str(), "." | "..") {
        None
    } else {
        Some(out)
    }
}

async fn read_json(path: &Path) -> Option<Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

fn agent_config_matches(config: &Value, agent_ref: &str) -> bool {
    let matches_value = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case(agent_ref))
    };
    matches_value(config.get("id"))
        || matches_value(config.get("name"))
        || matches_value(
            config
                .get("identity")
                .and_then(|identity| identity.get("name")),
        )
}

async fn load_agent_config(
    savfox_home: &Path,
    agent_ref: &str,
) -> Result<Option<(PathBuf, Value)>, PathBuf> {
    let agent_ref = agent_ref.trim();
    if agent_ref.is_empty() {
        return Ok(None);
    }

    let dir = savfox_home.join(AGENTS_SUBDIR);
    if let Some(stem) = sanitized_agent_file_stem(agent_ref)
        && stem == agent_ref
        && let Some(path) = safe_join(&dir, &stem, ".json")
    {
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => {
                let config = serde_json::from_str(&data).map_err(|_| path.clone())?;
                return Ok(Some((path, config)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(path),
        }
    }

    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Ok(None);
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(config) = read_json(&path).await else {
            continue;
        };
        if agent_config_matches(&config, agent_ref) {
            return Ok(Some((path, config)));
        }
    }
    Ok(None)
}

fn requested_execution_mode(
    execution_policy: Option<&AgentExecutionPolicy>,
    permission_policy: Option<&AgentPermissionPolicy>,
    capabilities: ApprovalClientCapabilities,
) -> ExecutionMode {
    if let Some(execution_policy) = execution_policy {
        return match execution_policy.mode {
            AgentExecutionMode::Interactive => ExecutionMode::Interactive,
            AgentExecutionMode::Unattended => ExecutionMode::Unattended,
            AgentExecutionMode::AutoReview => ExecutionMode::AutoReview,
            AgentExecutionMode::Other(_) => ExecutionMode::Unattended,
        };
    }

    match permission_policy.and_then(|policy| policy.approval.as_ref()) {
        Some(AgentApprovalMode::Never) => ExecutionMode::Unattended,
        Some(
            AgentApprovalMode::Untrusted
            | AgentApprovalMode::OnFailure
            | AgentApprovalMode::OnRequest
            | AgentApprovalMode::Granular,
        ) => {
            if capabilities.can_interact() {
                ExecutionMode::Interactive
            } else {
                ExecutionMode::Unattended
            }
        }
        Some(AgentApprovalMode::Other(_)) => ExecutionMode::Unattended,
        None => {
            if capabilities.can_interact() {
                ExecutionMode::Interactive
            } else {
                ExecutionMode::Unattended
            }
        }
    }
}

fn effective_execution_mode(
    requested: ExecutionMode,
    capabilities: ApprovalClientCapabilities,
) -> (ExecutionMode, Option<String>) {
    match requested {
        ExecutionMode::Interactive if !capabilities.can_interact() => (
            ExecutionMode::Unattended,
            Some("entry point cannot correlate interactive approval responses".to_owned()),
        ),
        ExecutionMode::AutoReview => (
            ExecutionMode::Unattended,
            Some("automatic approval reviewer is not configured".to_owned()),
        ),
        other => (other, None),
    }
}

fn apply_permission_policy(config: &mut Config, policy: &AgentPermissionPolicy) -> Option<String> {
    let mut fallback_reason = None;
    if let Some(profile) = policy.profile.as_deref() {
        let sandbox = match profile {
            ":read-only" => SandboxPolicy::ReadOnly,
            ":workspace" => SandboxPolicy::new_workspace_write_policy(),
            ":danger-full-access" => SandboxPolicy::DangerFullAccess,
            unknown => {
                fallback_reason = Some(format!(
                    "unknown permission profile '{unknown}'; using read-only"
                ));
                SandboxPolicy::ReadOnly
            }
        };
        if let Err(error) = config.sandbox_policy.set(sandbox) {
            warn!(%error, profile, "permission profile rejected by managed constraints");
        }
    }

    // A named profile is the authoritative filesystem boundary. The legacy
    // `sandbox` field is consulted only when no profile is selected so stale
    // compatibility data cannot silently override a profile.
    if policy.profile.is_none()
        && let Some(sandbox_mode) = policy.sandbox.as_ref()
    {
        let sandbox = match sandbox_mode {
            AgentSandboxMode::ReadOnly => Some(SandboxPolicy::ReadOnly),
            AgentSandboxMode::WorkspaceWrite => Some(SandboxPolicy::new_workspace_write_policy()),
            AgentSandboxMode::DangerFullAccess => Some(SandboxPolicy::DangerFullAccess),
            AgentSandboxMode::Other(value) => {
                warn!(
                    sandbox = value,
                    "unknown agent sandbox policy; using read-only"
                );
                Some(SandboxPolicy::ReadOnly)
            }
        };
        if let Some(sandbox) = sandbox
            && let Err(error) = config.sandbox_policy.set(sandbox)
        {
            warn!(%error, "agent sandbox policy rejected by managed constraints");
        }
    }

    if let Some(tool_access) = policy.tool_access.as_ref() {
        config.tool_access_policy = Some(ToolAccessPolicy {
            allowed: tool_access.allowed.clone(),
            denied: tool_access.denied.clone(),
            tool_approval_overrides: tool_access
                .tool_approval_overrides
                .clone()
                .into_iter()
                .collect(),
        });
    }
    if let Some(network) = policy.network.as_ref() {
        let current = config.sandbox_policy.get().clone();
        if let SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network_access: _,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
        } = current
        {
            let has_domain_scope =
                !network.allowed_domains.is_empty() || !network.denied_domains.is_empty();
            let enabled = network.enabled && !has_domain_scope;
            if network.enabled && has_domain_scope {
                fallback_reason = Some(match fallback_reason {
                    Some(existing) => format!(
                        "{existing}; domain-scoped network permission requires managed proxy enforcement and remains restricted"
                    ),
                    None => {
                        "domain-scoped network permission requires managed proxy enforcement and remains restricted"
                            .to_owned()
                    }
                });
            }
            if let Err(error) = config.sandbox_policy.set(SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access: enabled,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
            }) {
                warn!(%error, "agent network permission rejected by managed constraints");
            }
        } else if network.enabled {
            fallback_reason = Some(match fallback_reason {
                Some(existing) => {
                    format!(
                        "{existing}; network permission is unavailable for this sandbox profile"
                    )
                }
                None => "network permission is unavailable for this sandbox profile".to_owned(),
            });
        }
    }
    fallback_reason
}

fn apply_execution_mode(
    config: &mut Config,
    mode: ExecutionMode,
    policy: Option<&AgentPermissionPolicy>,
) {
    let approval = match mode {
        ExecutionMode::Interactive => match policy {
            Some(AgentPermissionPolicy {
                approval: Some(AgentApprovalMode::Granular),
                granular_approval: Some(granular),
                ..
            }) => AskForApproval::Granular(GranularApprovalConfig {
                sandbox_approval: granular.sandbox_approval,
                rules: granular.rules,
            }),
            _ => AskForApproval::OnRequest,
        },
        ExecutionMode::Unattended | ExecutionMode::AutoReview => AskForApproval::Never,
    };
    if let Err(error) = config.approval_policy.set(approval) {
        warn!(%error, mode = mode.as_str(), "execution mode rejected by managed constraints");
    }
}

#[cfg(target_os = "windows")]
fn enforce_platform_sandbox_readiness(config: &mut Config) -> Option<String> {
    use savfox_core::windows_sandbox::{WindowsSandboxLevelExt, sandbox_setup_is_complete};
    use savfox_protocol::config_types::WindowsSandboxLevel;

    if !matches!(
        config.sandbox_policy.get(),
        SandboxPolicy::WorkspaceWrite { .. }
    ) {
        return None;
    }

    match WindowsSandboxLevel::from_config(config) {
        WindowsSandboxLevel::Elevated if !sandbox_setup_is_complete(&config.savfox_home) => {
            // The elevated backend requires provisioning. Gateway entry points
            // have no safe way to open an elevation prompt, so retain a real
            // sandbox by falling back to Restricted Token.
            config.set_windows_elevated_sandbox_enabled(false);
            config.set_windows_sandbox_enabled(true);
            Some("elevated Windows sandbox is not ready; using restricted-token sandbox".to_owned())
        }
        WindowsSandboxLevel::Disabled => {
            if let Err(error) = config.sandbox_policy.set(SandboxPolicy::ReadOnly) {
                warn!(%error, "Windows fail-closed read-only policy rejected by managed constraints");
            }
            Some(
                "Windows workspace-write sandbox is disabled; using read-only fail-closed policy"
                    .to_owned(),
            )
        }
        WindowsSandboxLevel::RestrictedToken | WindowsSandboxLevel::Elevated => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn enforce_platform_sandbox_readiness(_config: &mut Config) -> Option<String> {
    None
}

fn sandbox_enforcement(config: &Config) -> &'static str {
    match config.sandbox_policy.get() {
        SandboxPolicy::DangerFullAccess => "unrestricted",
        SandboxPolicy::ExternalSandbox { .. } => "external-sandbox",
        SandboxPolicy::ReadOnly => "read-only-command-gate",
        SandboxPolicy::WorkspaceWrite { .. } => {
            #[cfg(target_os = "windows")]
            {
                use savfox_core::windows_sandbox::WindowsSandboxLevelExt;
                use savfox_protocol::config_types::WindowsSandboxLevel;
                return match WindowsSandboxLevel::from_config(config) {
                    WindowsSandboxLevel::Elevated => "windows-elevated",
                    WindowsSandboxLevel::RestrictedToken => "windows-restricted-token",
                    WindowsSandboxLevel::Disabled => "unavailable",
                };
            }
            #[cfg(target_os = "linux")]
            {
                return "linux-seccomp";
            }
            #[cfg(target_os = "macos")]
            {
                return "macos-seatbelt";
            }
            #[allow(unreachable_code)]
            "platform-sandbox"
        }
    }
}

fn effective_sandbox_name(config: &Config) -> &'static str {
    match config.sandbox_policy.get() {
        SandboxPolicy::DangerFullAccess => "danger-full-access",
        SandboxPolicy::ExternalSandbox { .. } => "external-sandbox",
        SandboxPolicy::ReadOnly => "read-only",
        SandboxPolicy::WorkspaceWrite { .. } => "workspace-write",
    }
}

fn policy_fingerprint(
    config: &Config,
    agent_ref: &str,
    policy: Option<&AgentPermissionPolicy>,
    requested_mode: ExecutionMode,
    effective_mode: ExecutionMode,
    capabilities: ApprovalClientCapabilities,
) -> String {
    let material = json!({
        "version": 1,
        "agent": agent_ref.trim(),
        "permission_policy": policy,
        "requested_mode": requested_mode.as_str(),
        "effective_mode": effective_mode.as_str(),
        "capabilities": {
            "interactive": capabilities.supports_interactive_approvals,
            "request_ids": capabilities.supports_request_ids,
            "session_grants": capabilities.supports_session_grants,
            "persisted_rules": capabilities.supports_persisted_rules,
        },
        "effective_sandbox": format!("{:?}", config.sandbox_policy.get()),
        "effective_approval": format!("{:?}", config.approval_policy.get()),
        "effective_tool_access": format!("{:?}", config.tool_access_policy),
        "effective_cwd": config.cwd.to_string_lossy(),
        "effective_mcp_servers": format!("{:?}", config.mcp_servers.get()),
        "effective_web_search": format!("{:?}", config.web_search_mode),
        "effective_shell_environment": format!("{:?}", config.shell_environment_policy),
        "effective_notify": &config.notify,
        "effective_apply_patch": config.include_apply_patch_tool,
        "effective_unified_exec": config.use_experimental_unified_exec_tool,
        "effective_features": format!("{:?}", config.features),
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&material).unwrap_or_default());
    hex::encode(hasher.finalize())
}

/// Resolve an agent's capability policy and entry-point behavior before a
/// session is started, resumed, or forked.
pub(crate) async fn resolve_execution_security(
    base_config: &Config,
    savfox_home: &Path,
    agent_ref: &str,
    capabilities: ApprovalClientCapabilities,
) -> ResolvedExecutionSecurity {
    let mut config = base_config.clone();
    let (loaded, invalid_agent_config) = match load_agent_config(savfox_home, agent_ref).await {
        Ok(loaded) => (loaded, false),
        Err(path) => {
            warn!(
                agent = agent_ref,
                path = %path.display(),
                "agent config is unreadable or malformed; applying fail-closed policy"
            );
            (None, true)
        }
    };
    let policy_source = loaded.as_ref().map(|(path, _)| path.clone());
    let permission_value = loaded
        .as_ref()
        .and_then(|(_, value)| value.get("permission_policy"))
        .filter(|value| !value.is_null());
    let (permission_policy, invalid_permission_policy) = match permission_value {
        Some(value) => match serde_json::from_value::<AgentPermissionPolicy>(value.clone()) {
            Ok(policy) => (Some(policy), false),
            Err(error) => {
                warn!(agent = agent_ref, %error, "invalid agent permission policy; applying fail-closed policy");
                (None, true)
            }
        },
        None => (None, false),
    };
    let execution_value = loaded
        .as_ref()
        .and_then(|(_, value)| value.get("execution_policy"))
        .filter(|value| !value.is_null());
    let (execution_policy, invalid_execution_policy) = match execution_value {
        Some(value) => match serde_json::from_value::<AgentExecutionPolicy>(value.clone()) {
            Ok(policy) => (Some(policy), false),
            Err(error) => {
                warn!(agent = agent_ref, %error, "invalid agent execution policy; applying fail-closed policy");
                (None, true)
            }
        },
        None => (None, false),
    };

    let unknown_policy_value = permission_policy.as_ref().is_some_and(|policy| {
        matches!(policy.sandbox.as_ref(), Some(AgentSandboxMode::Other(_)))
            || matches!(policy.approval.as_ref(), Some(AgentApprovalMode::Other(_)))
            || policy.profile.as_deref().is_some_and(|profile| {
                !matches!(profile, ":read-only" | ":workspace" | ":danger-full-access")
            })
    }) || execution_policy
        .as_ref()
        .is_some_and(|policy| matches!(&policy.mode, AgentExecutionMode::Other(_)));
    let profile_fallback = permission_policy
        .as_ref()
        .and_then(|policy| apply_permission_policy(&mut config, policy));
    let invalid_policy = invalid_agent_config
        || invalid_permission_policy
        || invalid_execution_policy
        || unknown_policy_value;
    if invalid_policy && let Err(error) = config.sandbox_policy.set(SandboxPolicy::ReadOnly) {
        warn!(%error, "fail-closed read-only sandbox rejected by managed constraints");
    }
    let readiness_fallback = enforce_platform_sandbox_readiness(&mut config);

    let requested_mode = if invalid_policy {
        ExecutionMode::Unattended
    } else {
        requested_execution_mode(
            execution_policy.as_ref(),
            permission_policy.as_ref(),
            capabilities,
        )
    };
    let (mode, mut fallback_reason) = effective_execution_mode(requested_mode, capabilities);
    if invalid_policy {
        fallback_reason = Some("agent security configuration is invalid".to_owned());
        if unknown_policy_value {
            fallback_reason =
                Some("agent security configuration contains an unknown policy value".to_owned());
        }
    }
    if let Some(readiness_fallback) = readiness_fallback {
        fallback_reason = Some(match fallback_reason {
            Some(existing) => format!("{existing}; {readiness_fallback}"),
            None => readiness_fallback,
        });
    }
    if let Some(profile_fallback) = profile_fallback {
        fallback_reason = Some(match fallback_reason {
            Some(existing) => format!("{existing}; {profile_fallback}"),
            None => profile_fallback,
        });
    }
    apply_execution_mode(&mut config, mode, permission_policy.as_ref());
    let sandbox_enforcement = sandbox_enforcement(&config);
    let effective_sandbox = effective_sandbox_name(&config);
    let policy_fingerprint = policy_fingerprint(
        &config,
        agent_ref,
        permission_policy.as_ref(),
        requested_mode,
        mode,
        capabilities,
    );

    ResolvedExecutionSecurity {
        config,
        context: ExecutionSecurityContext {
            agent_id: agent_ref.trim().to_owned(),
            mode,
            requested_mode,
            capabilities,
            policy_fingerprint,
            policy_source,
            fallback_reason,
            sandbox_enforcement,
            effective_sandbox,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_is_unattended() {
        let capabilities = ApprovalClientCapabilities::for_channel("future-one-way-adapter");
        assert!(!capabilities.can_interact());
        assert_eq!(
            requested_execution_mode(None, None, capabilities),
            ExecutionMode::Unattended
        );
    }

    #[test]
    fn interactive_mode_requires_request_correlation() {
        let capabilities = ApprovalClientCapabilities {
            supports_interactive_approvals: true,
            supports_request_ids: false,
            ..ApprovalClientCapabilities::default()
        };
        let (mode, reason) = effective_execution_mode(ExecutionMode::Interactive, capabilities);
        assert_eq!(mode, ExecutionMode::Unattended);
        assert!(reason.is_some());
    }

    #[test]
    fn unattended_does_not_change_the_sandbox_dimension() {
        let policy: AgentPermissionPolicy = serde_json::from_value(json!({
            "sandbox": "workspace-write",
            "approval": "never"
        }))
        .expect("permission policy");
        let capabilities = ApprovalClientCapabilities::for_channel("telegram");
        assert_eq!(
            requested_execution_mode(None, Some(&policy), capabilities),
            ExecutionMode::Unattended
        );
        assert_eq!(policy.sandbox, Some(AgentSandboxMode::WorkspaceWrite));
    }

    #[test]
    fn auto_review_fails_closed_until_reviewer_is_available() {
        let capabilities = ApprovalClientCapabilities::for_channel("telegram");
        let (mode, reason) = effective_execution_mode(ExecutionMode::AutoReview, capabilities);
        assert_eq!(mode, ExecutionMode::Unattended);
        assert!(reason.is_some());
    }

    #[test]
    fn unknown_legacy_approval_mode_fails_closed() {
        let policy: AgentPermissionPolicy = serde_json::from_value(json!({
            "approval": "future-mode"
        }))
        .expect("forward-compatible policy");
        assert_eq!(
            requested_execution_mode(
                None,
                Some(&policy),
                ApprovalClientCapabilities::interactive()
            ),
            ExecutionMode::Unattended
        );
    }

    #[tokio::test]
    async fn malformed_direct_agent_config_is_not_treated_as_missing() {
        let home = tempfile::tempdir().expect("temp home");
        let agents = home.path().join(AGENTS_SUBDIR);
        tokio::fs::create_dir_all(&agents)
            .await
            .expect("agents dir");
        tokio::fs::write(agents.join("worker.json"), "{malformed")
            .await
            .expect("agent config");

        assert!(load_agent_config(home.path(), "worker").await.is_err());
    }

    #[tokio::test]
    async fn sanitized_name_cannot_alias_an_unrelated_agent_file() {
        let home = tempfile::tempdir().expect("temp home");
        let agents = home.path().join(AGENTS_SUBDIR);
        tokio::fs::create_dir_all(&agents)
            .await
            .expect("agents dir");
        tokio::fs::write(
            agents.join("foo-bar.json"),
            r#"{"id":"foo-bar","permission_policy":{"sandbox":"danger-full-access"}}"#,
        )
        .await
        .expect("agent config");

        assert!(
            load_agent_config(home.path(), "foo/bar")
                .await
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn agent_file_stem_cannot_escape_agents_directory() {
        let stem = sanitized_agent_file_stem("../../outside").expect("sanitized stem");
        assert!(!stem.contains('/'));
        assert!(!stem.contains('\\'));
        assert_ne!(stem, "..");
    }
}
