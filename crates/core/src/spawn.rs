use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};
use tracing::trace;

use crate::protocol::SandboxPolicy;

/// Extra environment entries that neutralize repo-config-driven code execution
/// when the spawned program is `git`.
///
/// A `git show`/`log`/`diff` on an untrusted working tree can run arbitrary code
/// via repo-local `core.pager` / `GIT_PAGER`, `diff.external`, or `.gitattributes`
/// diff drivers. We force a safe pager and disable external diff drivers via the
/// `GIT_CONFIG_*` env interface (so we don't have to rewrite argv, and it can't
/// be confused with a model-supplied `-c`). Caller-provided git config env is
/// left untouched. Note: per-driver `textconv` filters from repo config still
/// require `--no-textconv` on the command line and are not covered here.
fn git_safety_env(program: &Path, env: &HashMap<String, String>) -> Vec<(String, String)> {
    let is_git = program
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("git"))
        .unwrap_or(false);
    if !is_git {
        return Vec::new();
    }

    let mut extra = Vec::new();
    if !env.contains_key("GIT_PAGER") {
        extra.push(("GIT_PAGER".to_owned(), "cat".to_owned()));
    }
    if !env.contains_key("PAGER") {
        extra.push(("PAGER".to_owned(), "cat".to_owned()));
    }
    // Only inject our config block if the caller hasn't already set up a
    // GIT_CONFIG_* sequence (appending into one mid-sequence would be brittle).
    if !env.contains_key("GIT_CONFIG_COUNT") {
        extra.push(("GIT_CONFIG_COUNT".to_owned(), "2".to_owned()));
        extra.push(("GIT_CONFIG_KEY_0".to_owned(), "core.pager".to_owned()));
        extra.push(("GIT_CONFIG_VALUE_0".to_owned(), "cat".to_owned()));
        extra.push(("GIT_CONFIG_KEY_1".to_owned(), "diff.external".to_owned()));
        extra.push(("GIT_CONFIG_VALUE_1".to_owned(), String::new()));
    }
    extra
}

/// Experimental environment variable that will be set to some non-empty value
/// if both of the following are true:
///
/// 1. The process was spawned by Savfox as part of a shell tool call.
/// 2. SandboxPolicy.has_full_network_access() was false for the tool call.
///
/// We may try to have just one environment variable for all sandboxing
/// attributes, so this may change in the future.
pub const SAVFOX_SANDBOX_NETWORK_DISABLED_ENV_VAR: &str = "SAVFOX_SANDBOX_NETWORK_DISABLED";

/// Should be set when the process is spawned under a sandbox. Currently, the
/// value is "seatbelt" for macOS, but it may change in the future to
/// accommodate sandboxing configuration and other sandboxing mechanisms.
pub const SAVFOX_SANDBOX_ENV_VAR: &str = "SAVFOX_SANDBOX";

#[derive(Debug, Clone, Copy)]
pub enum StdioPolicy {
    RedirectForShellTool,
    Inherit,
}

/// Spawns the appropriate child process for the ExecParams and SandboxPolicy,
/// ensuring the args and environment variables used to create the `Command`
/// (and `Child`) honor the configuration.
///
/// For now, we take `SandboxPolicy` as a parameter to spawn_child() because
/// we need to determine whether to set the
/// `SAVFOX_SANDBOX_NETWORK_DISABLED_ENV_VAR` environment variable.
pub(crate) async fn spawn_child_async(
    program: PathBuf,
    args: Vec<String>,
    #[cfg_attr(not(unix), allow(unused_variables))] arg0: Option<&str>,
    cwd: PathBuf,
    sandbox_policy: &SandboxPolicy,
    stdio_policy: StdioPolicy,
    env: HashMap<String, String>,
) -> std::io::Result<Child> {
    trace!(
        "spawn_child_async: {program:?} {args:?} {arg0:?} {cwd:?} {sandbox_policy:?} {stdio_policy:?} {env:?}"
    );

    let mut cmd = Command::new(&program);
    #[cfg(unix)]
    cmd.arg0(arg0.map_or_else(|| program.to_string_lossy().to_string(), String::from));
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.env_clear();
    let git_extra = git_safety_env(&program, &env);
    cmd.envs(env);
    for (key, value) in git_extra {
        cmd.env(key, value);
    }

    if !sandbox_policy.has_full_network_access() {
        cmd.env(SAVFOX_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1");
    }

    // If this Savfox process dies (including being killed via SIGKILL), we want
    // any child processes that were spawned as part of a `"shell"` tool call
    // to also be terminated.
    savfox_utils::pty::process_group::configure_command_pre_exec(
        &mut cmd,
        matches!(stdio_policy, StdioPolicy::RedirectForShellTool),
    );

    match stdio_policy {
        StdioPolicy::RedirectForShellTool => {
            // Do not create a file descriptor for stdin because otherwise some
            // commands may hang forever waiting for input. For example, ripgrep has
            // a heuristic where it may try to read from stdin as explained here:
            // https://github.com/BurntSushi/ripgrep/blob/e2362d4d5185d02fa857bf381e7bd52e66fafc73/crates/core/flags/hiargs.rs#L1101-L1103
            cmd.stdin(Stdio::null());

            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        StdioPolicy::Inherit => {
            // Inherit stdin, stdout, and stderr from the parent process.
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
    }

    cmd.kill_on_drop(true).spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_safety_env_injects_for_git_only() {
        let empty = HashMap::new();
        for prog in [
            "git",
            "/usr/bin/git",
            "git.exe",
            "C:\\tools\\Git\\bin\\git.exe",
        ] {
            let extra = git_safety_env(Path::new(prog), &empty);
            assert!(
                extra
                    .iter()
                    .any(|(k, v)| k == "GIT_CONFIG_KEY_0" && v == "core.pager"),
                "{prog} should inject git config"
            );
            assert!(extra.iter().any(|(k, v)| k == "GIT_PAGER" && v == "cat"));
            assert!(
                extra
                    .iter()
                    .any(|(k, v)| k == "GIT_CONFIG_KEY_1" && v == "diff.external"),
                "{prog} should disable external diff"
            );
        }

        // Non-git programs get nothing.
        assert!(git_safety_env(Path::new("/bin/ls"), &empty).is_empty());
        assert!(git_safety_env(Path::new("python3"), &empty).is_empty());
    }

    #[test]
    fn git_safety_env_respects_caller_provided_config() {
        let mut env = HashMap::new();
        env.insert("GIT_PAGER".to_owned(), "less".to_owned());
        env.insert("GIT_CONFIG_COUNT".to_owned(), "1".to_owned());
        let extra = git_safety_env(Path::new("git"), &env);
        // We don't override an existing GIT_PAGER or GIT_CONFIG_* sequence.
        assert!(!extra.iter().any(|(k, _)| k == "GIT_PAGER"));
        assert!(!extra.iter().any(|(k, _)| k == "GIT_CONFIG_COUNT"));
        // PAGER was not set by the caller, so it is still added.
        assert!(extra.iter().any(|(k, v)| k == "PAGER" && v == "cat"));
    }
}
