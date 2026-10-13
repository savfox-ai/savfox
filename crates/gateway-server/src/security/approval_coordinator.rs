//! Unified approval coordination for Gateway entry points and text Channels.
//!
//! Channel conversations have a logical session id, while concurrent turns may
//! execute in ephemeral forked Core sessions. This registry maps an opaque
//! external request id to the exact Core session and approval key, preventing a
//! reply from being delivered to whichever logical session happens to be
//! active when it arrives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use savfox_protocol::SessionId;
use savfox_protocol::approvals::ExecPolicyAmendment;
use savfox_protocol::protocol::{Op, ReviewDecision};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use crate::channel::GatewayChannel;
use crate::exec_approval::{
    ExecApprovalRequest, ExecApprovalResolution, ResolveOutcome, generate_approval_nonce,
    persist_pending_approval, persist_resolved_approval,
};
use crate::security::execution_policy::ApprovalClientCapabilities;

const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApprovalKind {
    Exec,
    Patch,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApprovalDecisionKind {
    ApproveOnce,
    ApproveSession,
    AllowRule,
    Deny,
    Abort,
}

/// Transport-neutral, durable description of one approval request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ApprovalRequestEnvelope {
    pub(crate) id: String,
    pub(crate) nonce: String,
    pub(crate) kind: ApprovalKind,
    pub(crate) agent_id: String,
    pub(crate) channel_instance_id: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) peer_id: Option<String>,
    pub(crate) logical_session_id: String,
    pub(crate) core_session_id: String,
    pub(crate) turn_id: String,
    pub(crate) environment_id: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) redacted_summary: String,
    pub(crate) reason: Option<String>,
    pub(crate) available_decisions: Vec<ApprovalDecisionKind>,
    pub(crate) policy_fingerprint: String,
    pub(crate) created_at_ms: u64,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovalNotificationAction {
    pub(crate) label: &'static str,
    pub(crate) decision: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovalNotification {
    pub(crate) text: String,
    pub(crate) request_id: String,
    pub(crate) actions: Vec<ApprovalNotificationAction>,
}

/// Authenticated route on which an approval was requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelApprovalScope {
    pub(crate) channel: String,
    pub(crate) channel_config_id: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) peer_id: String,
    pub(crate) logical_session_id: String,
}

/// Core operation waiting for a decision.
#[derive(Clone, Debug)]
pub(crate) enum ChannelApprovalKind {
    Exec {
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    Patch,
}

pub(crate) struct ApprovalRequestMetadata {
    pub(crate) agent_id: String,
    pub(crate) policy_fingerprint: String,
    pub(crate) cwd: PathBuf,
    pub(crate) summary: String,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) reason: Option<String>,
}

/// Data required to route one response back to Core.
#[derive(Clone, Debug)]
pub(crate) struct PendingChannelApproval {
    pub(crate) envelope: ApprovalRequestEnvelope,
    pub(crate) request_id: String,
    pub(crate) core_approval_id: String,
    pub(crate) core_session_id: SessionId,
    pub(crate) scope: ChannelApprovalScope,
    pub(crate) kind: ChannelApprovalKind,
    pub(crate) capabilities: ApprovalClientCapabilities,
    expires_at: Instant,
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn truncate_utf8(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut output: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

fn redacted_command_summary(command: &[String]) -> String {
    const SECRET_MARKERS: &[&str] = &[
        "password",
        "passwd",
        "token",
        "secret",
        "api-key",
        "apikey",
        "authorization",
        "credential",
    ];
    let mut redact_next = false;
    let words = command.iter().map(|word| {
        let lower = word.to_ascii_lowercase();
        if redact_next {
            redact_next = false;
            return "<redacted>".to_owned();
        }
        if SECRET_MARKERS
            .iter()
            .any(|marker| lower == format!("--{marker}") || lower == format!("-{marker}"))
        {
            redact_next = true;
            return word.clone();
        }
        if let Some((key, _)) = word.split_once('=')
            && SECRET_MARKERS
                .iter()
                .any(|marker| key.to_ascii_lowercase().contains(marker))
        {
            return format!("{key}=<redacted>");
        }
        word.clone()
    });
    truncate_utf8(
        &super::redaction::redact_text_always(&words.collect::<Vec<_>>().join(" ")),
        512,
    )
}

impl PendingChannelApproval {
    #[must_use]
    pub(crate) fn supports_session_grant(&self) -> bool {
        self.capabilities.supports_session_grants
    }

    #[must_use]
    pub(crate) fn supports_persisted_rule(&self) -> bool {
        self.capabilities.supports_persisted_rules
            && matches!(
                self.kind,
                ChannelApprovalKind::Exec {
                    proposed_execpolicy_amendment: Some(_)
                }
            )
    }

    pub(crate) fn notification(&self, text: String) -> ApprovalNotification {
        let actions = self
            .envelope
            .available_decisions
            .iter()
            .map(|decision| match decision {
                ApprovalDecisionKind::ApproveOnce => ApprovalNotificationAction {
                    label: "Approve once",
                    decision: "approve-once",
                },
                ApprovalDecisionKind::ApproveSession => ApprovalNotificationAction {
                    label: "Approve session",
                    decision: "approve-session",
                },
                ApprovalDecisionKind::AllowRule => ApprovalNotificationAction {
                    label: "Allow rule",
                    decision: "allow-rule",
                },
                ApprovalDecisionKind::Deny => ApprovalNotificationAction {
                    label: "Deny",
                    decision: "deny",
                },
                ApprovalDecisionKind::Abort => ApprovalNotificationAction {
                    label: "Abort",
                    decision: "abort",
                },
            })
            .collect();
        ApprovalNotification {
            text,
            request_id: self.request_id.clone(),
            actions,
        }
    }

    fn as_persisted_request(&self) -> ExecApprovalRequest {
        let kind = match self.envelope.kind {
            ApprovalKind::Exec => "exec",
            ApprovalKind::Patch => "patch",
        };
        let available_decisions = self
            .envelope
            .available_decisions
            .iter()
            .map(|decision| match decision {
                ApprovalDecisionKind::ApproveOnce => "approve-once",
                ApprovalDecisionKind::ApproveSession => "approve-session",
                ApprovalDecisionKind::AllowRule => "allow-rule",
                ApprovalDecisionKind::Deny => "deny",
                ApprovalDecisionKind::Abort => "abort",
            })
            .map(ToOwned::to_owned)
            .collect();
        ExecApprovalRequest {
            id: self.envelope.id.clone(),
            command: self.envelope.redacted_summary.clone(),
            cwd: Some(self.envelope.cwd.to_string_lossy().into_owned()),
            security: Some(kind.to_owned()),
            ask: self.envelope.reason.clone(),
            agent_id: Some(self.envelope.agent_id.clone()),
            session_id: Some(self.envelope.logical_session_id.clone()),
            created_at_ms: self.envelope.created_at_ms,
            expires_at_ms: self.envelope.expires_at_ms,
            nonce: self.envelope.nonce.clone(),
            kind: Some(kind.to_owned()),
            channel_instance_id: self.envelope.channel_instance_id.clone(),
            account_id: self.envelope.account_id.clone(),
            peer_id: self.envelope.peer_id.clone(),
            logical_session_id: Some(self.envelope.logical_session_id.clone()),
            core_session_id: Some(self.envelope.core_session_id.clone()),
            turn_id: Some(self.envelope.turn_id.clone()),
            environment_id: self.envelope.environment_id.clone(),
            available_decisions,
            policy_fingerprint: Some(self.envelope.policy_fingerprint.clone()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalReplyAction {
    ApproveOnce,
    ApproveSession,
    AllowRule,
    Deny,
    Abort,
}

struct ParsedApprovalReply {
    action: ApprovalReplyAction,
    request_id: Option<String>,
}

/// Result of handling an approval-like Channel message.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChannelApprovalReplyOutcome {
    Resolved {
        request_id: String,
        decision: &'static str,
    },
    NoMatch,
    Ambiguous,
    UnsupportedDecision,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthenticatedApprovalOutcome {
    Resolved { decision: &'static str },
    NotCoordinated,
    NonceMismatch,
    UnsupportedDecision,
}

/// In-memory active approval registry. Durable audit/pending state is added by
/// the unified ApprovalCoordinator phase; active Core session handles are
/// intentionally never restored after a process restart.
#[derive(Default)]
pub(crate) struct ApprovalCoordinator {
    savfox_home: PathBuf,
    persistence_enabled: bool,
    pending: Mutex<HashMap<String, PendingChannelApproval>>,
}

impl ApprovalCoordinator {
    #[must_use]
    pub(crate) fn new(savfox_home: PathBuf) -> Self {
        Self {
            savfox_home,
            persistence_enabled: true,
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn register(
        &self,
        core_approval_id: String,
        core_session_id: SessionId,
        scope: ChannelApprovalScope,
        kind: ChannelApprovalKind,
        capabilities: ApprovalClientCapabilities,
        metadata: ApprovalRequestMetadata,
    ) -> Result<PendingChannelApproval, String> {
        let pending = self
            .register_with_ttl(
                core_approval_id,
                core_session_id,
                scope,
                kind,
                capabilities,
                metadata,
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        persist_pending_approval(&self.savfox_home, &pending.as_persisted_request()).await?;
        self.pending
            .lock()
            .await
            .insert(pending.request_id.clone(), pending.clone());
        Ok(pending)
    }

    async fn register_with_ttl(
        &self,
        core_approval_id: String,
        core_session_id: SessionId,
        scope: ChannelApprovalScope,
        kind: ChannelApprovalKind,
        capabilities: ApprovalClientCapabilities,
        metadata: ApprovalRequestMetadata,
        ttl: Duration,
    ) -> PendingChannelApproval {
        let request_id = uuid::Uuid::now_v7().to_string();
        let created_at_ms = unix_time_ms();
        let expires_at_ms =
            created_at_ms.saturating_add(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX));
        let approval_kind = match &kind {
            ChannelApprovalKind::Exec { .. } => ApprovalKind::Exec,
            ChannelApprovalKind::Patch => ApprovalKind::Patch,
        };
        let mut available_decisions = vec![
            ApprovalDecisionKind::ApproveOnce,
            ApprovalDecisionKind::Deny,
            ApprovalDecisionKind::Abort,
        ];
        if capabilities.supports_session_grants {
            available_decisions.insert(1, ApprovalDecisionKind::ApproveSession);
        }
        if capabilities.supports_persisted_rules
            && matches!(
                &kind,
                ChannelApprovalKind::Exec {
                    proposed_execpolicy_amendment: Some(_)
                }
            )
        {
            available_decisions.insert(2, ApprovalDecisionKind::AllowRule);
        }
        let redacted_summary = match (approval_kind, metadata.command.as_ref()) {
            (ApprovalKind::Exec, Some(command)) => redacted_command_summary(command),
            (ApprovalKind::Patch, _) | (ApprovalKind::Exec, None) => truncate_utf8(
                &super::redaction::redact_text_always(&metadata.summary),
                512,
            ),
        };
        let envelope = ApprovalRequestEnvelope {
            id: request_id.clone(),
            nonce: generate_approval_nonce(),
            kind: approval_kind,
            agent_id: metadata.agent_id,
            channel_instance_id: scope.channel_config_id.clone(),
            account_id: scope.account_id.clone(),
            peer_id: Some(scope.peer_id.clone()),
            logical_session_id: scope.logical_session_id.clone(),
            core_session_id: core_session_id.to_string(),
            // Core does not currently expose a separate turn id on approval
            // events. The approval operation id is the exact per-session
            // correlation key and is persisted in this compatibility field.
            turn_id: core_approval_id.clone(),
            environment_id: None,
            cwd: metadata.cwd,
            redacted_summary,
            reason: metadata
                .reason
                .map(|reason| truncate_utf8(&super::redaction::redact_text_always(&reason), 512)),
            available_decisions,
            policy_fingerprint: metadata.policy_fingerprint,
            created_at_ms,
            expires_at_ms,
        };
        PendingChannelApproval {
            envelope,
            request_id,
            core_approval_id,
            core_session_id,
            scope,
            kind,
            capabilities,
            expires_at: Instant::now() + ttl,
        }
    }

    #[cfg(test)]
    async fn register_for_test(
        &self,
        core_approval_id: String,
        core_session_id: SessionId,
        scope: ChannelApprovalScope,
        kind: ChannelApprovalKind,
        capabilities: ApprovalClientCapabilities,
        ttl: Duration,
    ) -> PendingChannelApproval {
        let pending = self
            .register_with_ttl(
                core_approval_id,
                core_session_id,
                scope,
                kind,
                capabilities,
                ApprovalRequestMetadata {
                    agent_id: "test-agent".to_owned(),
                    policy_fingerprint: "test-policy".to_owned(),
                    cwd: PathBuf::from("."),
                    summary: "test request".to_owned(),
                    command: None,
                    reason: None,
                },
                ttl,
            )
            .await;
        self.pending
            .lock()
            .await
            .insert(pending.request_id.clone(), pending.clone());
        pending
    }

    async fn persist_terminal(
        &self,
        pending: &PendingChannelApproval,
        approved: bool,
        resolved_by: &str,
        reason: &str,
    ) {
        if !self.persistence_enabled {
            return;
        }
        let resolution = ExecApprovalResolution {
            id: pending.request_id.clone(),
            approved,
            resolved_by: Some(resolved_by.to_owned()),
            reason: Some(reason.to_owned()),
            nonce: pending.envelope.nonce.clone(),
        };
        if let Err(error) = persist_resolved_approval(&self.savfox_home, &resolution).await {
            warn!(
                request_id = pending.request_id,
                %error,
                "failed to persist terminal approval state"
            );
        }
    }

    pub(crate) async fn remove(&self, request_id: &str) {
        let removed = self.pending.lock().await.remove(request_id);
        if let Some(pending) = removed {
            self.persist_terminal(
                &pending,
                false,
                "gateway",
                "approval request superseded or canceled",
            )
            .await;
        }
    }

    pub(crate) async fn is_pending(&self, request_id: &str) -> bool {
        self.pending.lock().await.contains_key(request_id)
    }

    pub(crate) async fn remove_for_core_session(&self, core_session_id: SessionId) {
        let removed = {
            let mut active = self.pending.lock().await;
            let ids = active
                .iter()
                .filter(|(_, pending)| pending.core_session_id == core_session_id)
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|request_id| active.remove(&request_id))
                .collect::<Vec<_>>()
        };
        for pending in removed {
            self.persist_terminal(
                &pending,
                false,
                "gateway",
                "Core session closed before approval resolution",
            )
            .await;
        }
    }

    async fn prune_expired(&self) {
        let now = Instant::now();
        let expired = {
            let mut active = self.pending.lock().await;
            let ids = active
                .iter()
                .filter(|(_, pending)| pending.expires_at <= now)
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|request_id| active.remove(&request_id))
                .collect::<Vec<_>>()
        };
        for pending in expired {
            self.persist_terminal(&pending, false, "gateway", "approval request expired")
                .await;
        }
    }

    async fn restore(&self, pending: PendingChannelApproval) {
        if pending.expires_at > Instant::now() {
            self.pending
                .lock()
                .await
                .insert(pending.request_id.clone(), pending);
        }
    }

    async fn take_for_reply(
        &self,
        scope: &ChannelApprovalScope,
        parsed: &ParsedApprovalReply,
    ) -> Result<PendingChannelApproval, ChannelApprovalReplyOutcome> {
        self.prune_expired().await;
        let mut pending = self.pending.lock().await;

        let request_id = if let Some(request_id) = parsed.request_id.as_deref() {
            let Some(request) = pending.get(request_id) else {
                return Err(ChannelApprovalReplyOutcome::NoMatch);
            };
            if &request.scope != scope {
                return Err(ChannelApprovalReplyOutcome::NoMatch);
            }
            request_id.to_owned()
        } else {
            let mut matches = pending
                .values()
                .filter(|request| &request.scope == scope)
                .map(|request| request.request_id.clone());
            let Some(request_id) = matches.next() else {
                return Err(ChannelApprovalReplyOutcome::NoMatch);
            };
            if matches.next().is_some() {
                return Err(ChannelApprovalReplyOutcome::Ambiguous);
            }
            request_id
        };

        let request = pending
            .get(&request_id)
            .expect("request id selected from the same registry");
        let supported = match parsed.action {
            ApprovalReplyAction::ApproveSession => request.supports_session_grant(),
            ApprovalReplyAction::AllowRule => request.supports_persisted_rule(),
            ApprovalReplyAction::ApproveOnce
            | ApprovalReplyAction::Deny
            | ApprovalReplyAction::Abort => true,
        };
        if !supported {
            return Err(ChannelApprovalReplyOutcome::UnsupportedDecision);
        }

        Ok(pending
            .remove(&request_id)
            .expect("request id selected from the same registry"))
    }

    async fn take_authenticated(
        &self,
        request_id: &str,
        nonce: &str,
        action: ApprovalReplyAction,
    ) -> Result<PendingChannelApproval, AuthenticatedApprovalOutcome> {
        use subtle::ConstantTimeEq;

        self.prune_expired().await;
        let mut pending = self.pending.lock().await;
        let Some(request) = pending.get(request_id) else {
            return Err(AuthenticatedApprovalOutcome::NotCoordinated);
        };
        if !bool::from(request.envelope.nonce.as_bytes().ct_eq(nonce.as_bytes())) {
            return Err(AuthenticatedApprovalOutcome::NonceMismatch);
        }
        let supported = match action {
            ApprovalReplyAction::ApproveSession => request.supports_session_grant(),
            ApprovalReplyAction::AllowRule => request.supports_persisted_rule(),
            ApprovalReplyAction::ApproveOnce
            | ApprovalReplyAction::Deny
            | ApprovalReplyAction::Abort => true,
        };
        if !supported {
            return Err(AuthenticatedApprovalOutcome::UnsupportedDecision);
        }
        Ok(pending
            .remove(request_id)
            .expect("authenticated request selected from the same registry"))
    }
}

fn command_request_id(text: &str, command: &str) -> Option<String> {
    let (head, tail) = text.split_once(':')?;
    if !head.eq_ignore_ascii_case(command) {
        return None;
    }
    let request_id = tail.trim();
    (!request_id.is_empty()).then(|| request_id.to_owned())
}

fn parse_reply(text: &str) -> Option<ParsedApprovalReply> {
    let text = text.trim();
    match text {
        "+" => {
            return Some(ParsedApprovalReply {
                action: ApprovalReplyAction::ApproveOnce,
                request_id: None,
            });
        }
        "-" => {
            return Some(ParsedApprovalReply {
                action: ApprovalReplyAction::Deny,
                request_id: None,
            });
        }
        _ => {}
    }

    for (command, action) in [
        ("approve", ApprovalReplyAction::ApproveOnce),
        ("approve-once", ApprovalReplyAction::ApproveOnce),
        ("approve-session", ApprovalReplyAction::ApproveSession),
        ("allow-rule", ApprovalReplyAction::AllowRule),
        ("deny", ApprovalReplyAction::Deny),
        ("abort", ApprovalReplyAction::Abort),
    ] {
        if let Some(request_id) = command_request_id(text, command) {
            return Some(ParsedApprovalReply {
                action,
                request_id: Some(request_id),
            });
        }
    }
    None
}

/// Whether a message should be consumed by the approval response path.
#[must_use]
pub(crate) fn looks_like_channel_approval_reply(text: &str) -> bool {
    parse_reply(text).is_some()
}

fn review_decision(
    pending: &PendingChannelApproval,
    action: ApprovalReplyAction,
) -> Option<ReviewDecision> {
    match action {
        ApprovalReplyAction::ApproveOnce => Some(ReviewDecision::Approved),
        ApprovalReplyAction::ApproveSession => Some(ReviewDecision::ApprovedForSession),
        ApprovalReplyAction::AllowRule => match &pending.kind {
            ChannelApprovalKind::Exec {
                proposed_execpolicy_amendment: Some(amendment),
            } => Some(ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment: amendment.clone(),
            }),
            ChannelApprovalKind::Exec {
                proposed_execpolicy_amendment: None,
            }
            | ChannelApprovalKind::Patch => None,
        },
        ApprovalReplyAction::Deny => Some(ReviewDecision::Denied),
        ApprovalReplyAction::Abort => Some(ReviewDecision::Abort),
    }
}

fn named_action(decision: &str) -> Option<ApprovalReplyAction> {
    match decision.trim().to_ascii_lowercase().as_str() {
        "approve" | "approved" | "approve-once" => Some(ApprovalReplyAction::ApproveOnce),
        "approve-session" => Some(ApprovalReplyAction::ApproveSession),
        "allow-rule" => Some(ApprovalReplyAction::AllowRule),
        "deny" | "denied" => Some(ApprovalReplyAction::Deny),
        "abort" => Some(ApprovalReplyAction::Abort),
        _ => None,
    }
}

/// Resolve a coordinator-owned request from an authenticated REST/WS caller.
/// Returns `NotCoordinated` so legacy node approval requests can continue
/// through their existing durable-only path during migration.
pub(crate) async fn resolve_authenticated_approval(
    channel: &GatewayChannel,
    request_id: &str,
    nonce: &str,
    decision: &str,
    resolved_by: Option<String>,
    reason: Option<String>,
) -> anyhow::Result<AuthenticatedApprovalOutcome> {
    let Some(action) = named_action(decision) else {
        return Ok(AuthenticatedApprovalOutcome::UnsupportedDecision);
    };
    let pending = match channel
        .approval_coordinator()
        .take_authenticated(request_id, nonce, action)
        .await
    {
        Ok(pending) => pending,
        Err(outcome) => return Ok(outcome),
    };
    let Some(review_decision) = review_decision(&pending, action) else {
        channel.approval_coordinator().restore(pending).await;
        return Ok(AuthenticatedApprovalOutcome::UnsupportedDecision);
    };
    let decision_name = review_decision.to_opaque_string();
    let approved = matches!(
        &review_decision,
        ReviewDecision::Approved
            | ReviewDecision::ApprovedForSession
            | ReviewDecision::ApprovedExecpolicyAmendment { .. }
    );
    let op = match pending.kind {
        ChannelApprovalKind::Exec { .. } => Op::ExecApproval {
            id: pending.core_approval_id.clone(),
            decision: review_decision,
        },
        ChannelApprovalKind::Patch => Op::PatchApproval {
            id: pending.core_approval_id.clone(),
            decision: review_decision,
        },
    };
    let session = match channel
        .session_manager()
        .get_session(pending.core_session_id)
        .await
    {
        Ok(session) => session,
        Err(error) => {
            channel.approval_coordinator().restore(pending).await;
            return Err(anyhow::anyhow!(
                "approval target Core session is unavailable: {error}"
            ));
        }
    };
    if let Err(error) = session.submit(op).await {
        channel.approval_coordinator().restore(pending).await;
        return Err(anyhow::anyhow!(
            "failed to submit authenticated approval: {error}"
        ));
    }

    let resolution = ExecApprovalResolution {
        id: pending.request_id,
        approved,
        resolved_by,
        reason: reason
            .map(|reason| super::redaction::redact_text_always(&reason))
            .or_else(|| Some(decision_name.to_owned())),
        nonce: pending.envelope.nonce,
    };
    if let Err(error) = persist_resolved_approval(&channel.config().savfox_home, &resolution).await
    {
        // Core has already consumed the decision; replay must remain closed.
        warn!(request_id = request_id, %error, "failed to persist authenticated approval audit");
    }
    Ok(AuthenticatedApprovalOutcome::Resolved {
        decision: decision_name,
    })
}

/// Resolve a text reply and deliver it to the exact Core session that emitted
/// the request.
pub(crate) async fn resolve_channel_approval_reply(
    channel: &GatewayChannel,
    scope: &ChannelApprovalScope,
    text: &str,
) -> anyhow::Result<ChannelApprovalReplyOutcome> {
    let Some(parsed) = parse_reply(text) else {
        return Ok(ChannelApprovalReplyOutcome::NoMatch);
    };
    let pending = match channel
        .approval_coordinator()
        .take_for_reply(scope, &parsed)
        .await
    {
        Ok(pending) => pending,
        Err(outcome) => return Ok(outcome),
    };
    let Some(decision) = review_decision(&pending, parsed.action) else {
        channel.approval_coordinator().restore(pending).await;
        return Ok(ChannelApprovalReplyOutcome::UnsupportedDecision);
    };
    let decision_name = decision.to_opaque_string();
    let approved = matches!(
        &decision,
        ReviewDecision::Approved
            | ReviewDecision::ApprovedForSession
            | ReviewDecision::ApprovedExecpolicyAmendment { .. }
    );
    let op = match pending.kind {
        ChannelApprovalKind::Exec { .. } => Op::ExecApproval {
            id: pending.core_approval_id.clone(),
            decision,
        },
        ChannelApprovalKind::Patch => Op::PatchApproval {
            id: pending.core_approval_id.clone(),
            decision,
        },
    };

    let session = match channel
        .session_manager()
        .get_session(pending.core_session_id)
        .await
    {
        Ok(session) => session,
        Err(error) => {
            channel.approval_coordinator().restore(pending).await;
            return Err(anyhow::anyhow!(
                "approval target Core session is unavailable: {error}"
            ));
        }
    };
    if let Err(error) = session.submit(op).await {
        channel.approval_coordinator().restore(pending).await;
        return Err(anyhow::anyhow!(
            "failed to submit correlated approval: {error}"
        ));
    }
    let resolution = ExecApprovalResolution {
        id: pending.request_id.clone(),
        approved,
        resolved_by: Some(format!("channel-peer:{}", pending.scope.peer_id)),
        reason: Some(decision_name.to_owned()),
        nonce: pending.envelope.nonce.clone(),
    };
    match persist_resolved_approval(&channel.config().savfox_home, &resolution).await {
        Ok(ResolveOutcome::Resolved) => {}
        Ok(
            ResolveOutcome::NotPending
            | ResolveOutcome::NonceMismatch
            | ResolveOutcome::LegacyMissingNonce,
        ) => {
            warn!(
                request_id = pending.request_id,
                "approval resolved in Core but durable audit state did not converge"
            );
        }
        Err(error) => {
            // Core already consumed the single-use decision. Never restore the
            // active request here, because doing so would permit a replay.
            warn!(
                request_id = pending.request_id,
                %error,
                "approval resolved in Core but durable audit persistence failed"
            );
        }
    }

    Ok(ChannelApprovalReplyOutcome::Resolved {
        request_id: pending.request_id,
        decision: decision_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(peer: &str) -> ChannelApprovalScope {
        ChannelApprovalScope {
            channel: "telegram:room".to_owned(),
            channel_config_id: Some("telegram-main".to_owned()),
            account_id: Some("account-main".to_owned()),
            peer_id: peer.to_owned(),
            logical_session_id: "session-1".to_owned(),
        }
    }

    fn capabilities() -> ApprovalClientCapabilities {
        ApprovalClientCapabilities::interactive()
    }

    #[tokio::test]
    async fn bare_reply_requires_one_pending_request_in_the_same_scope() {
        let registry = ApprovalCoordinator::default();
        let first = registry
            .register_for_test(
                "core-1".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        let _second = registry
            .register_for_test(
                "core-2".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        let parsed = parse_reply("+").expect("reply");
        assert!(matches!(
            registry.take_for_reply(&scope("alice"), &parsed).await,
            Err(ChannelApprovalReplyOutcome::Ambiguous)
        ));

        registry.remove(&first.request_id).await;
        assert!(
            registry
                .take_for_reply(&scope("alice"), &parsed)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn request_id_cannot_cross_authenticated_peer_scope() {
        let registry = ApprovalCoordinator::default();
        let pending = registry
            .register_for_test(
                "core-1".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        let parsed =
            parse_reply(&format!("approve:{}", pending.request_id)).expect("targeted reply");
        assert!(matches!(
            registry.take_for_reply(&scope("mallory"), &parsed).await,
            Err(ChannelApprovalReplyOutcome::NoMatch)
        ));
    }

    #[tokio::test]
    async fn allow_rule_requires_a_server_proposed_amendment() {
        let registry = ApprovalCoordinator::default();
        let pending = registry
            .register_for_test(
                "core-1".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Exec {
                    proposed_execpolicy_amendment: None,
                },
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        let parsed =
            parse_reply(&format!("allow-rule:{}", pending.request_id)).expect("rule reply");
        assert!(matches!(
            registry.take_for_reply(&scope("alice"), &parsed).await,
            Err(ChannelApprovalReplyOutcome::UnsupportedDecision)
        ));
    }

    #[tokio::test]
    async fn expired_request_is_removed_before_matching() {
        let registry = ApprovalCoordinator::default();
        let pending = registry
            .register_for_test(
                "core-1".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                Duration::ZERO,
            )
            .await;
        let parsed =
            parse_reply(&format!("approve:{}", pending.request_id)).expect("targeted reply");
        assert!(matches!(
            registry.take_for_reply(&scope("alice"), &parsed).await,
            Err(ChannelApprovalReplyOutcome::NoMatch)
        ));
        assert!(registry.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn authenticated_resolution_requires_nonce_and_is_single_use() {
        let registry = ApprovalCoordinator::default();
        let pending = registry
            .register_for_test(
                "core-1".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;

        assert!(matches!(
            registry
                .take_authenticated(
                    &pending.request_id,
                    "wrong-nonce",
                    ApprovalReplyAction::ApproveOnce,
                )
                .await,
            Err(AuthenticatedApprovalOutcome::NonceMismatch)
        ));
        assert!(
            registry
                .pending
                .lock()
                .await
                .contains_key(&pending.request_id)
        );

        assert!(
            registry
                .take_authenticated(
                    &pending.request_id,
                    &pending.envelope.nonce,
                    ApprovalReplyAction::ApproveOnce,
                )
                .await
                .is_ok()
        );
        assert!(matches!(
            registry
                .take_authenticated(
                    &pending.request_id,
                    &pending.envelope.nonce,
                    ApprovalReplyAction::ApproveOnce,
                )
                .await,
            Err(AuthenticatedApprovalOutcome::NotCoordinated)
        ));
    }

    #[tokio::test]
    async fn authenticated_resolution_rejects_expired_and_unsupported_actions() {
        let registry = ApprovalCoordinator::default();
        let expired = registry
            .register_for_test(
                "core-expired".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Patch,
                capabilities(),
                Duration::ZERO,
            )
            .await;
        assert!(matches!(
            registry
                .take_authenticated(
                    &expired.request_id,
                    &expired.envelope.nonce,
                    ApprovalReplyAction::ApproveOnce,
                )
                .await,
            Err(AuthenticatedApprovalOutcome::NotCoordinated)
        ));

        let no_rule = registry
            .register_for_test(
                "core-no-rule".to_owned(),
                SessionId::new(),
                scope("alice"),
                ChannelApprovalKind::Exec {
                    proposed_execpolicy_amendment: None,
                },
                capabilities(),
                DEFAULT_APPROVAL_TTL,
            )
            .await;
        assert!(matches!(
            registry
                .take_authenticated(
                    &no_rule.request_id,
                    &no_rule.envelope.nonce,
                    ApprovalReplyAction::AllowRule,
                )
                .await,
            Err(AuthenticatedApprovalOutcome::UnsupportedDecision)
        ));
        assert!(
            registry
                .pending
                .lock()
                .await
                .contains_key(&no_rule.request_id)
        );
    }

    #[test]
    fn ordinary_chat_is_not_consumed_as_an_approval() {
        assert!(!looks_like_channel_approval_reply("approve this refactor"));
        assert!(!looks_like_channel_approval_reply("yes"));
        assert!(looks_like_channel_approval_reply("approve:request-id"));
    }

    #[test]
    fn persisted_command_summary_redacts_multiple_secret_shapes() {
        let summary = redacted_command_summary(&[
            "curl".to_owned(),
            "-H".to_owned(),
            "Authorization: Bearer abcdefghijklmnop".to_owned(),
            "token=plain-secret".to_owned(),
        ]);
        assert!(!summary.contains("abcdefghijklmnop"));
        assert!(!summary.contains("plain-secret"));
    }
}
