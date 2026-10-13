//! Codex app-server's newline-delimited JSON-RPC transport. Each invocation
//! owns a child process; durable Codex threads survive process restarts.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::Command;

use crate::agent_terminal_delegate::TerminalEventSink;
use crate::terminal_agent::{
    TerminalAgentEvent, TerminalCommandSpec, TerminalExitReason, TerminalSupervisorResult,
};

// Serialize turns (and workspace cleanup) for the same Savfox session, while
// allowing independent sessions to run concurrently.
pub(crate) async fn session_lock(path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    type Locks = BTreeMap<std::path::PathBuf, Weak<tokio::sync::Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
    let lock = {
        let mut locks = LOCKS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks
            .get(path)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
        locks.insert(path.to_owned(), Arc::downgrade(&lock));
        lock
    };
    lock.lock_owned().await
}

struct Client<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
    limit: usize,
    sink: Option<TerminalEventSink>,
    messages: BTreeMap<String, String>,
    order: Vec<String>,
    thread_id: Option<String>,
    completed: Option<Value>,
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin> Client<R, W> {
    async fn send(&mut self, message: Value) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<Value> {
        let mut bytes = Vec::new();
        let size = (&mut self.reader)
            .take(self.limit as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .await?;
        if size == 0 {
            bail!("Codex app-server closed stdout before completing the turn");
        }
        if size > self.limit {
            bail!("Codex app-server message exceeds the configured output limit");
        }
        serde_json::from_slice(&bytes).context("invalid Codex app-server JSON-RPC message")
    }

    fn emit(&self, event: TerminalAgentEvent) {
        if let Some(sink) = &self.sink {
            sink(event);
        }
    }

    async fn handle(&mut self, message: Value) -> anyhow::Result<()> {
        let Some(method) = message["method"].as_str() else {
            return Ok(());
        };
        if let Some(id) = message.get("id") {
            // Never silently grant permissions on behalf of a gateway user.
            let result = match method {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    json!({"decision": "decline"})
                }
                "item/permissions/requestApproval" => json!({"permissions": {}, "scope": "turn"}),
                "mcpServer/elicitation/request" => json!({"action": "decline", "content": null}),
                _ => {
                    self.send(json!({"id": id, "error": {"code": -32601, "message": "Gateway does not support this Codex server request"}})).await?;
                    bail!("unsupported Codex app-server request: {method}");
                }
            };
            self.send(json!({"id": id, "result": result})).await?;
            self.emit(TerminalAgentEvent::Log {
                stream: "stderr".to_owned(),
                text: format!("Declined Codex request: {method}"),
            });
            return Ok(());
        }
        let params = &message["params"];
        if let Some(thread_id) = params["threadId"].as_str()
            && self.thread_id.as_deref() != Some(thread_id)
        {
            return Ok(());
        }
        match method {
            "item/agentMessage/delta" => {
                let id = params["itemId"]
                    .as_str()
                    .context("Codex delta is missing itemId")?;
                let delta = params["delta"]
                    .as_str()
                    .context("Codex delta is missing text")?;
                if !self.messages.contains_key(id) {
                    self.order.push(id.to_owned());
                }
                let total: usize = self.messages.values().map(String::len).sum();
                if total.saturating_add(delta.len()) > self.limit {
                    bail!("Codex reply exceeds the configured output limit");
                }
                self.messages
                    .entry(id.to_owned())
                    .or_default()
                    .push_str(delta);
                self.emit(TerminalAgentEvent::OutputDelta {
                    stream: "stdout".to_owned(),
                    text: delta.to_owned(),
                });
            }
            "item/completed" if params["item"]["type"] == "agentMessage" => {
                let item = &params["item"];
                let id = item["id"].as_str().context("Codex message is missing id")?;
                let text = item["text"]
                    .as_str()
                    .context("Codex message is missing text")?;
                let total: usize = self
                    .messages
                    .iter()
                    .filter(|(key, _)| key.as_str() != id)
                    .map(|(_, value)| value.len())
                    .sum();
                if total.saturating_add(text.len()) > self.limit {
                    bail!("Codex reply exceeds the configured output limit");
                }
                if !self.messages.contains_key(id) {
                    self.order.push(id.to_owned());
                    self.emit(TerminalAgentEvent::OutputDelta {
                        stream: "stdout".to_owned(),
                        text: text.to_owned(),
                    });
                }
                self.messages.insert(id.to_owned(), text.to_owned());
            }
            "turn/completed" => self.completed = Some(params["turn"].clone()),
            "error" => self.emit(TerminalAgentEvent::Log {
                stream: "stderr".to_owned(),
                text: params.to_string(),
            }),
            _ => {}
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let message = self.receive().await?;
            if message["id"] == id && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    bail!("Codex {method} failed: {error}");
                }
                return message
                    .get("result")
                    .cloned()
                    .context("Codex response is missing result");
            }
            self.handle(message).await?;
        }
    }

    async fn turn(
        &mut self,
        thread_path: &Path,
        cwd: &Path,
        prompt: &str,
        resumed_prompt: &str,
        resume: bool,
    ) -> anyhow::Result<String> {
        self.request("initialize", json!({"clientInfo": {"name": "savfox_gateway", "title": "Savfox Gateway", "version": env!("CARGO_PKG_VERSION")}})).await?;
        self.send(json!({"method": "initialized"})).await?;
        let saved = if resume {
            match tokio::fs::read_to_string(thread_path).await {
                Ok(id) => Some(id),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(err.into()),
            }
        } else {
            None
        };
        let mut params = json!({"cwd": cwd});
        if let Some(id) = &saved {
            params["threadId"] = json!(id.trim());
        }
        let result = self
            .request(
                if saved.is_some() {
                    "thread/resume"
                } else {
                    "thread/start"
                },
                params,
            )
            .await?;
        let id = result["thread"]["id"]
            .as_str()
            .context("Codex response is missing thread.id")?
            .to_owned();
        if resume {
            tokio::fs::write(thread_path, &id).await?;
        }
        self.thread_id = Some(id.clone());
        self.request("turn/start", json!({"threadId": id, "input": [{"type": "text", "text": if saved.is_some() { resumed_prompt } else { prompt }}]})).await?;
        loop {
            if let Some(turn) = self.completed.take() {
                if turn["status"] != "completed" {
                    bail!("Codex turn did not complete successfully: {turn}");
                }
                return Ok(self
                    .order
                    .iter()
                    .filter_map(|id| self.messages.get(id))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n\n"));
            }
            let message = self.receive().await?;
            self.handle(message).await?;
        }
    }
}

pub(crate) async fn run(
    spec: TerminalCommandSpec,
    thread_path: &Path,
    values: &crate::terminal_agent::TerminalTemplateValues,
    resume: bool,
    sink: Option<TerminalEventSink>,
) -> TerminalSupervisorResult {
    let mut result = TerminalSupervisorResult {
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        pid: None,
        exit_code: None,
        exit_reason: TerminalExitReason::SpawnError,
        error: None,
    };
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            result.error = Some(format!("failed to start Codex app-server: {err}"));
            return result;
        }
    };
    result.pid = child.id();
    let stderr = child.stderr.take().expect("piped stderr");
    let limit = spec.max_output_bytes;
    // Drain continuously even after reaching the retained-log limit.
    let mut stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut retained = Vec::new();
        let mut buffer = [0u8; 4096];
        let mut truncated = false;
        loop {
            let size = reader.read(&mut buffer).await?;
            if size == 0 {
                break;
            }
            let keep = size.min(limit.saturating_sub(retained.len()));
            retained.extend_from_slice(&buffer[..keep]);
            truncated |= keep < size;
        }
        Ok::<_, std::io::Error>((String::from_utf8_lossy(&retained).into_owned(), truncated))
    });
    let _stderr_guard = AbortOnDrop(stderr_task.abort_handle());
    let mut client = Client {
        reader: BufReader::new(child.stdout.take().expect("piped stdout")),
        writer: child.stdin.take().expect("piped stdin"),
        next_id: 1,
        limit,
        sink,
        messages: BTreeMap::new(),
        order: Vec::new(),
        thread_id: None,
        completed: None,
    };
    let resumed_prompt = format!("{}\n\n{}", values.prompt, values.attachment_manifest);
    match tokio::time::timeout(
        spec.timeout,
        client.turn(
            thread_path,
            &spec.cwd,
            &values.full_prompt,
            &resumed_prompt,
            resume,
        ),
    )
    .await
    {
        Ok(Ok(reply)) => {
            result.stdout = reply;
            result.exit_reason = TerminalExitReason::Completed;
            result.exit_code = Some(0);
        }
        Ok(Err(err)) => {
            result.exit_reason = TerminalExitReason::IoError;
            result.error = Some(err.to_string());
        }
        Err(_) => {
            result.exit_reason = TerminalExitReason::Timeout;
            result.error = Some("Codex app-server turn timed out".to_owned());
        }
    }
    // Closing stdin lets app-server flush its durable thread history before
    // exiting. Failed or timed-out turns still require immediate termination.
    drop(client);
    let graceful_exit = result.exit_reason.is_success()
        && matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await,
            Ok(Ok(_))
        );
    if !graceful_exit {
        let _ = child.kill().await;
    }
    if let Ok(Ok(Ok((stderr, truncated)))) =
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut stderr_task).await
    {
        result.stderr = stderr;
        result.stderr_truncated = truncated;
    } else {
        stderr_task.abort();
        result.stderr_truncated = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;

    fn client(
        stream: DuplexStream,
    ) -> Client<BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>> {
        let (reader, writer) = tokio::io::split(stream);
        Client {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
            limit: 4096,
            sink: None,
            messages: BTreeMap::new(),
            order: Vec::new(),
            thread_id: None,
            completed: None,
        }
    }

    async fn read(reader: &mut BufReader<ReadHalf<DuplexStream>>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read request");
        serde_json::from_str(&line).expect("JSON request")
    }

    async fn write(writer: &mut WriteHalf<DuplexStream>, value: Value) {
        writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .expect("write response");
    }

    #[tokio::test]
    async fn handshake_streaming_and_resume_preserve_thread_and_do_not_duplicate_history() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("thread");
        for (resume, status, retain) in [
            (false, "completed", true),
            (true, "completed", true),
            (true, "failed", true),
            (true, "interrupted", true),
            (false, "completed", false),
        ] {
            let (local, remote) = tokio::io::duplex(8192);
            let mut client = client(local);
            let events = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&events);
            client.sink = Some(Arc::new(move |event| {
                captured.lock().expect("event lock").push(event);
            }));
            let fake = tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(remote);
                let mut reader = BufReader::new(reader);
                let init = read(&mut reader).await;
                assert_eq!(init["method"], "initialize");
                assert_eq!(init["params"]["clientInfo"]["name"], "savfox_gateway");
                assert!(init.get("jsonrpc").is_none());
                write(&mut writer, json!({"id": init["id"], "result": {}})).await;
                assert_eq!(read(&mut reader).await["method"], "initialized");
                let thread = read(&mut reader).await;
                assert_eq!(
                    thread["method"],
                    if resume {
                        "thread/resume"
                    } else {
                        "thread/start"
                    }
                );
                if resume {
                    assert_eq!(thread["params"]["threadId"], "thread-1");
                }
                write(
                    &mut writer,
                    json!({"id": thread["id"], "result": {"thread": {"id": "thread-1"}}}),
                )
                .await;
                let turn = read(&mut reader).await;
                assert_eq!(turn["method"], "turn/start");
                assert_eq!(
                    turn["params"]["input"][0]["text"],
                    if resume { "next" } else { "history and first" }
                );
                write(
                    &mut writer,
                    json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}),
                )
                .await;
                write(&mut writer, json!({"method": "item/agentMessage/delta", "params": {"threadId": "other", "itemId": "ignored", "delta": "wrong"}})).await;
                write(&mut writer, json!({"method": "item/agentMessage/delta", "params": {"threadId": "thread-1", "itemId": "item-1", "delta": "hel"}})).await;
                write(&mut writer, json!({"method": "item/agentMessage/delta", "params": {"threadId": "thread-1", "itemId": "item-1", "delta": "lo"}})).await;
                write(&mut writer, json!({"method": "item/completed", "params": {"threadId": "thread-1", "item": {"type": "agentMessage", "id": "item-1", "text": "hello"}}})).await;
                write(&mut writer, json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"status": status}}})).await;
            });
            let reply = client
                .turn(&path, dir.path(), "history and first", "next", retain)
                .await;
            if status == "completed" {
                assert_eq!(reply.expect("successful turn"), "hello");
            } else {
                assert!(reply.unwrap_err().to_string().contains(status));
            }
            assert_eq!(
                tokio::fs::read_to_string(&path)
                    .await
                    .expect("saved thread"),
                "thread-1"
            );
            fake.await.expect("fake server");
            assert_eq!(
                *events.lock().expect("event lock"),
                vec![
                    TerminalAgentEvent::OutputDelta {
                        stream: "stdout".to_owned(),
                        text: "hel".to_owned()
                    },
                    TerminalAgentEvent::OutputDelta {
                        stream: "stdout".to_owned(),
                        text: "lo".to_owned()
                    },
                ]
            );
        }
    }

    #[tokio::test]
    async fn approvals_are_declined_and_unknown_requests_fail_explicitly() {
        let (local, remote) = tokio::io::duplex(8192);
        let mut client = client(local);
        let (reader, _writer) = tokio::io::split(remote);
        let mut reader = BufReader::new(reader);
        client
            .handle(json!({"id": "approval-1", "method": "item/commandExecution/requestApproval"}))
            .await
            .expect("decline approval");
        assert_eq!(
            read(&mut reader).await,
            json!({"id": "approval-1", "result": {"decision": "decline"}})
        );
        assert!(
            client
                .handle(json!({"id": 99, "method": "item/tool/requestUserInput"}))
                .await
                .is_err()
        );
        assert_eq!(read(&mut reader).await["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn oversized_frames_and_premature_eof_fail() {
        let (local, mut remote) = tokio::io::duplex(8192);
        let mut client = client(local);
        client.limit = 8;
        remote
            .write_all(b"0123456789\n")
            .await
            .expect("write oversized frame");
        assert!(
            client
                .receive()
                .await
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
        drop(remote);
        let (local, remote) = tokio::io::duplex(8192);
        drop(remote);
        assert!(
            client_from_stream_eof(local)
                .await
                .contains("closed stdout")
        );
    }

    async fn client_from_stream_eof(stream: DuplexStream) -> String {
        client(stream).receive().await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn successful_subprocess_exits_on_stdin_eof_before_returning() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("graceful-exit");
        #[cfg(windows)]
        let (program, args) = {
            let script = dir.path().join("fake-app-server.ps1");
            tokio::fs::write(&script, r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    switch ($request.method) {
        'initialize' { [Console]::Out.WriteLine('{"id":1,"result":{}}') }
        'thread/start' { [Console]::Out.WriteLine('{"id":2,"result":{"thread":{"id":"thread-1"}}}') }
        'turn/start' {
            [Console]::Out.WriteLine('{"id":3,"result":{"turn":{"id":"turn-1"}}}')
            [Console]::Out.WriteLine('{"method":"item/completed","params":{"threadId":"thread-1","item":{"type":"agentMessage","id":"item-1","text":"hello"}}}')
            [Console]::Out.WriteLine('{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"status":"completed"}}}')
        }
    }
    [Console]::Out.Flush()
}
[IO.File]::WriteAllText($env:EXIT_MARKER, 'finished')
"#).await.expect("write fake server");
            (
                "powershell",
                vec![
                    "-NoProfile".to_owned(),
                    "-File".to_owned(),
                    script.to_string_lossy().into_owned(),
                ],
            )
        };
        #[cfg(not(windows))]
        let (program, args) = ("sh", vec!["-c".to_owned(), r#"
while IFS= read -r line; do
    case "$line" in
        *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
        *'"method":"thread/start"'*) printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-1"}}}' ;;
        *'"method":"turn/start"'*)
            printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-1"}}}'
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","item":{"type":"agentMessage","id":"item-1","text":"hello"}}}'
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"status":"completed"}}}' ;;
    esac
done
printf finished > "$EXIT_MARKER"
"#.to_owned()]);
        let spec = TerminalCommandSpec {
            program: program.to_owned(),
            args,
            cwd: dir.path().to_owned(),
            env: BTreeMap::from([(
                "EXIT_MARKER".to_owned(),
                marker.to_string_lossy().into_owned(),
            )]),
            stdin: None,
            timeout: std::time::Duration::from_secs(10),
            max_output_bytes: 4096,
        };
        let result = run(
            spec,
            &dir.path().join("thread"),
            &crate::terminal_agent::TerminalTemplateValues::default(),
            true,
            None,
        )
        .await;
        assert_eq!(
            result.exit_reason,
            TerminalExitReason::Completed,
            "{:?}",
            result.error
        );
        assert_eq!(result.stdout, "hello");
        assert_eq!(
            tokio::fs::read_to_string(marker)
                .await
                .expect("graceful exit marker"),
            "finished"
        );
    }

    #[tokio::test]
    async fn subprocess_timeout_and_spawn_failure_are_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        #[cfg(windows)]
        let (program, args) = (
            "powershell",
            vec!["-NoProfile", "-Command", "Start-Sleep -Seconds 60"],
        );
        #[cfg(not(windows))]
        let (program, args) = ("sh", vec!["-c", "read first; read second"]);
        let mut spec = TerminalCommandSpec {
            program: program.to_owned(),
            args: args.into_iter().map(str::to_owned).collect(),
            cwd: dir.path().to_owned(),
            env: BTreeMap::new(),
            stdin: None,
            timeout: std::time::Duration::from_millis(50),
            max_output_bytes: 4096,
        };
        let values = crate::terminal_agent::TerminalTemplateValues::default();
        let result = run(
            spec.clone(),
            &dir.path().join("thread"),
            &values,
            true,
            None,
        )
        .await;
        assert_eq!(result.exit_reason, TerminalExitReason::Timeout);
        assert!(result.pid.is_some());
        assert!(!dir.path().join("thread").exists());
        spec.program = dir
            .path()
            .join("missing-codex-executable")
            .to_string_lossy()
            .into_owned();
        let result = run(spec, &dir.path().join("thread"), &values, true, None).await;
        assert_eq!(result.exit_reason, TerminalExitReason::SpawnError);
        assert!(result.pid.is_none());
    }
}
