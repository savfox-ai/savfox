use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use savfox_core::config::Config;
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{
    AgentMessageEvent, EventMsg, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine,
    SessionModel, SessionSource, UserMessageEvent,
};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::warn;

use crate::channel::{AgentInvocationResult, GatewayChannel};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AgentTerminalDelegateConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    pub(crate) stdin: Option<String>,
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) timeout_secs: Option<u64>,
    #[serde(default = "default_include_system_prompt")]
    pub(crate) include_system_prompt: bool,
    #[serde(default)]
    pub(crate) interactive_command: Option<String>,
    #[serde(default)]
    pub(crate) interactive_args: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedTerminalDelegate {
    config: AgentTerminalDelegateConfig,
    agent_id: String,
    agent_name: String,
    system_prompt: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TerminalDelegateInvocation {
    pub(crate) result: AgentInvocationResult,
    pub(crate) model: String,
    pub(crate) provider: String,
}

#[derive(Debug, Clone)]
struct PromptTemplateValues<'a> {
    prompt: &'a str,
    full_prompt: String,
    system_prompt: &'a str,
    agent_id: &'a str,
    agent_name: &'a str,
    model: &'a str,
}

fn default_include_system_prompt() -> bool {
    true
}

fn sanitize_agent_file_stem(raw: &str) -> Option<String> {
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
    if out.is_empty() || out == "." || out == ".." {
        None
    } else {
        Some(out)
    }
}

async fn read_agent_config(path: &Path) -> Option<serde_json::Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

fn agents_dir(config: &Config) -> PathBuf {
    config.savfox_home.join("agents")
}

pub(crate) async fn resolve_agent_config(
    config: &Config,
    agent_ref: &str,
) -> Option<(String, serde_json::Value)> {
    let dir = agents_dir(config);
    let trimmed = agent_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed) {
        let direct = dir.join(format!("{safe_ref}.json"));
        if let Some(config) = read_agent_config(&direct).await {
            return Some((safe_ref, config));
        }
    }

    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }

        let Some(stem) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            continue;
        };

        if stem.eq_ignore_ascii_case(trimmed)
            && let Some(config) = read_agent_config(&path).await
        {
            return Some((stem, config));
        }

        let Some(config) = read_agent_config(&path).await else {
            continue;
        };

        let id_match = config
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|id| id.eq_ignore_ascii_case(trimmed));
        let name_match = config
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        let identity_match = config
            .get("identity")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));

        if id_match || name_match || identity_match {
            return Some((stem, config));
        }
    }

    None
}

fn terminal_delegate_from_agent_config(
    file_stem: String,
    config: serde_json::Value,
) -> Option<ResolvedTerminalDelegate> {
    let delegate_value = config.get("terminal_delegate")?;
    let delegate: AgentTerminalDelegateConfig =
        serde_json::from_value(delegate_value.clone()).ok()?;
    if !delegate.enabled {
        return None;
    }
    if delegate
        .command
        .as_deref()
        .map(str::trim)
        .is_none_or(|value| value.is_empty())
    {
        return None;
    }

    let agent_id = config
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(file_stem.as_str())
        .to_owned();
    let agent_name = config
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(agent_id.as_str())
        .to_owned();
    let system_prompt = config
        .get("system_prompt")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned();

    Some(ResolvedTerminalDelegate {
        config: delegate,
        agent_id,
        agent_name,
        system_prompt,
    })
}

fn render_template(template: &str, values: &PromptTemplateValues<'_>) -> String {
    template
        .replace("{{prompt}}", &values.full_prompt)
        .replace("{{user_prompt}}", values.prompt)
        .replace("{{system_prompt}}", values.system_prompt)
        .replace("{{agent_id}}", values.agent_id)
        .replace("{{agent_name}}", values.agent_name)
        .replace("{{model}}", values.model)
}

fn build_prompt_values<'a>(
    delegate: &'a ResolvedTerminalDelegate,
    prompt: &'a str,
    model: &'a str,
) -> PromptTemplateValues<'a> {
    let full_prompt =
        if delegate.config.include_system_prompt && !delegate.system_prompt.trim().is_empty() {
            format!(
                "{}\n\nUser request:\n{}",
                delegate.system_prompt.trim(),
                prompt.trim()
            )
        } else {
            prompt.to_owned()
        };

    PromptTemplateValues {
        prompt,
        full_prompt,
        system_prompt: &delegate.system_prompt,
        agent_id: &delegate.agent_id,
        agent_name: &delegate.agent_name,
        model,
    }
}

fn rendered_args(
    delegate: &ResolvedTerminalDelegate,
    values: &PromptTemplateValues<'_>,
) -> Vec<String> {
    if delegate.config.args.is_empty() && delegate.config.stdin.is_none() {
        return vec![values.full_prompt.clone()];
    }

    delegate
        .config
        .args
        .iter()
        .map(|arg| render_template(arg, values))
        .collect()
}

async fn read_pipe_limited<R>(reader: R) -> std::io::Result<(String, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut reader = reader.take((MAX_OUTPUT_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    if truncated {
        bytes.truncate(MAX_OUTPUT_BYTES);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

async fn run_command(
    config: &Config,
    delegate: &ResolvedTerminalDelegate,
    prompt: &str,
    model: &str,
) -> anyhow::Result<String> {
    let values = build_prompt_values(delegate, prompt, model);
    let command = delegate
        .config
        .command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .expect("resolved terminal delegate command is present");
    let args = rendered_args(delegate, &values);
    let timeout_secs = delegate
        .config
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .max(MIN_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    let mut cmd = Command::new(command);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let stdin_text = delegate
        .config
        .stdin
        .as_deref()
        .map(|template| render_template(template, &values));
    if stdin_text.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let cwd = delegate
        .config
        .cwd
        .as_deref()
        .map(|cwd| render_template(cwd, &values))
        .map(PathBuf::from)
        .unwrap_or_else(|| config.cwd.clone());
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        config.cwd.join(cwd)
    };
    cmd.current_dir(cwd);

    for (key, value) in &delegate.config.env {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        cmd.env(key, render_template(value, &values));
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| anyhow::anyhow!("failed to start terminal delegate `{command}`: {err}"))?;

    if let Some(stdin_text) = stdin_text
        && let Some(mut stdin) = child.stdin.take()
    {
        tokio::spawn(async move {
            let _ = stdin.write_all(stdin_text.as_bytes()).await;
        });
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = stdout.map(|stdout| tokio::spawn(read_pipe_limited(stdout)));
    let stderr_task = stderr.map(|stderr| tokio::spawn(read_pipe_limited(stderr)));

    let status = if let Ok(result) = tokio::time::timeout(timeout, child.wait()).await {
        result.map_err(|err| anyhow::anyhow!("terminal delegate wait failed: {err}"))?
    } else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(anyhow::anyhow!(
            "terminal delegate `{command}` timed out after {timeout_secs}s"
        ));
    };

    let (stdout, stdout_truncated) = match stdout_task {
        Some(task) => task
            .await
            .map_err(|err| anyhow::anyhow!("terminal delegate stdout task failed: {err}"))?
            .map_err(|err| anyhow::anyhow!("failed to read terminal delegate stdout: {err}"))?,
        None => (String::new(), false),
    };
    let (stderr, stderr_truncated) = match stderr_task {
        Some(task) => task
            .await
            .map_err(|err| anyhow::anyhow!("terminal delegate stderr task failed: {err}"))?
            .map_err(|err| anyhow::anyhow!("failed to read terminal delegate stderr: {err}"))?,
        None => (String::new(), false),
    };

    let mut stdout = stdout;
    let mut stderr = stderr;
    if stdout_truncated {
        stdout.push_str("\n[stdout truncated]");
    }
    if stderr_truncated {
        stderr.push_str("\n[stderr truncated]");
    }

    if !status.success() {
        let code = status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_owned());
        let stderr_preview = stderr.trim();
        let stdout_preview = stdout.trim();
        let detail = if !stderr_preview.is_empty() {
            stderr_preview
        } else if !stdout_preview.is_empty() {
            stdout_preview
        } else {
            "no output"
        };
        return Err(anyhow::anyhow!(
            "terminal delegate `{command}` exited with {code}: {detail}"
        ));
    }

    let reply = stdout.trim_end();
    if !reply.is_empty() {
        return Ok(reply.to_owned());
    }

    let stderr = stderr.trim_end();
    if !stderr.is_empty() {
        return Ok(stderr.to_owned());
    }

    Ok("(no response from terminal delegate)".to_owned())
}

fn rollout_timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn rollout_line(item: RolloutItem) -> RolloutLine {
    RolloutLine {
        timestamp: rollout_timestamp(),
        item,
    }
}

async fn write_rollout_line(file: &mut tokio::fs::File, item: RolloutItem) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&rollout_line(item))?;
    line.push('\n');
    file.write_all(line.as_bytes()).await?;
    file.flush().await
}

async fn persist_terminal_delegate_rollout(
    config: &Config,
    prompt: &str,
    reply: &str,
    session_id: &str,
    model: &str,
    provider: &str,
) -> std::io::Result<PathBuf> {
    let session_id = SessionId::from_string(session_id)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let session_id_string = session_id.to_string();
    let sessions_dir = config.savfox_home.join("sessions");
    tokio::fs::create_dir_all(&sessions_dir).await?;
    let rollout_path = sessions_dir.join(format!("{session_id_string}.jsonl"));
    let needs_meta = tokio::fs::metadata(&rollout_path)
        .await
        .map(|metadata| metadata.len() == 0)
        .unwrap_or(true);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rollout_path)
        .await?;

    if needs_meta {
        write_rollout_line(
            &mut file,
            RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: session_id,
                    forked_from_id: None,
                    timestamp: rollout_timestamp(),
                    cwd: config.cwd.clone(),
                    originator: "savfox-gateway".to_owned(),
                    cli_version: env!("CARGO_PKG_VERSION").to_owned(),
                    source: SessionSource::VSCode,
                    model: Some(SessionModel {
                        provider: provider.to_owned(),
                        model_slug: model.to_owned(),
                    }),
                    model_provider: None,
                    base_instructions: None,
                    dynamic_tools: None,
                },
                git: None,
            }),
        )
        .await?;
    }

    write_rollout_line(
        &mut file,
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: prompt.to_owned(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
    )
    .await?;
    write_rollout_line(
        &mut file,
        RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
            message: reply.to_owned(),
        })),
    )
    .await?;

    Ok(rollout_path)
}

impl GatewayChannel {
    pub(crate) async fn resolve_terminal_delegate(
        &self,
        agent_ref: &str,
    ) -> Option<ResolvedTerminalDelegate> {
        let (file_stem, config) = resolve_agent_config(self.config(), agent_ref).await?;
        terminal_delegate_from_agent_config(file_stem, config)
    }

    pub(crate) async fn invoke_terminal_delegate_agent(
        &self,
        prompt: &str,
        agent_ref: &str,
        model: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<Option<TerminalDelegateInvocation>> {
        let Some(delegate) = self.resolve_terminal_delegate(agent_ref).await else {
            return Ok(None);
        };

        let reply = run_command(self.config(), &delegate, prompt, model).await?;
        let session_id = session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let model = format!("terminal/{}", delegate.agent_id);
        let provider = "terminal".to_owned();
        let rollout_path = match persist_terminal_delegate_rollout(
            self.config(),
            prompt,
            &reply,
            &session_id,
            &model,
            &provider,
        )
        .await
        {
            Ok(path) => Some(path),
            Err(err) => {
                warn!("failed to persist terminal delegate rollout: {err}");
                None
            }
        };
        let result = AgentInvocationResult {
            reply,
            session_id: session_id.clone(),
            thread_id: session_id,
            rollout_path,
            last_token_usage: None,
        };

        Ok(Some(TerminalDelegateInvocation {
            result,
            model,
            provider,
        }))
    }
}

#[cfg(test)]
mod tests {
    use savfox_protocol::protocol::{AgentMessageEvent, EventMsg, RolloutItem};
    use serde_json::json;

    use super::{
        PromptTemplateValues, ResolvedTerminalDelegate, terminal_delegate_from_agent_config,
        write_rollout_line,
    };
    use crate::agent_terminal_delegate::{
        AgentTerminalDelegateConfig, render_template, rendered_args,
    };

    #[test]
    fn terminal_delegate_requires_enabled_command() {
        let config = json!({
            "id": "cli",
            "name": "CLI",
            "terminal_delegate": {
                "enabled": true,
                "command": "claude"
            }
        });

        let delegate = terminal_delegate_from_agent_config("cli".to_owned(), config)
            .expect("enabled delegate with command should resolve");

        assert_eq!(delegate.agent_id, "cli");
        assert_eq!(delegate.agent_name, "CLI");
    }

    #[test]
    fn terminal_delegate_ignores_disabled_config() {
        let config = json!({
            "id": "cli",
            "terminal_delegate": {
                "enabled": false,
                "command": "claude"
            }
        });

        assert!(terminal_delegate_from_agent_config("cli".to_owned(), config).is_none());
    }

    #[test]
    fn render_template_supports_prompt_and_metadata() {
        let values = PromptTemplateValues {
            prompt: "hello",
            full_prompt: "system\n\nUser request:\nhello".to_owned(),
            system_prompt: "system",
            agent_id: "cli",
            agent_name: "CLI",
            model: "default",
        };

        assert_eq!(
            render_template("{{agent_name}}/{{agent_id}} {{model}} {{prompt}}", &values),
            "CLI/cli default system\n\nUser request:\nhello"
        );
        assert_eq!(
            render_template("{{user_prompt}} -- {{system_prompt}}", &values),
            "hello -- system"
        );
    }

    #[test]
    fn rendered_args_default_to_prompt_when_no_stdin_or_args() {
        let delegate = ResolvedTerminalDelegate {
            config: AgentTerminalDelegateConfig {
                enabled: true,
                command: Some("tool".to_owned()),
                args: Vec::new(),
                stdin: None,
                cwd: None,
                env: Default::default(),
                timeout_secs: None,
                include_system_prompt: true,
                interactive_command: None,
                interactive_args: None,
            },
            agent_id: "cli".to_owned(),
            agent_name: "CLI".to_owned(),
            system_prompt: String::new(),
        };
        let values = PromptTemplateValues {
            prompt: "hello",
            full_prompt: "hello".to_owned(),
            system_prompt: "",
            agent_id: "cli",
            agent_name: "CLI",
            model: "default",
        };

        assert_eq!(rendered_args(&delegate, &values), vec!["hello"]);
    }

    #[tokio::test]
    async fn write_rollout_line_persists_parseable_event() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("session.jsonl");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .expect("open rollout file");

        write_rollout_line(
            &mut file,
            RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: "from cli".to_owned(),
            })),
        )
        .await
        .expect("write rollout line");

        let text = tokio::fs::read_to_string(path)
            .await
            .expect("read rollout file");
        let line: savfox_protocol::protocol::RolloutLine =
            serde_json::from_str(text.trim()).expect("parse rollout line");
        match line.item {
            RolloutItem::EventMsg(EventMsg::AgentMessage(message)) => {
                assert_eq!(message.message, "from cli");
            }
            other => panic!("unexpected rollout item: {other:?}"),
        }
    }
}
