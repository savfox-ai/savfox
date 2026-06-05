use std::collections::BTreeMap;
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
use tokio::sync::{Mutex, Notify, RwLock};
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
    pub(crate) reconnect: TerminalPtyReconnectHint,
}

pub(crate) struct TerminalPtySession {
    child: Mutex<Box<dyn TerminalPtyChild>>,
    transcript: Mutex<TerminalPtyTranscript>,
    metadata: RwLock<TerminalPtySessionMetadata>,
    last_activity: Mutex<Instant>,
    notify: Notify,
}

impl TerminalPtySession {
    fn new(
        key: TerminalPtySessionKey,
        spec: &TerminalPtySpawnSpec,
        process: TerminalPtyProcess,
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
            reconnect: TerminalPtyReconnectHint::attached(&process.backend, process.native_pty),
        };
        let session = Arc::new(Self {
            child: Mutex::new(process.child),
            transcript: Mutex::new(TerminalPtyTranscript::new(transcript_limit_bytes)),
            metadata: RwLock::new(metadata),
            last_activity: Mutex::new(Instant::now()),
            notify: Notify::new(),
        });
        (session, Some(process.stdout), process.stderr)
    }

    async fn append_output(&self, stream: &str, text: String) {
        let saw_sentinel = text.contains(SENTINEL_COMPLETE);
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
        if metadata.completion != TerminalPtyCompletion::Running {
            return;
        }
        metadata.completion = completion;
        metadata.completed_at = Some(now_timestamp());
        metadata.reconnect = TerminalPtyReconnectHint::closed(&metadata.completion);
        self.notify.notify_waiters();
    }

    pub(crate) async fn metadata(&self) -> TerminalPtySessionMetadata {
        self.metadata.read().await.clone()
    }

    pub(crate) async fn transcript(&self) -> Vec<TerminalPtyTranscriptEntry> {
        self.transcript.lock().await.entries.clone()
    }

    async fn is_idle(&self, timeout: Duration) -> bool {
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
        if let Some(session) = self.sessions.lock().await.get(&key).cloned() {
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

        self.sessions.lock().await.insert(key, Arc::clone(&session));
        Ok(session)
    }

    pub(crate) async fn write(
        &self,
        key: &TerminalPtySessionKey,
        input: TerminalPtyWrite,
    ) -> io::Result<()> {
        let session = self.session(key).await?;
        if input.kind == TerminalPtyWriteKind::ManualComplete {
            session.mark_completion(TerminalPtyCompletion::Manual).await;
            return Ok(());
        }

        let bytes = input.bytes();
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
            let entries = session.transcript().await;
            if entries.iter().any(|entry| entry.text.contains(needle)) {
                return Ok(Some(entries));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait = deadline.saturating_duration_since(now);
            if tokio::time::timeout(wait, session.notify.notified())
                .await
                .is_err()
            {
                return Ok(None);
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
        TerminalPtyManager::process_backed(Duration::from_secs(30 * 60), 2 * 1024 * 1024)
    })
}

fn spawn_reader(
    session: Arc<TerminalPtySession>,
    stream: &'static str,
    mut reader: BoxedPtyReader,
) {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buffer[..n]).to_string();
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
    });
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
        assert!(!metadata.reconnect.can_reconnect_in_gateway);

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
            super::TerminalPtyReconnectState::Closed
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
}
