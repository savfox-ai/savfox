//! Launches an agent's configured CLI tool in a system terminal window so the
//! user can interact with it directly (login flows, TUIs, multi-turn input).
//!
//! Unlike `agent_terminal_delegate`, which captures stdout for a one-shot
//! prompt, this module spawns a detached terminal emulator (Windows Terminal,
//! Terminal.app, gnome-terminal, …) and hands control to the user.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[allow(unused_imports)]
use anyhow::{Context, Result, anyhow, bail};
use savfox_core::config::Config;
use serde::Deserialize;
use serde_json::Value;
#[allow(unused_imports)]
use tokio::process::Command;

use crate::agent_terminal_delegate::resolve_agent_config;
use crate::terminal_agent::{
    TerminalCommandResolver, TerminalCommandTemplate, TerminalTemplateValues,
};

const INTERACTIVE_LAUNCH_TIMEOUT_SECS: u64 = 300;
const INTERACTIVE_LAUNCH_MIN_TIMEOUT_SECS: u64 = 5;

/// Result of a successful interactive launch.
#[derive(Debug, Clone)]
pub(crate) struct InteractiveLaunchResult {
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) terminal: String,
    pub(crate) pid: Option<u32>,
}

/// Internal: minimal subset of the agent JSON we care about for launch.
#[derive(Debug, Clone, Default, Deserialize)]
struct LaunchSpec {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    interactive_command: Option<String>,
    #[serde(default)]
    interactive_args: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedLaunch {
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: BTreeMap<String, String>,
}

/// Resolve an agent reference into a concrete program + cwd ready to launch.
/// Pure function — does not spawn anything.
pub(crate) fn resolve_launch(
    base_cwd: &Path,
    file_stem: String,
    raw: Value,
) -> Result<ResolvedLaunch> {
    if raw.get("kind").and_then(|value| value.as_str()) != Some("terminal") {
        bail!("agent is not a terminal agent");
    }
    let delegate_value = raw
        .get("terminal")
        .ok_or_else(|| anyhow!("agent has no `terminal` configuration"))?;

    let spec: LaunchSpec =
        serde_json::from_value(delegate_value.clone()).context("invalid terminal configuration")?;

    let agent_id = raw
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(file_stem.as_str())
        .to_owned();
    let agent_name = raw
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(agent_id.as_str())
        .to_owned();

    let command = spec
        .interactive_command
        .as_deref()
        .or(spec.command.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "terminal agent is missing a `command` (and `interactive_command`) — nothing to launch"
            )
        })?
        .to_owned();
    let values = TerminalTemplateValues {
        agent_id: agent_id.clone(),
        agent_name: agent_name.clone(),
        ..Default::default()
    };
    let resolved = TerminalCommandResolver::new(base_cwd).resolve(
        TerminalCommandTemplate {
            command,
            args: spec.interactive_args.unwrap_or_default(),
            stdin: None,
            cwd: spec.cwd,
            env: spec.env,
            timeout_secs: None,
            default_args_to_prompt: false,
            default_timeout_secs: INTERACTIVE_LAUNCH_TIMEOUT_SECS,
            min_timeout_secs: INTERACTIVE_LAUNCH_MIN_TIMEOUT_SECS,
            max_output_bytes: 0,
        },
        &values,
    )?;

    Ok(ResolvedLaunch {
        agent_id,
        agent_name,
        program: resolved.spec.program,
        args: resolved.spec.args,
        cwd: resolved.spec.cwd,
        env: resolved.spec.env,
    })
}

/// Top-level entry: resolve the agent and spawn a system terminal window.
pub(crate) async fn launch_interactive(
    config: &Config,
    agent_ref: &str,
) -> Result<InteractiveLaunchResult> {
    let (file_stem, raw) = resolve_agent_config(config, agent_ref)
        .await
        .ok_or_else(|| anyhow!("agent `{agent_ref}` not found"))?;

    let resolved = resolve_launch(&config.cwd, file_stem, raw)?;
    spawn_in_terminal(resolved).await
}

#[cfg(target_os = "windows")]
async fn spawn_in_terminal(resolved: ResolvedLaunch) -> Result<InteractiveLaunchResult> {
    // CREATE_NEW_CONSOLE | DETACHED_PROCESS conflict on Windows; use just
    // CREATE_NEW_CONSOLE so the spawned terminal owns its own console window.
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    let cmd_line = build_windows_command_line(&resolved.program, &resolved.args);

    // Try Windows Terminal (`wt.exe`) first — it pops a tab/window even when
    // launched from a service-style host process.
    if which_exists("wt.exe").await {
        let mut cmd = Command::new("wt.exe");
        cmd.arg("-d")
            .arg(&resolved.cwd)
            .arg("--")
            .arg("cmd.exe")
            .arg("/k")
            .arg(&cmd_line)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in &resolved.env {
            cmd.env(key, value);
        }
        cmd.creation_flags(CREATE_NEW_CONSOLE);
        let child = cmd
            .spawn()
            .context("failed to spawn `wt.exe` for interactive agent terminal")?;
        return Ok(InteractiveLaunchResult {
            agent_id: resolved.agent_id,
            agent_name: resolved.agent_name,
            program: resolved.program,
            args: resolved.args,
            cwd: resolved.cwd,
            terminal: "windows-terminal".to_owned(),
            pid: child.id(),
        });
    }

    // Fallback: classic `cmd /c start cmd /k <cmd-line>` opens a new console.
    let title = format!("Savfox Agent: {}", resolved.agent_name);
    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/c")
        .arg("start")
        .arg(&title)
        .arg("/D")
        .arg(&resolved.cwd)
        .arg("cmd.exe")
        .arg("/k")
        .arg(&cmd_line)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &resolved.env {
        cmd.env(key, value);
    }
    cmd.creation_flags(CREATE_NEW_CONSOLE);
    let child = cmd
        .spawn()
        .context("failed to spawn `cmd start` for interactive agent terminal")?;

    Ok(InteractiveLaunchResult {
        agent_id: resolved.agent_id,
        agent_name: resolved.agent_name,
        program: resolved.program,
        args: resolved.args,
        cwd: resolved.cwd,
        terminal: "cmd".to_owned(),
        pid: child.id(),
    })
}

#[cfg(target_os = "macos")]
async fn spawn_in_terminal(resolved: ResolvedLaunch) -> Result<InteractiveLaunchResult> {
    let cmd_line = build_posix_command_line(&resolved.program, &resolved.args);
    let cwd_str = resolved.cwd.to_string_lossy().to_string();

    let mut env_prefix = String::new();
    for (key, value) in &resolved.env {
        env_prefix.push_str(&format!(
            "export {}={}; ",
            shell_quote_posix(key),
            shell_quote_posix(value)
        ));
    }

    let script = format!(
        "cd {}; {}{}",
        shell_quote_posix(&cwd_str),
        env_prefix,
        cmd_line
    );
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    let apple_script =
        format!("tell application \"Terminal\"\nactivate\ndo script \"{escaped}\"\nend tell");

    let child = Command::new("osascript")
        .arg("-e")
        .arg(&apple_script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn `osascript` to open Terminal.app")?;

    Ok(InteractiveLaunchResult {
        agent_id: resolved.agent_id,
        agent_name: resolved.agent_name,
        program: resolved.program,
        args: resolved.args,
        cwd: resolved.cwd,
        terminal: "macos-terminal".to_owned(),
        pid: child.id(),
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
async fn spawn_in_terminal(resolved: ResolvedLaunch) -> Result<InteractiveLaunchResult> {
    let cmd_line = build_posix_command_line(&resolved.program, &resolved.args);
    let cwd_str = resolved.cwd.to_string_lossy().to_string();
    let inner_script = format!(
        "{}; echo; echo '[savfox] command exited — press Enter to close'; read _",
        cmd_line
    );

    let candidates: &[(&str, &[&str])] = &[
        (
            "gnome-terminal",
            &["--working-directory=", "--", "bash", "-c"],
        ),
        ("konsole", &["--workdir", "-e", "bash", "-c"]),
        ("xfce4-terminal", &["--working-directory=", "-e"]),
        ("x-terminal-emulator", &["-e", "bash", "-c"]),
        ("xterm", &["-e", "bash", "-c"]),
    ];

    for (program, _flags) in candidates {
        if !which_exists(program).await {
            continue;
        }

        let mut cmd = Command::new(program);
        match *program {
            "gnome-terminal" => {
                cmd.arg(format!("--working-directory={cwd_str}"))
                    .arg("--")
                    .arg("bash")
                    .arg("-c")
                    .arg(&inner_script);
            }
            "konsole" => {
                cmd.arg("--workdir")
                    .arg(&cwd_str)
                    .arg("-e")
                    .arg("bash")
                    .arg("-c")
                    .arg(&inner_script);
            }
            "xfce4-terminal" => {
                cmd.arg(format!("--working-directory={cwd_str}"))
                    .arg("-e")
                    .arg(format!("bash -c {}", shell_quote_posix(&inner_script)));
            }
            "x-terminal-emulator" | "xterm" => {
                let wrapped = format!("cd {} && {}", shell_quote_posix(&cwd_str), inner_script);
                cmd.arg("-e").arg("bash").arg("-c").arg(&wrapped);
            }
            _ => unreachable!(),
        }

        for (key, value) in &resolved.env {
            cmd.env(key, value);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                return Ok(InteractiveLaunchResult {
                    agent_id: resolved.agent_id,
                    agent_name: resolved.agent_name,
                    program: resolved.program,
                    args: resolved.args,
                    cwd: resolved.cwd,
                    terminal: (*program).to_owned(),
                    pid: child.id(),
                });
            }
            Err(err) => {
                tracing::warn!("failed to spawn `{program}` for interactive agent: {err}");
            }
        }
    }

    bail!(
        "no supported Linux terminal emulator found (tried gnome-terminal, konsole, xfce4-terminal, x-terminal-emulator, xterm)"
    );
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
async fn spawn_in_terminal(_resolved: ResolvedLaunch) -> Result<InteractiveLaunchResult> {
    bail!("interactive agent launch is not supported on this platform");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn shell_quote_posix(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    let safe = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | ',' | '@'));
    if safe {
        return value.to_owned();
    }
    let escaped = value.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(any(unix, target_os = "macos"))]
#[allow(dead_code)]
fn build_posix_command_line(program: &str, args: &[String]) -> String {
    let mut out = shell_quote_posix(program);
    for arg in args {
        out.push(' ');
        out.push_str(&shell_quote_posix(arg));
    }
    out
}

#[cfg(target_os = "windows")]
fn build_windows_command_line(program: &str, args: &[String]) -> String {
    let mut out = quote_windows_arg(program);
    for arg in args {
        out.push(' ');
        out.push_str(&quote_windows_arg(arg));
    }
    out
}

#[cfg(target_os = "windows")]
fn quote_windows_arg(arg: &str) -> String {
    // CommandLineToArgvW quoting: only spaces/tabs/quotes force the wrapping
    // double-quotes. Bare backslashes are passed through unchanged. Shell
    // metacharacters (&, <, >, |, ^) get quoted too so cmd.exe doesn't
    // interpret them inside the eventual `/k <cmdline>` argument.
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '"' | '&' | '<' | '>' | '|' | '^'))
    {
        return arg.to_owned();
    }
    // CommandLineToArgvW-compatible quoting.
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(c);
                backslashes = 0;
            }
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

#[allow(dead_code)]
async fn which_exists(program: &str) -> bool {
    // Lightweight PATH lookup; avoids pulling in the `which` crate.
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![String::new()]
    };

    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if file_exists(&candidate).await {
            return true;
        }
        if cfg!(windows)
            && Path::new(program).extension().is_none()
            && let Some(stem) = candidate.file_name().and_then(|f| f.to_str())
        {
            for ext in &exts {
                let mut with_ext = candidate.with_file_name(stem);
                with_ext.set_extension(ext.trim_start_matches('.'));
                if file_exists(&with_ext).await {
                    return true;
                }
            }
        }
    }
    false
}

#[allow(dead_code)]
async fn file_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolve_launch_prefers_interactive_command() {
        let cwd = PathBuf::from("/tmp");
        let raw = json!({
            "id": "codex",
            "name": "Codex",
            "kind": "terminal",
            "terminal": {
                "command": "codex",
                "args": ["exec", "{{prompt}}"],
                "interactive_command": "codex",
                "interactive_args": []
            }
        });

        let resolved = resolve_launch(&cwd, "codex".to_owned(), raw).expect("resolve");
        assert_eq!(resolved.program, "codex");
        assert!(resolved.args.is_empty());
        assert_eq!(resolved.agent_id, "codex");
        assert_eq!(resolved.agent_name, "Codex");
    }

    #[test]
    fn resolve_launch_falls_back_to_command_when_interactive_missing() {
        let cwd = PathBuf::from("/tmp");
        let raw = json!({
            "id": "claude",
            "kind": "terminal",
            "terminal": {
                "command": "claude"
            }
        });

        let resolved = resolve_launch(&cwd, "claude".to_owned(), raw).expect("resolve");
        assert_eq!(resolved.program, "claude");
        assert!(
            resolved.args.is_empty(),
            "default args are empty for interactive launch"
        );
    }

    #[test]
    fn resolve_launch_errors_when_no_command() {
        let cwd = PathBuf::from("/tmp");
        let raw = json!({
            "id": "broken",
            "kind": "terminal",
            "terminal": {
                "args": ["foo"]
            }
        });

        let err = resolve_launch(&cwd, "broken".to_owned(), raw).unwrap_err();
        assert!(
            err.to_string().contains("missing a `command`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_launch_errors_when_not_terminal_agent() {
        let cwd = PathBuf::from("/tmp");
        let raw = json!({ "id": "plain" });
        let err = resolve_launch(&cwd, "plain".to_owned(), raw).unwrap_err();
        assert!(err.to_string().contains("not a terminal agent"));
    }

    #[test]
    fn resolve_launch_resolves_relative_cwd() {
        let cwd = PathBuf::from("/work");
        let raw = json!({
            "id": "x",
            "kind": "terminal",
            "terminal": {
                "command": "x",
                "cwd": "sub/dir"
            }
        });
        let resolved = resolve_launch(&cwd, "x".to_owned(), raw).expect("resolve");
        assert_eq!(resolved.cwd, PathBuf::from("/work").join("sub/dir"));
    }

    #[test]
    fn resolve_launch_renders_shared_terminal_templates() {
        let cwd = PathBuf::from("/work");
        let raw = json!({
            "id": "x",
            "name": "X Agent",
            "kind": "terminal",
            "terminal": {
                "command": "x",
                "interactive_args": ["--agent", "{{agent_id}}"],
                "cwd": "sessions/{{agent_id}}",
                "env": {
                    "AGENT_NAME": "{{agent_name}}",
                    "EMPTY_KEY_IGNORED": "kept"
                }
            }
        });
        let resolved = resolve_launch(&cwd, "x".to_owned(), raw).expect("resolve");
        assert_eq!(resolved.args, vec!["--agent", "x"]);
        assert_eq!(resolved.cwd, PathBuf::from("/work").join("sessions/x"));
        assert_eq!(
            resolved.env.get("AGENT_NAME").map(String::as_str),
            Some("X Agent")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_quoting_handles_spaces_and_quotes() {
        assert_eq!(quote_windows_arg("foo"), "foo");
        assert_eq!(quote_windows_arg("a b"), "\"a b\"");
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_windows_arg("a\\\\b"), "a\\\\b");
        assert_eq!(quote_windows_arg("a\\\"b"), "\"a\\\\\\\"b\"");
    }

    #[cfg(any(unix, target_os = "macos"))]
    #[test]
    fn posix_quoting_handles_spaces_and_quotes() {
        assert_eq!(shell_quote_posix("foo"), "foo");
        assert_eq!(shell_quote_posix("a b"), "'a b'");
        assert_eq!(shell_quote_posix("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote_posix(""), "''");
    }
}
