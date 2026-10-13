use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use savfox_protocol::protocol::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, oneshot, watch};

use crate::auth::TokenScope;
use crate::exec_approval::{
    ExecApprovalRequest, ExecApprovalResolution, ResolveOutcome, generate_approval_nonce,
    persist_pending_approval, persist_resolved_approval,
};
use crate::session::GatewaySessionManager;
use crate::terminal_agent::TerminalSupervisorResult;

#[derive(Clone, Debug)]
pub(crate) struct Execution {
    pub(crate) owner: String,
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_id: String,
    pub(crate) approval_client: bool,
    pub(crate) trusted_context: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RunOutput {
    pub(crate) supervised: TerminalSupervisorResult,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) vendor_usage: Option<Value>,
    pub(crate) run_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    run_id: String,
    request_id: String,
    session_id: String,
    agent_id: String,
    input_digest: String,
    status: String,
    reply: Option<String>,
    error: Option<String>,
    usage: Option<Value>,
    stderr: Option<String>,
    exit_reason: Option<String>,
}

struct Entry {
    execution: Execution,
    snapshot: Snapshot,
    cancel: watch::Sender<bool>,
    done: watch::Sender<Option<RunOutput>>,
    created: Instant,
}

struct Pending {
    owner: String,
    run_id: String,
    nonce: String,
    question: bool,
    question_input: Option<Value>,
    cancel: watch::Receiver<bool>,
    expires_at: Instant,
    sender: oneshot::Sender<Value>,
}

#[derive(Clone)]
pub(crate) struct Runtime {
    home: PathBuf,
    sessions: GatewaySessionManager,
    runs: Arc<Mutex<HashMap<String, Entry>>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    closing: watch::Sender<bool>,
}

impl Runtime {
    pub(crate) fn new(home: PathBuf, sessions: GatewaySessionManager) -> Self {
        Self {
            home,
            sessions,
            runs: Arc::default(),
            pending: Arc::default(),
            closing: watch::channel(false).0,
        }
    }

    fn key(execution: &Execution) -> String {
        super::digest(format!("{}\0{}", execution.owner, execution.request_id).as_bytes())
    }

    fn record_path(&self, key: &str) -> PathBuf {
        self.home
            .join("terminal-agents/claude-runs")
            .join(format!("{key}.json"))
    }

    fn owner_path(&self, session_id: &str) -> PathBuf {
        let session_id = uuid::Uuid::parse_str(session_id)
            .map_or_else(|_| session_id.to_owned(), |id| id.to_string());
        self.home
            .join("terminal-agents/claude-session-owners")
            .join(format!("{}.json", super::digest(session_id.as_bytes())))
    }

    pub(crate) async fn authorize_session(
        &self,
        owner: Option<&str>,
        session_id: &str,
    ) -> anyhow::Result<()> {
        match tokio::fs::read(self.owner_path(session_id)).await {
            Ok(bytes) => {
                let saved: String = serde_json::from_slice(&bytes)?;
                ensure!(
                    owner == Some(saved.as_str()),
                    "Claude chat belongs to another credential"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(crate) async fn begin(
        &self,
        execution: Execution,
        input_digest: String,
    ) -> anyhow::Result<(
        String,
        watch::Receiver<bool>,
        Option<watch::Receiver<Option<RunOutput>>>,
    )> {
        ensure!(!*self.closing.borrow(), "gateway is shutting down");
        let key = Self::key(&execution);
        let mut runs = self.runs.lock().await;
        // Shutdown can begin while this caller waits for the registry lock.
        ensure!(!*self.closing.borrow(), "gateway is shutting down");
        self.authorize_session(Some(&execution.owner), &execution.session_id)
            .await?;
        super::runner::atomic_json(&self.owner_path(&execution.session_id), &execution.owner)
            .await?;
        if let Some(existing) = runs.get(&key) {
            ensure!(
                existing.snapshot.input_digest == input_digest
                    && existing.execution.session_id == execution.session_id
                    && existing.execution.agent_id == execution.agent_id,
                "request_id was already used with different input or target"
            );
            return Ok((
                existing.snapshot.run_id.clone(),
                existing.cancel.subscribe(),
                Some(existing.done.subscribe()),
            ));
        }
        if tokio::fs::try_exists(self.record_path(&key)).await? {
            bail!(
                "request_id already has a durable execution record; query chat.run.get, do not replay an uncertain operation"
            );
        }
        runs.retain(|_, entry| {
            entry.done.borrow().is_none() || entry.created.elapsed() < Duration::from_secs(600)
        });
        if runs.len() >= 256
            && let Some(key) = runs
                .iter()
                .filter(|(_, entry)| entry.done.borrow().is_some())
                .min_by_key(|(_, entry)| entry.created)
                .map(|(key, _)| key.clone())
        {
            runs.remove(&key);
        }
        ensure!(
            runs.len() < 256
                && runs
                    .values()
                    .filter(|entry| entry.done.borrow().is_none())
                    .count()
                    < 64,
            "too many Claude executions"
        );
        let run_id = uuid::Uuid::now_v7().to_string();
        let snapshot = Snapshot {
            run_id: run_id.clone(),
            request_id: execution.request_id.clone(),
            session_id: execution.session_id.clone(),
            agent_id: execution.agent_id.clone(),
            input_digest,
            status: "starting".to_owned(),
            reply: None,
            error: None,
            usage: None,
            stderr: None,
            exit_reason: None,
        };
        super::runner::atomic_json(&self.record_path(&key), &snapshot).await?;
        let (cancel, rx) = watch::channel(false);
        runs.insert(
            key,
            Entry {
                execution,
                snapshot,
                cancel,
                done: watch::channel(None).0,
                created: Instant::now(),
            },
        );
        Ok((run_id, rx, None))
    }

    pub(crate) async fn finish(&self, execution: &Execution, output: RunOutput, status: &str) {
        let key = Self::key(execution);
        let mut runs = self.runs.lock().await;
        if let Some(entry) = runs.get_mut(&key) {
            if entry.done.borrow().is_some() {
                return;
            }
            entry.snapshot.status = if *entry.cancel.borrow() {
                "cancelled".to_owned()
            } else {
                status.to_owned()
            };
            entry.snapshot.reply = Some(output.supervised.stdout.clone());
            entry.snapshot.error = output.supervised.error.clone();
            entry.snapshot.usage = output.vendor_usage.clone();
            entry.snapshot.stderr = Some(output.supervised.stderr.clone());
            entry.snapshot.exit_reason = Some(output.supervised.exit_reason.as_str().to_owned());
            if let Err(error) =
                super::runner::atomic_json(&self.record_path(&key), &entry.snapshot).await
            {
                tracing::warn!(%error, "failed to persist Claude result; startup record remains unknown");
            }
            entry.done.send_replace(Some(output));
        }
    }

    pub(crate) async fn finish_setup_error(
        &self,
        execution: &Execution,
        run_id: &str,
        error: String,
    ) {
        self.finish(
            execution,
            RunOutput {
                supervised: TerminalSupervisorResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    pid: None,
                    exit_code: None,
                    exit_reason: crate::terminal_agent::TerminalExitReason::IoError,
                    error: Some(error),
                },
                usage: None,
                vendor_usage: None,
                run_id: run_id.to_owned(),
            },
            "error",
        )
        .await;
    }

    pub(crate) async fn running(&self, execution: &Execution) -> anyhow::Result<()> {
        let key = Self::key(execution);
        let mut runs = self.runs.lock().await;
        let entry = runs
            .get_mut(&key)
            .context("Claude execution is not registered")?;
        ensure!(
            !*entry.cancel.borrow(),
            "Claude execution stopped before prompt"
        );
        entry.snapshot.status = "running".to_owned();
        super::runner::atomic_json(&self.record_path(&key), &entry.snapshot).await
    }

    pub(crate) async fn query(&self, owner: &str, request_id: &str) -> anyhow::Result<Value> {
        let key = super::digest(format!("{owner}\0{request_id}").as_bytes());
        if let Some(entry) = self.runs.lock().await.get(&key) {
            let mut value = serde_json::to_value(&entry.snapshot)?;
            value["pending_approvals"] = self.pending_for(owner, &entry.snapshot.run_id).await;
            return Ok(value);
        }
        let bytes = tokio::fs::read(self.record_path(&key))
            .await
            .context("Claude execution not found")?;
        let mut snapshot: Snapshot = serde_json::from_slice(&bytes)?;
        if snapshot.status == "starting" || snapshot.status == "running" {
            snapshot.status = "unknown_after_restart".to_owned();
        }
        Ok(serde_json::to_value(snapshot)?)
    }

    async fn pending_for(&self, owner: &str, run_id: &str) -> Value {
        // Nonces are queryable only by the credential owner of this execution.
        Value::Array(self.pending.lock().await.iter().filter(|(_, pending)| pending.owner == owner && pending.run_id == run_id).map(|(id, pending)| json!({"request_id": id, "nonce": pending.nonce, "question": pending.question})).collect())
    }

    pub(crate) async fn cancel(&self, owner: &str, target: &str) -> bool {
        let runs = self.runs.lock().await;
        let target =
            uuid::Uuid::parse_str(target).map_or_else(|_| target.to_owned(), |id| id.to_string());
        let mut found = false;
        for entry in runs.values() {
            if entry.execution.owner == owner
                && (entry.snapshot.run_id == target || entry.execution.session_id == target)
            {
                found = true;
                if entry.done.borrow().is_none() {
                    entry.cancel.send_replace(true);
                }
            }
        }
        found
    }

    pub(crate) async fn pending_ids(&self, owner: &str) -> std::collections::HashSet<String> {
        self.pending
            .lock()
            .await
            .iter()
            .filter(|(_, pending)| pending.owner == owner && !pending.sender.is_closed())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) async fn shutdown(&self) {
        self.closing.send_replace(true);
        let receivers: Vec<_> = self
            .runs
            .lock()
            .await
            .values()
            .filter(|entry| entry.done.borrow().is_none())
            .map(|entry| {
                entry.cancel.send_replace(true);
                entry.done.subscribe()
            })
            .collect();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            for mut receiver in receivers {
                while receiver.borrow().is_none() && receiver.changed().await.is_ok() {}
            }
        })
        .await;
    }

    pub(crate) async fn can_approve(&self, execution: &Execution) -> bool {
        execution.approval_client
            && self
                .sessions
                .has_owner_client(&execution.owner, TokenScope::OperatorApprovals)
                .await
    }

    pub(crate) async fn approval(
        &self,
        execution: &Execution,
        run_id: &str,
        control_id: &str,
        tool: &str,
        input: &Value,
        cwd: &std::path::Path,
        ttl: Duration,
    ) -> anyhow::Result<(String, oneshot::Receiver<Value>)> {
        ensure!(
            self.can_approve(execution).await,
            "no authorized Claude approval client"
        );
        let request_id = uuid::Uuid::now_v7().to_string();
        let nonce = generate_approval_nonce();
        let question = tool == "AskUserQuestion";
        let cancel = self
            .runs
            .lock()
            .await
            .values()
            .find(|entry| {
                entry.snapshot.run_id == run_id && entry.execution.owner == execution.owner
            })
            .map(|entry| entry.cancel.subscribe())
            .context("Claude approval target is not active")?;
        ensure!(!*cancel.borrow(), "Claude execution already stopped");
        let questions = if question {
            Some(super::protocol::questions(input)?)
        } else {
            None
        };
        let summary =
            crate::exec_approval::sanitized_approval_text(&format!("{tool}: {input}"), 2048);
        let created_at_ms = chrono::Utc::now().timestamp_millis().unsigned_abs();
        let request = ExecApprovalRequest {
            id: request_id.clone(),
            command: summary.clone(),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            agent_id: Some(execution.agent_id.clone()),
            session_id: Some(execution.session_id.clone()),
            nonce: nonce.clone(),
            created_at_ms,
            expires_at_ms: created_at_ms.saturating_add(ttl.as_millis() as u64),
            questions: questions.clone(),
            kind: Some("claude".to_owned()),
            peer_id: Some(super::digest(execution.owner.as_bytes())),
            turn_id: Some(run_id.to_owned()),
            available_decisions: vec![
                "approve-once".to_owned(),
                "deny".to_owned(),
                "abort".to_owned(),
            ],
            ..Default::default()
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            ensure!(
                pending
                    .values()
                    .filter(|pending| pending.run_id == run_id)
                    .count()
                    < 16,
                "too many pending Claude approvals"
            );
            persist_pending_approval(&self.home, &request)
                .await
                .map_err(anyhow::Error::msg)?;
            pending.insert(
                request_id.clone(),
                Pending {
                    owner: execution.owner.clone(),
                    run_id: run_id.to_owned(),
                    nonce: nonce.clone(),
                    question,
                    question_input: question.then(|| input.clone()),
                    cancel,
                    expires_at: Instant::now() + ttl,
                    sender,
                },
            );
        }
        let mut payload = json!({"request_id": request_id, "nonce": nonce, "run_id": run_id, "control_request_id": control_id, "session_id": execution.session_id, "agent_id": execution.agent_id, "tool": tool, "summary": summary, "available_decisions": request.available_decisions, "expires_at_ms": request.expires_at_ms, "execution_target": "claude"});
        if let Some(questions) = questions {
            payload["questions"] = questions;
        }
        let event = if question {
            "claude.question"
        } else {
            "exec.approval.requested"
        };
        if !self
            .sessions
            .send_to_owner(
                &execution.owner,
                TokenScope::OperatorApprovals,
                event,
                payload,
            )
            .await
        {
            self.remove_approval(&request_id, "approval delivery failed")
                .await;
            bail!("could not deliver Claude approval to an authorized client");
        }
        Ok((request_id, receiver))
    }

    pub(crate) async fn remove_approval(&self, request_id: &str, reason: &str) {
        if let Some(pending) = self.pending.lock().await.remove(request_id) {
            let resolution = ExecApprovalResolution {
                id: request_id.to_owned(),
                nonce: pending.nonce,
                approved: false,
                resolved_by: Some("gateway".to_owned()),
                reason: Some(reason.to_owned()),
            };
            let _ = persist_resolved_approval(&self.home, &resolution).await;
        }
    }

    pub(crate) async fn resolve(
        &self,
        owner: &str,
        params: &Value,
    ) -> anyhow::Result<Option<Value>> {
        let id = params["request_id"]
            .as_str()
            .context("missing request_id")?;
        let nonce = params["nonce"].as_str().context("missing nonce")?;
        let mut pending = self.pending.lock().await;
        let Some(target) = pending.get(id) else {
            return Ok(None);
        };
        ensure!(
            target.owner == owner,
            "Claude approval belongs to another credential"
        );
        ensure!(
            bool::from(target.nonce.as_bytes().ct_eq(nonce.as_bytes())),
            "approval nonce mismatch"
        );
        ensure!(
            !target.sender.is_closed(),
            "Claude approval is no longer active"
        );
        ensure!(!*target.cancel.borrow(), "Claude execution already stopped");
        ensure!(
            Instant::now() < target.expires_at,
            "Claude approval expired"
        );
        let decision = params["decision"]
            .as_str()
            .unwrap_or(if params["approved"] == true {
                "approve-once"
            } else {
                "deny"
            });
        ensure!(
            matches!(decision, "approve-once" | "deny" | "abort"),
            "Claude supports only approve-once, deny and abort"
        );
        if target.question && decision == "approve-once" {
            super::protocol::approved_input(
                target
                    .question_input
                    .as_ref()
                    .context("missing original questions")?,
                &params["answers"],
            )?;
        }
        let target = pending.remove(id).context("approval disappeared")?;
        // Persist consumption before returning a control decision. Failed audit
        // closes the waiter, so it denies rather than granting without a record.
        let consumed = persist_resolved_approval(
            &self.home,
            &ExecApprovalResolution {
                id: id.to_owned(),
                nonce: target.nonce,
                approved: decision == "approve-once",
                resolved_by: Some("credential-owner".to_owned()),
                reason: Some("decision recorded; CLI delivery not yet acknowledged".to_owned()),
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
        ensure!(
            matches!(consumed, ResolveOutcome::Resolved),
            "Claude approval is no longer pending in the audit store"
        );
        let value = json!({"decision": decision, "answers": params.get("answers")});
        target
            .sender
            .send(value)
            .map_err(|_| anyhow::anyhow!("Claude approval expired before delivery"))?;
        Ok(Some(
            json!({"request_id": id, "status": "decision_recorded", "execution_target": "claude"}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_rejects_admission_already_waiting_for_registry_lock() {
        let home = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(home.path().to_owned(), GatewaySessionManager::new());
        let guard = runtime.runs.lock().await;
        let execution = Execution {
            owner: "owner".to_owned(),
            request_id: "request".to_owned(),
            session_id: uuid::Uuid::now_v7().to_string(),
            agent_id: "claude".to_owned(),
            approval_client: false,
            trusted_context: None,
        };
        let begin = runtime.begin(execution, "input".to_owned());
        tokio::pin!(begin);
        // Poll begin through the initial closing check and into the held lock.
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut begin)
                .await
                .is_err()
        );
        runtime.closing.send_replace(true);
        drop(guard);
        assert!(begin.await.is_err());
        assert!(runtime.runs.lock().await.is_empty());
    }
}
