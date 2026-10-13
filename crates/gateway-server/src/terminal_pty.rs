use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, RwLock, broadcast, oneshot};
use tokio::time::Instant;

type BoxedPtyReader = Pin<Box<dyn AsyncRead + Send>>;

const SENTINEL_COMPLETE: &str = "::savfox-complete";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPtySize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Default for TerminalPtySize {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 30,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalPtySpawnSpec {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) size: TerminalPtySize,
}

impl TerminalPtySpawnSpec {
    pub(crate) fn command_display(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.program.clone());
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

pub(crate) struct TerminalPtyProcess {
    child: Box<dyn TerminalPtyChild>,
    stdout: BoxedPtyReader,
    stderr: Option<BoxedPtyReader>,
    backend: String,
    native_pty: bool,
    native_output: Option<(broadcast::Receiver<Vec<u8>>, oneshot::Receiver<i32>)>,
}

/// Uses the same platform PTY implementation as core unified_exec.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeTerminalPtyBackend;

#[async_trait]
impl TerminalPtyBackend for NativeTerminalPtyBackend {
    async fn spawn(&self, spec: TerminalPtySpawnSpec) -> io::Result<TerminalPtyProcess> {
        if !tokio::fs::metadata(&spec.cwd).await?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal cwd is not a directory",
            ));
        }
        // The shared PTY helper intentionally clears the environment. Gateway
        // commands inherit the host environment, with configured overrides.
        let mut env: HashMap<String, String> = std::env::vars().collect();
        for (key, value) in spec.env {
            // Windows environment names are case-insensitive.
            if cfg!(windows) {
                env.retain(|existing, _| !existing.eq_ignore_ascii_case(&key));
            }
            env.insert(key, value);
        }
        env.entry("TERM".to_owned())
            .or_insert_with(|| "xterm-256color".to_owned());
        let spawned = savfox_utils::pty::pty::spawn_process_with_size(
            &spec.program,
            &spec.args,
            &spec.cwd,
            &env,
            &None,
            spec.size.rows,
            spec.size.cols,
        )
        .await
        .map_err(io::Error::other)?;
        Ok(TerminalPtyProcess {
            child: Box::new(NativeTerminalPtyChild(spawned.session)),
            stdout: Box::pin(tokio::io::empty()),
            stderr: None,
            backend: if cfg!(windows) { "conpty" } else { "unix_pty" }.to_owned(),
            native_pty: true,
            native_output: Some((spawned.output_rx, spawned.exit_rx)),
        })
    }
}

struct NativeTerminalPtyChild(savfox_utils::pty::ProcessHandle);

#[async_trait]
impl TerminalPtyChild for NativeTerminalPtyChild {
    fn pid(&self) -> Option<u32> {
        self.0.process_id()
    }

    async fn write(&mut self, input: &[u8]) -> io::Result<()> {
        if self.0.has_exited() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal process exited",
            ));
        }
        self.0
            .writer_sender()
            .send(input.to_vec())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "terminal input closed"))
    }

    async fn resize(&mut self, size: TerminalPtySize) -> io::Result<()> {
        self.0.resize(size.rows, size.cols)
    }

    async fn kill(&mut self) -> io::Result<()> {
        self.0.terminate();
        Ok(())
    }
}

#[async_trait]
pub(crate) trait TerminalPtyBackend: Send + Sync {
    async fn spawn(&self, spec: TerminalPtySpawnSpec) -> io::Result<TerminalPtyProcess>;
}

#[async_trait]
pub(crate) trait TerminalPtyChild: Send {
    fn pid(&self) -> Option<u32>;
    async fn write(&mut self, input: &[u8]) -> io::Result<()>;
    async fn resize(&mut self, size: TerminalPtySize) -> io::Result<()>;
    async fn kill(&mut self) -> io::Result<()>;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessBackedTerminalPtyBackend;

#[async_trait]
impl TerminalPtyBackend for ProcessBackedTerminalPtyBackend {
    async fn spawn(&self, spec: TerminalPtySpawnSpec) -> io::Result<TerminalPtyProcess> {
        let program = spec.program.trim();
        if program.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing managed terminal command",
            ));
        }

        let metadata = tokio::fs::metadata(&spec.cwd).await.map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid managed terminal cwd `{}`: {err}",
                    spec.cwd.display()
                ),
            )
        })?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid managed terminal cwd `{}`: not a directory",
                    spec.cwd.display()
                ),
            ));
        }

        let mut command = Command::new(program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in &spec.env {
            let key = key.trim();
            if !key.is_empty() {
                command.env(key, value);
            }
        }

        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("managed terminal process did not provide stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("managed terminal process did not provide stdout"))?;
        let stderr = child
            .stderr
            .take()
            .map(|stderr| Box::pin(stderr) as BoxedPtyReader);

        Ok(TerminalPtyProcess {
            child: Box::new(ProcessBackedTerminalPtyChild {
                child,
                stdin,
                size: spec.size,
            }),
            stdout: Box::pin(stdout),
            stderr,
            backend: "process_backed".to_owned(),
            native_pty: false,
            native_output: None,
        })
    }
}

struct ProcessBackedTerminalPtyChild {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    size: TerminalPtySize,
}

#[async_trait]
impl TerminalPtyChild for ProcessBackedTerminalPtyChild {
    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    async fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.stdin.write_all(input).await?;
        self.stdin.flush().await
    }

    async fn resize(&mut self, size: TerminalPtySize) -> io::Result<()> {
        self.size = size;
        Ok(())
    }

    async fn kill(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct TerminalPtySessionKey {
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
}

impl TerminalPtySessionKey {
    pub(crate) fn new(agent_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalPtyWriteKind {
    Text,
    Line,
    Newline,
    Interrupt,
    ControlSequence,
    ManualComplete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPtyWrite {
    pub(crate) kind: TerminalPtyWriteKind,
    pub(crate) text: String,
}

impl TerminalPtyWrite {
    pub(crate) fn line(text: impl Into<String>) -> Self {
        Self {
            kind: TerminalPtyWriteKind::Line,
            text: text.into(),
        }
    }

    pub(crate) fn manual_complete() -> Self {
        Self {
            kind: TerminalPtyWriteKind::ManualComplete,
            text: String::new(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        match self.kind {
            TerminalPtyWriteKind::Text | TerminalPtyWriteKind::ControlSequence => {
                self.text.as_bytes().to_vec()
            }
            TerminalPtyWriteKind::Line => {
                let mut bytes = self.text.as_bytes().to_vec();
                bytes.push(b'\n');
                bytes
            }
            TerminalPtyWriteKind::Newline => b"\n".to_vec(),
            TerminalPtyWriteKind::Interrupt => vec![0x03],
            TerminalPtyWriteKind::ManualComplete => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPtyTranscriptEntry {
    pub(crate) sequence: u64,
    pub(crate) stream: String,
    pub(crate) text: String,
    pub(crate) at: String,
}

#[derive(Clone, Debug)]
struct TerminalPtyTranscript {
    entries: Vec<TerminalPtyTranscriptEntry>,
    next_sequence: u64,
    total_bytes: usize,
    max_bytes: usize,
}

impl TerminalPtyTranscript {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: Vec::new(),
            next_sequence: 1,
            total_bytes: 0,
            max_bytes,
        }
    }

    fn append(&mut self, stream: &str, text: String) {
        let text_bytes = text.len();
        self.entries.push(TerminalPtyTranscriptEntry {
            sequence: self.next_sequence,
            stream: stream.to_owned(),
            text,
            at: now_timestamp(),
        });
        self.next_sequence += 1;
        self.total_bytes += text_bytes;
        while self.total_bytes > self.max_bytes && self.entries.len() > 1 {
            let removed = self.entries.remove(0);
            self.total_bytes = self.total_bytes.saturating_sub(removed.text.len());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalPtyCompletion {
    Running,
    Sentinel,
    Manual,
    IdleTimeout,
    ExplicitClose,
    GatewayShutdown,
    Exited,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalPtyReconnectState {
    Attached,
    NeedsManualRebind,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPtyReconnectHint {
    pub(crate) state: TerminalPtyReconnectState,
    pub(crate) can_reconnect_in_gateway: bool,
    pub(crate) requires_manual_rebind_after_restart: bool,
    pub(crate) message: String,
}

impl TerminalPtyReconnectHint {
    fn attached(backend: &str, native_pty: bool) -> Self {
        Self {
            state: TerminalPtyReconnectState::Attached,
            can_reconnect_in_gateway: true,
            requires_manual_rebind_after_restart: true,
            message: format!(
                "{backend} session is attached to this gateway process; native_pty={native_pty}"
            ),
        }
    }

    fn closed(completion: &TerminalPtyCompletion) -> Self {
        Self {
            state: TerminalPtyReconnectState::Closed,
            can_reconnect_in_gateway: false,
            requires_manual_rebind_after_restart: false,
            message: format!("managed terminal session is closed: {completion:?}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPtySessionMetadata {
    pub(crate) key: TerminalPtySessionKey,
    pub(crate) backend: String,
    pub(crate) native_pty: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) command: String,
    pub(crate) cwd: String,
    pub(crate) size: TerminalPtySize,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) last_activity_at: String,
    pub(crate) completed_at: Option<String>,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) transcript_entries: usize,
    pub(crate) transcript_bytes: usize,
    pub(crate) completion: TerminalPtyCompletion,
    pub(crate) turn_pending: bool,
    pub(crate) reconnect: TerminalPtyReconnectHint,
}

pub(crate) struct TerminalPtySession {
    spec: TerminalPtySpawnSpec,
    child: Mutex<Box<dyn TerminalPtyChild>>,
    transcript: Mutex<TerminalPtyTranscript>,
    metadata: RwLock<TerminalPtySessionMetadata>,
    last_activity: Mutex<Instant>,
    notify: Notify,
    turn: Mutex<()>,
    sentinel_lines: Mutex<BTreeMap<String, String>>,
}

impl TerminalPtySession {
    fn new(
        key: TerminalPtySessionKey,
        spec: &TerminalPtySpawnSpec,
        mut process: TerminalPtyProcess,
        idle_timeout: Duration,
        transcript_limit_bytes: usize,
    ) -> (Arc<Self>, Option<BoxedPtyReader>, Option<BoxedPtyReader>) {
        let now = now_timestamp();
        let pid = process.child.pid();
        let metadata = TerminalPtySessionMetadata {
            key,
            backend: process.backend.clone(),
            native_pty: process.native_pty,
            pid,
            exit_code: None,
            command: spec.command_display(),
            cwd: spec.cwd.display().to_string(),
            size: spec.size,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_activity_at: now,
            completed_at: None,
            idle_timeout_ms: duration_millis_u64(idle_timeout),
            transcript_entries: 0,
            transcript_bytes: 0,
            completion: TerminalPtyCompletion::Running,
            turn_pending: false,
            reconnect: TerminalPtyReconnectHint::attached(&process.backend, process.native_pty),
        };
        let session = Arc::new(Self {
            spec: spec.clone(),
            child: Mutex::new(process.child),
            transcript: Mutex::new(TerminalPtyTranscript::new(transcript_limit_bytes)),
            metadata: RwLock::new(metadata),
            last_activity: Mutex::new(Instant::now()),
            notify: Notify::new(),
            turn: Mutex::new(()),
            sentinel_lines: Mutex::new(BTreeMap::new()),
        });
        if let Some((output, exit)) = process.native_output.take() {
            spawn_native_reader(Arc::clone(&session), output, exit);
            return (session, None, None);
        }
        (session, Some(process.stdout), process.stderr)
    }

    async fn append_output(&self, stream: &str, text: String) {
        if text.is_empty() {
            return;
        }
        // A standalone marker can span chunks. Do not mistake echoed prompts
        // mentioning the marker for a completed turn.
        let saw_sentinel = {
            let mut lines = self.sentinel_lines.lock().await;
            let line = lines.entry(stream.to_owned()).or_default();
            let mut complete = false;
            for ch in text.chars() {
                if ch == '\n' {
                    complete |= is_completion_line(line);
                    line.clear();
                } else {
                    line.push(ch);
                    if line.len() > 4096 {
                        line.clear();
                    }
                }
            }
            complete
        };
        let (entries, bytes) = {
            let mut transcript = self.transcript.lock().await;
            transcript.append(stream, text);
            (transcript.entries.len(), transcript.total_bytes)
        };
        let now = now_timestamp();
        {
            let mut last_activity = self.last_activity.lock().await;
            *last_activity = Instant::now();
        }
        {
            let mut metadata = self.metadata.write().await;
            metadata.updated_at = now.clone();
            metadata.last_activity_at = now;
            metadata.transcript_entries = entries;
            metadata.transcript_bytes = bytes;
        }
        if saw_sentinel {
            self.mark_completion(TerminalPtyCompletion::Sentinel).await;
        }
        self.notify.notify_waiters();
    }

    async fn touch(&self) {
        let now = now_timestamp();
        {
            let mut last_activity = self.last_activity.lock().await;
            *last_activity = Instant::now();
        }
        {
            let mut metadata = self.metadata.write().await;
            metadata.updated_at = now.clone();
            metadata.last_activity_at = now;
        }
    }

    async fn mark_completion(&self, completion: TerminalPtyCompletion) {
        let mut metadata = self.metadata.write().await;
        if completion == TerminalPtyCompletion::Exited
            && matches!(
                metadata.completion,
                TerminalPtyCompletion::ExplicitClose
                    | TerminalPtyCompletion::IdleTimeout
                    | TerminalPtyCompletion::GatewayShutdown
            )
        {
            return;
        }
        let ends_process = !matches!(
            completion,
            TerminalPtyCompletion::Sentinel | TerminalPtyCompletion::Manual
        );
        if !ends_process && metadata.completion != TerminalPtyCompletion::Running {
            return;
        }
        metadata.completion = completion;
        metadata.turn_pending = false;
        metadata.completed_at = Some(now_timestamp());
        if ends_process {
            metadata.reconnect = TerminalPtyReconnectHint::closed(&metadata.completion);
        }
        self.notify.notify_waiters();
    }

    pub(crate) async fn metadata(&self) -> TerminalPtySessionMetadata {
        self.metadata.read().await.clone()
    }

    pub(crate) async fn transcript(&self) -> Vec<TerminalPtyTranscriptEntry> {
        self.transcript.lock().await.entries.clone()
    }

    async fn is_idle(&self, timeout: Duration) -> bool {
        if self.turn.try_lock().is_err() || self.metadata.read().await.turn_pending {
            return false;
        }
        let last_activity = self.last_activity.lock().await;
        last_activity.elapsed() >= timeout
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalPtyCloseReason {
    ExplicitClose,
    IdleTimeout,
    GatewayShutdown,
}

impl TerminalPtyCloseReason {
    fn completion(self) -> TerminalPtyCompletion {
        match self {
            Self::ExplicitClose => TerminalPtyCompletion::ExplicitClose,
            Self::IdleTimeout => TerminalPtyCompletion::IdleTimeout,
            Self::GatewayShutdown => TerminalPtyCompletion::GatewayShutdown,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TerminalPtyManager {
    backend: Arc<dyn TerminalPtyBackend>,
    sessions: Arc<Mutex<BTreeMap<TerminalPtySessionKey, Arc<TerminalPtySession>>>>,
    idle_timeout: Duration,
    transcript_limit_bytes: usize,
}

impl TerminalPtyManager {
    pub(crate) fn process_backed(idle_timeout: Duration, transcript_limit_bytes: usize) -> Self {
        Self::new(
            Arc::new(ProcessBackedTerminalPtyBackend),
            idle_timeout,
            transcript_limit_bytes,
        )
    }

    pub(crate) fn new(
        backend: Arc<dyn TerminalPtyBackend>,
        idle_timeout: Duration,
        transcript_limit_bytes: usize,
    ) -> Self {
        Self {
            backend,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            idle_timeout,
            transcript_limit_bytes,
        }
    }

    pub(crate) async fn get_or_spawn(
        &self,
        key: TerminalPtySessionKey,
        spec: TerminalPtySpawnSpec,
    ) -> io::Result<Arc<TerminalPtySession>> {
        // Keep creation atomic, including concurrent requests for the same key.
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(&key).cloned() {
            if session.spec.program != spec.program
                || session.spec.args != spec.args
                || session.spec.cwd != spec.cwd
                || session.spec.env != spec.env
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "terminal configuration changed; close the existing session before restarting it",
                ));
            }
            return Ok(session);
        }

        let process = self.backend.spawn(spec.clone()).await?;
        let (session, stdout, stderr) = TerminalPtySession::new(
            key.clone(),
            &spec,
            process,
            self.idle_timeout,
            self.transcript_limit_bytes,
        );

        if let Some(stdout) = stdout {
            spawn_reader(Arc::clone(&session), "stdout", stdout);
        }
        if let Some(stderr) = stderr {
            spawn_reader(Arc::clone(&session), "stderr", stderr);
        }

        sessions.insert(key, Arc::clone(&session));
        Ok(session)
    }

    pub(crate) async fn write(
        &self,
        key: &TerminalPtySessionKey,
        input: TerminalPtyWrite,
    ) -> io::Result<()> {
        let session = self.session(key).await?;
        if !session
            .metadata
            .read()
            .await
            .reconnect
            .can_reconnect_in_gateway
        {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal process is closed; close the session before starting a replacement",
            ));
        }
        if input.kind == TerminalPtyWriteKind::ManualComplete {
            session.mark_completion(TerminalPtyCompletion::Manual).await;
            return Ok(());
        }

        let native = session.metadata.read().await.native_pty;
        let mut bytes = input.bytes();
        if native
            && matches!(
                input.kind,
                TerminalPtyWriteKind::Line | TerminalPtyWriteKind::Newline
            )
        {
            // A terminal Enter key is CR, unlike a pipe line delimiter.
            if let Some(last) = bytes.last_mut() {
                *last = b'\r';
            }
        }
        let mut child = session.child.lock().await;
        child.write(&bytes).await?;
        drop(child);
        session.touch().await;
        Ok(())
    }

    pub(crate) async fn resize(
        &self,
        key: &TerminalPtySessionKey,
        size: TerminalPtySize,
    ) -> io::Result<()> {
        let session = self.session(key).await?;
        let mut child = session.child.lock().await;
        child.resize(size).await?;
        drop(child);
        let mut metadata = session.metadata.write().await;
        metadata.size = size;
        metadata.updated_at = now_timestamp();
        Ok(())
    }

    pub(crate) async fn read_transcript(
        &self,
        key: &TerminalPtySessionKey,
    ) -> io::Result<Vec<TerminalPtyTranscriptEntry>> {
        Ok(self.session(key).await?.transcript().await)
    }

    pub(crate) async fn metadata(
        &self,
        key: &TerminalPtySessionKey,
    ) -> io::Result<TerminalPtySessionMetadata> {
        Ok(self.session(key).await?.metadata().await)
    }

    pub(crate) async fn list_metadata(&self) -> Vec<TerminalPtySessionMetadata> {
        let sessions = {
            let sessions = self.sessions.lock().await;
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut out = Vec::with_capacity(sessions.len());
        for session in sessions {
            out.push(session.metadata().await);
        }
        out.sort_by(|a, b| {
            a.key
                .agent_id
                .cmp(&b.key.agent_id)
                .then(a.key.session_id.cmp(&b.key.session_id))
        });
        out
    }

    pub(crate) async fn wait_for_text(
        &self,
        key: &TerminalPtySessionKey,
        needle: &str,
        timeout: Duration,
    ) -> io::Result<Option<Vec<TerminalPtyTranscriptEntry>>> {
        let session = self.session(key).await?;
        let deadline = Instant::now() + timeout;
        loop {
            let notified = session.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let entries = session.transcript().await;
            if entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<String>()
                .contains(needle)
            {
                return Ok(Some(entries));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait = deadline.saturating_duration_since(now);
            if tokio::time::timeout(wait, notified).await.is_err() {
                return Ok(None);
            }
        }
    }

    /// Submit a turn to an existing process. Completion is explicit: a marker,
    /// manual completion, or process exit. Silence is never treated as success.
    pub(crate) async fn run_turn(
        &self,
        key: &TerminalPtySessionKey,
        input: TerminalPtyWrite,
        timeout: Duration,
        on_output: impl Fn(&TerminalPtyTranscriptEntry),
    ) -> io::Result<(String, TerminalPtySessionMetadata)> {
        let session = self.session(key).await?;
        let _turn = session.turn.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "a turn is already running in this terminal session",
            )
        })?;
        let mut sequence = session.transcript.lock().await.next_sequence - 1;
        {
            let mut metadata = session.metadata.write().await;
            if !metadata.reconnect.can_reconnect_in_gateway {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal process has exited; close the session before restarting it",
                ));
            }
            if metadata.turn_pending {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "the previous terminal turn is still pending; complete or close it before submitting another prompt",
                ));
            }
            metadata.completion = TerminalPtyCompletion::Running;
            metadata.turn_pending = true;
            metadata.completed_at = None;
        }
        session.sentinel_lines.lock().await.clear();
        if let Err(error) = self.write(key, input).await {
            session.metadata.write().await.turn_pending = false;
            return Err(error);
        }
        let deadline = Instant::now() + timeout;
        let mut output = String::new();
        loop {
            let notified = session.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // Read completion before the transcript so a newly completed turn
            // cannot return before consuming its last output entry.
            let metadata = session.metadata().await;
            for entry in session
                .transcript()
                .await
                .into_iter()
                .filter(|entry| entry.sequence > sequence)
                .collect::<Vec<_>>()
            {
                sequence = entry.sequence;
                on_output(&entry);
                if output.len() + entry.text.len() > self.transcript_limit_bytes {
                    return Err(io::Error::other(
                        "managed terminal turn output exceeded its capture limit; the process remains available through pty.read",
                    ));
                }
                output.push_str(&entry.text);
            }
            if metadata.completion != TerminalPtyCompletion::Running {
                return Ok((output, metadata));
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "managed terminal turn timed out; the process is still running. Use pty.read/write to inspect or interrupt it, then manual_complete or close it",
                ));
            }
        }
    }

    pub(crate) async fn close(
        &self,
        key: &TerminalPtySessionKey,
        reason: TerminalPtyCloseReason,
    ) -> io::Result<Option<TerminalPtySessionMetadata>> {
        let session = self.sessions.lock().await.remove(key);
        let Some(session) = session else {
            return Ok(None);
        };
        session.mark_completion(reason.completion()).await;
        let mut child = session.child.lock().await;
        child.kill().await?;
        drop(child);
        Ok(Some(session.metadata().await))
    }

    pub(crate) async fn close_idle(&self) -> io::Result<usize> {
        let candidates = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .map(|(key, session)| (key.clone(), Arc::clone(session)))
                .collect::<Vec<_>>()
        };

        let mut closed = 0;
        for (key, session) in candidates {
            if session.is_idle(self.idle_timeout).await
                && self
                    .close(&key, TerminalPtyCloseReason::IdleTimeout)
                    .await?
                    .is_some()
            {
                closed += 1;
            }
        }
        Ok(closed)
    }

    pub(crate) async fn close_all(&self, reason: TerminalPtyCloseReason) -> io::Result<usize> {
        let keys = {
            let sessions = self.sessions.lock().await;
            sessions.keys().cloned().collect::<Vec<_>>()
        };
        let mut closed = 0;
        for key in keys {
            if self.close(&key, reason).await?.is_some() {
                closed += 1;
            }
        }
        Ok(closed)
    }

    pub(crate) async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    async fn session(&self, key: &TerminalPtySessionKey) -> io::Result<Arc<TerminalPtySession>> {
        self.sessions.lock().await.get(key).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "managed terminal session not found: {}/{}",
                    key.agent_id, key.session_id
                ),
            )
        })
    }
}

pub(crate) fn terminal_pty_manager() -> &'static TerminalPtyManager {
    static MANAGER: OnceLock<TerminalPtyManager> = OnceLock::new();
    MANAGER.get_or_init(|| {
        TerminalPtyManager::new(
            Arc::new(NativeTerminalPtyBackend),
            Duration::from_secs(30 * 60),
            2 * 1024 * 1024,
        )
    })
}

fn spawn_reader(
    session: Arc<TerminalPtySession>,
    stream: &'static str,
    mut reader: BoxedPtyReader,
) {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 4096];
        let mut decoder = Utf8Decoder::default();
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    let text = decoder.push(&buffer[..n]);
                    session.append_output(stream, text).await;
                }
                Err(err) => {
                    // Attribute the error to the stream it actually came from
                    // (was hardcoded "stderr", which mislabeled stdout errors).
                    session
                        .append_output(
                            stream,
                            format!("managed terminal {stream} read failed: {err}"),
                        )
                        .await;
                    break;
                }
            }
        }
        let tail = decoder.finish();
        if !tail.is_empty() {
            session.append_output(stream, tail).await;
        }
    });
}

/// Keep incomplete UTF-8 between reads; a ConPTY read can split a CJK character.
#[derive(Default)]
struct Utf8Decoder(Vec<u8>);

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.0.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.0) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.0.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.0[..valid]));
                    self.0.drain(..valid);
                    if let Some(length) = error.error_len() {
                        text.push('\u{fffd}');
                        self.0.drain(..length);
                    } else {
                        break;
                    }
                }
            }
        }
        text
    }

    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.0)).into_owned()
    }
}

fn spawn_native_reader(
    session: Arc<TerminalPtySession>,
    mut output: broadcast::Receiver<Vec<u8>>,
    mut exit: oneshot::Receiver<i32>,
) {
    tokio::spawn(async move {
        let mut decoder = Utf8Decoder::default();
        let code = loop {
            tokio::select! {
                result = output.recv() => {
                    match result {
                        Ok(bytes) => session.append_output("stdout", decoder.push(&bytes)).await,
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            session.append_output("system", format!("[terminal output lost: {count} chunks]\n")).await;
                        }
                        Err(broadcast::error::RecvError::Closed) => break exit.await.unwrap_or(-1),
                    }
                }
                result = &mut exit => break result.unwrap_or(-1),
            }
        };
        // ConPTY may deliver its last bytes after the process exit notification.
        // Bound draining even if descendants still hold the terminal open.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let wait =
                Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now()));
            if wait.is_zero() {
                break;
            }
            match tokio::time::timeout(wait, output.recv()).await {
                Ok(Ok(bytes)) => session.append_output("stdout", decoder.push(&bytes)).await,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                _ => break,
            }
        }
        let tail = decoder.finish();
        if !tail.is_empty() {
            session.append_output("stdout", tail).await;
        }
        session.metadata.write().await.exit_code = Some(code);
        session.mark_completion(TerminalPtyCompletion::Exited).await;
    });
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn is_completion_line(line: &str) -> bool {
    static ANSI: OnceLock<regex::Regex> = OnceLock::new();
    let ansi = ANSI.get_or_init(|| {
        regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
            .expect("valid ANSI pattern")
    });
    ansi.replace_all(line, "").trim() == SENTINEL_COMPLETE
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        TerminalPtyCloseReason, TerminalPtyCompletion, TerminalPtyManager, TerminalPtySessionKey,
        TerminalPtySize, TerminalPtySpawnSpec, TerminalPtyWrite,
    };

    fn fake_repl_spec(cwd: std::path::PathBuf) -> TerminalPtySpawnSpec {
        #[cfg(windows)]
        {
            TerminalPtySpawnSpec {
                program: "powershell".to_owned(),
                args: vec![
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    r#"
[Console]::Out.WriteLine('ready')
[Console]::Out.Flush()
while (($line = [Console]::In.ReadLine()) -ne $null) {
  if ($line -eq 'complete') {
    [Console]::Out.WriteLine('::savfox-complete')
    [Console]::Out.Flush()
    break
  } elseif ($line -eq 'stderr') {
    [Console]::Error.WriteLine('err:stderr')
    [Console]::Error.Flush()
  } else {
    [Console]::Out.WriteLine("reply:$line")
    [Console]::Out.Flush()
  }
}
"#
                    .to_owned(),
                ],
                cwd,
                env: BTreeMap::new(),
                size: TerminalPtySize::default(),
            }
        }
        #[cfg(not(windows))]
        {
            TerminalPtySpawnSpec {
                program: "sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "printf 'ready\n'; while IFS= read -r line; do case \"$line\" in complete) printf '::savfox-complete\n'; break;; stderr) printf 'err:stderr\n' >&2;; *) printf 'reply:%s\n' \"$line\";; esac; done".to_owned(),
                ],
                cwd,
                env: BTreeMap::new(),
                size: TerminalPtySize::default(),
            }
        }
    }

    fn manager(timeout: Duration) -> TerminalPtyManager {
        TerminalPtyManager::process_backed(timeout, 64 * 1024)
    }

    async fn spawn_fake(
        manager: &TerminalPtyManager,
        key: &TerminalPtySessionKey,
        cwd: std::path::PathBuf,
    ) {
        manager
            .get_or_spawn(key.clone(), fake_repl_spec(cwd))
            .await
            .expect("spawn fake repl");
        manager
            .wait_for_text(key, "ready", Duration::from_secs(5))
            .await
            .expect("wait for ready")
            .expect("ready output");
    }

    #[tokio::test]
    async fn pty_registry_reuses_session_for_same_agent_and_session() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_secs(30));
        let key = TerminalPtySessionKey::new("agent-a", "session-a");

        let first = manager
            .get_or_spawn(key.clone(), fake_repl_spec(temp.path().to_path_buf()))
            .await
            .expect("spawn first");
        let second = manager
            .get_or_spawn(key.clone(), fake_repl_spec(temp.path().to_path_buf()))
            .await
            .expect("reuse second");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.session_count().await, 1);

        manager
            .close(&key, TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("close session");
    }

    #[tokio::test]
    async fn fake_repl_round_trips_multiple_turns_and_transcript() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_secs(30));
        let key = TerminalPtySessionKey::new("agent-a", "session-b");
        spawn_fake(&manager, &key, temp.path().to_path_buf()).await;

        manager
            .write(&key, TerminalPtyWrite::line("one"))
            .await
            .expect("write first prompt");
        manager
            .wait_for_text(&key, "reply:one", Duration::from_secs(5))
            .await
            .expect("wait for first reply")
            .expect("first reply");

        manager
            .write(&key, TerminalPtyWrite::line("two"))
            .await
            .expect("write second prompt");
        let transcript = manager
            .wait_for_text(&key, "reply:two", Duration::from_secs(5))
            .await
            .expect("wait for second reply")
            .expect("second reply");
        let joined = transcript
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<String>();

        assert!(joined.contains("ready"));
        assert!(joined.contains("reply:one"));
        assert!(joined.contains("reply:two"));

        manager
            .close(&key, TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("close session");
    }

    #[tokio::test]
    async fn fake_repl_captures_stderr_and_sentinel_completion() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_secs(30));
        let key = TerminalPtySessionKey::new("agent-a", "session-c");
        spawn_fake(&manager, &key, temp.path().to_path_buf()).await;

        manager
            .write(&key, TerminalPtyWrite::line("stderr"))
            .await
            .expect("write stderr prompt");
        manager
            .wait_for_text(&key, "err:stderr", Duration::from_secs(5))
            .await
            .expect("wait stderr")
            .expect("stderr output");

        manager
            .write(&key, TerminalPtyWrite::line("complete"))
            .await
            .expect("write complete");
        manager
            .wait_for_text(&key, "::savfox-complete", Duration::from_secs(5))
            .await
            .expect("wait completion")
            .expect("completion marker");
        let metadata = manager.metadata(&key).await.expect("metadata");

        assert_eq!(metadata.completion, TerminalPtyCompletion::Sentinel);
        assert!(metadata.reconnect.can_reconnect_in_gateway);

        manager
            .close(&key, TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("close session");
    }

    #[tokio::test]
    async fn manual_complete_and_resize_update_metadata() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_secs(30));
        let key = TerminalPtySessionKey::new("agent-a", "session-d");
        spawn_fake(&manager, &key, temp.path().to_path_buf()).await;

        let new_size = TerminalPtySize { cols: 88, rows: 24 };
        manager.resize(&key, new_size).await.expect("resize");
        manager
            .write(&key, TerminalPtyWrite::manual_complete())
            .await
            .expect("manual complete");
        let metadata = manager.metadata(&key).await.expect("metadata");

        assert_eq!(metadata.size, new_size);
        assert_eq!(metadata.completion, TerminalPtyCompletion::Manual);
        assert_eq!(
            metadata.reconnect.state,
            super::TerminalPtyReconnectState::Attached
        );

        manager
            .close(&key, TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("close session");
    }

    #[tokio::test]
    async fn idle_timeout_closes_sessions_without_leaving_registry_entries() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_millis(20));
        let key = TerminalPtySessionKey::new("agent-a", "session-e");
        spawn_fake(&manager, &key, temp.path().to_path_buf()).await;

        tokio::time::sleep(Duration::from_millis(60)).await;
        let closed = manager.close_idle().await.expect("close idle");

        assert_eq!(closed, 1);
        assert_eq!(manager.session_count().await, 0);
        assert!(manager.read_transcript(&key).await.is_err());
    }

    #[tokio::test]
    async fn close_all_cleans_gateway_shutdown_sessions() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let manager = manager(Duration::from_secs(30));
        let key_a = TerminalPtySessionKey::new("agent-a", "session-f");
        let key_b = TerminalPtySessionKey::new("agent-a", "session-g");
        spawn_fake(&manager, &key_a, temp.path().to_path_buf()).await;
        spawn_fake(&manager, &key_b, temp.path().to_path_buf()).await;

        let closed = manager
            .close_all(TerminalPtyCloseReason::GatewayShutdown)
            .await
            .expect("close all");

        assert_eq!(closed, 2);
        assert_eq!(manager.session_count().await, 0);
    }

    fn native_repl_spec(cwd: std::path::PathBuf) -> TerminalPtySpawnSpec {
        let (program, args) = if cfg!(windows) {
            (
                "powershell.exe",
                vec![
                    "-NoProfile",
                    "-Command",
                    r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::WriteLine("tty:$(-not [Console]::IsInputRedirected)")
while (($line = [Console]::ReadLine()) -ne $null) {
  if ($line -eq 'quit') { exit 7 }
  if ($line -eq 'size') { [Console]::WriteLine("size:$([Console]::WindowWidth)x$([Console]::WindowHeight)") }
  else { [Console]::WriteLine("reply:$line") }
  [Console]::WriteLine('::savfox-complete')
}
"#,
                ],
            )
        } else {
            (
                "sh",
                vec![
                    "-c",
                    "test -t 0 && printf 'tty:True\n'; while IFS= read -r line; do case \"$line\" in quit) exit 7;; size) stty size;; *) printf 'reply:%s\n' \"$line\";; esac; printf '::savfox-complete\n'; done",
                ],
            )
        };
        TerminalPtySpawnSpec {
            program: program.to_owned(),
            args: args.into_iter().map(str::to_owned).collect(),
            cwd,
            env: BTreeMap::new(),
            size: TerminalPtySize::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_pty_roundtrip_resize_reuse_and_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manager = TerminalPtyManager::new(
            Arc::new(super::NativeTerminalPtyBackend),
            Duration::from_secs(60),
            64 * 1024,
        );
        let key = TerminalPtySessionKey::new("native", "roundtrip");
        let spec = native_repl_spec(temp.path().to_path_buf());
        let (first, second) = tokio::join!(
            manager.get_or_spawn(key.clone(), spec.clone()),
            manager.get_or_spawn(key.clone(), spec)
        );
        let first = first.expect("native spawn");
        assert!(Arc::ptr_eq(&first, &second.expect("concurrent reuse")));
        let initial = first.metadata().await;
        assert!(initial.native_pty);
        assert!(initial.pid.is_some());
        manager
            .wait_for_text(&key, "tty:True", Duration::from_secs(10))
            .await
            .expect("read")
            .expect("real terminal stdin");
        for prompt in ["hello", "中文输入"] {
            let (output, metadata) = manager
                .run_turn(
                    &key,
                    TerminalPtyWrite::line(prompt),
                    Duration::from_secs(10),
                    |_| {},
                )
                .await
                .expect("managed turn");
            assert!(output.contains(&format!("reply:{prompt}")), "{output:?}");
            assert_eq!(metadata.pid, initial.pid);
            assert_eq!(metadata.completion, TerminalPtyCompletion::Sentinel);
            assert!(metadata.reconnect.can_reconnect_in_gateway);
        }
        manager
            .resize(&key, TerminalPtySize { cols: 97, rows: 33 })
            .await
            .expect("real resize");
        let (output, _) = manager
            .run_turn(
                &key,
                TerminalPtyWrite::line("size"),
                Duration::from_secs(10),
                |_| {},
            )
            .await
            .expect("size query");
        assert!(
            output.contains(if cfg!(windows) { "size:97x33" } else { "33 97" }),
            "{output:?}"
        );
        let (_, metadata) = manager
            .run_turn(
                &key,
                TerminalPtyWrite::line("quit"),
                Duration::from_secs(10),
                |_| {},
            )
            .await
            .expect("exit");
        assert_eq!(metadata.exit_code, Some(7));
        assert!(!metadata.reconnect.can_reconnect_in_gateway);
        assert!(
            manager
                .write(&key, TerminalPtyWrite::line("after exit"))
                .await
                .is_err()
        );
        manager
            .close_all(TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("cleanup");
    }

    #[test]
    fn utf8_decoder_preserves_split_cjk_and_handles_invalid_bytes() {
        let mut decoder = super::Utf8Decoder::default();
        let bytes = "中文".as_bytes();
        assert_eq!(decoder.push(&bytes[..2]), "");
        assert_eq!(decoder.push(&bytes[2..4]), "中");
        assert_eq!(decoder.push(&bytes[4..]), "文");
        assert_eq!(decoder.push(&[0xff]), "\u{fffd}");
        assert!(decoder.finish().is_empty());
    }

    #[tokio::test]
    async fn pending_turn_survives_timeout_until_manual_completion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manager = manager(Duration::ZERO);
        let key = TerminalPtySessionKey::new("pending", "timeout");
        spawn_fake(&manager, &key, temp.path().to_path_buf()).await;
        let pid = manager.metadata(&key).await.expect("metadata").pid;
        let error = manager
            .run_turn(
                &key,
                TerminalPtyWrite::line("working"),
                Duration::from_millis(200),
                |_| {},
            )
            .await
            .expect_err("no completion marker");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(manager.metadata(&key).await.expect("metadata").turn_pending);
        assert_eq!(manager.close_idle().await.expect("idle scan"), 0);
        let error = manager
            .run_turn(
                &key,
                TerminalPtyWrite::line("overlap"),
                Duration::from_secs(1),
                |_| {},
            )
            .await
            .expect_err("pending turn rejects another prompt");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        manager
            .write(
                &key,
                TerminalPtyWrite {
                    kind: super::TerminalPtyWriteKind::ManualComplete,
                    text: String::new(),
                },
            )
            .await
            .expect("manual completion");
        assert!(!manager.metadata(&key).await.expect("metadata").turn_pending);
        let (_, metadata) = manager
            .run_turn(
                &key,
                TerminalPtyWrite::line("complete"),
                Duration::from_secs(5),
                |_| {},
            )
            .await
            .expect("next turn after completion");
        assert_eq!(metadata.pid, pid);
        assert_eq!(metadata.completion, TerminalPtyCompletion::Sentinel);
        manager
            .close(&key, TerminalPtyCloseReason::ExplicitClose)
            .await
            .expect("cleanup");
    }
}
