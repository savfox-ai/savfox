//! Rust-owned Claude Code stdio execution. No SDK runtime or credential copying.
mod protocol;
mod runner;
mod runtime;

use anyhow::{bail, ensure};
pub(crate) use runner::{RunOptions, run};
pub(crate) use runtime::{Execution, Runtime};
use savfox_gateway_shared::ClaudeCodeConfig;

pub(crate) fn validate(config: &ClaudeCodeConfig, args: &[String]) -> anyhow::Result<()> {
    ensure!(
        args.is_empty(),
        "claude_stream owns argv; configure terminal.claude instead of args"
    );
    ensure!(
        matches!(config.settings_policy.as_str(), "controlled" | "user"),
        "unknown Claude settings_policy"
    );
    ensure!(
        (1..=60).contains(&config.initialize_timeout_secs),
        "Claude initialize timeout must be 1..60 seconds"
    );
    ensure!(
        (1..=600).contains(&config.approval_timeout_secs),
        "Claude approval timeout must be 1..600 seconds"
    );
    ensure!(
        (1..=30).contains(&config.shutdown_timeout_secs),
        "Claude shutdown timeout must be 1..30 seconds"
    );
    if let Some(model) = &config.model {
        ensure!(
            !model.is_empty()
                && !model.contains('/')
                && !model.starts_with('-')
                && !model.chars().any(char::is_whitespace),
            "use a Claude model name, not a provider/model slug"
        );
    }
    if config
        .expected_version
        .as_deref()
        .is_none_or(|version| version.trim().is_empty())
    {
        bail!(
            "claude_stream requires terminal.claude.expected_version (exact claude --version output); validate this CLI locally before enabling the adapter"
        );
    }
    if let Some(mcp) = &config.mcp_config {
        let value: serde_json::Value = serde_json::from_str(mcp)?;
        ensure!(
            value["mcpServers"].is_object(),
            "Claude mcp_config must be JSON with mcpServers"
        );
    }
    Ok(())
}

pub(crate) fn validate_workspace(mode: Option<&str>) -> anyhow::Result<()> {
    ensure!(
        mode.is_none_or(|mode| matches!(mode, "shared" | "worktree" | "patch_only")),
        "claude_stream supports shared, worktree and patch_only; read_only requires a verified OS sandbox"
    );
    Ok(())
}

pub(crate) fn digest(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value))
}

#[cfg(test)]
mod tests;
