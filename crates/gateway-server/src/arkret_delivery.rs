//! Durable projection of private Savfox execution sessions into Arkret.
//!
//! This module deliberately stores delivery checkpoints rather than local chat
//! messages.  The file is local sensitive state and is written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

const STORE_VERSION: u32 = 1;
const MAX_PUBLIC_SUMMARY_CHARS: usize = 6_000;
const MAX_REMOTE_CONTEXT_EVENTS: usize = 128;

fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteConversationKey {
    pub channel_config_id: String,
    pub account_id: String,
    pub realm_id: String,
    pub strand_id: String,
    pub stream_ref: arkret::CommitStreamRef,
}

impl RemoteConversationKey {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("channel_config_id", &self.channel_config_id),
            ("account_id", &self.account_id),
            ("realm_id", &self.realm_id),
            ("strand_id", &self.strand_id),
        ] {
            anyhow::ensure!(!value.trim().is_empty(), "Arkret {name} cannot be empty");
        }
        anyhow::ensure!(
            self.stream_ref.realm_id().as_str() == self.realm_id,
            "Arkret conversation stream belongs to another Realm"
        );
        Ok(())
    }

    #[must_use]
    pub(crate) fn routing_scope(&self) -> String {
        let stream_digest = arkret::canonical::canonical_sha256(&self.stream_ref)
            .expect("closed SDK CommitStreamRef serializes canonically");
        format!(
            "{}:{}:{}:{}:{}",
            self.channel_config_id, self.account_id, self.realm_id, self.strand_id, stream_digest
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArkretDeliveryMode {
    TaskDelivery,
    #[default]
    InteractiveChat,
}

impl ArkretDeliveryMode {
    fn request_scoped(self, stream: &arkret::CommitStreamRef) -> bool {
        self == Self::TaskDelivery || matches!(stream, arkret::CommitStreamRef::Sidecar { .. })
    }

    #[must_use]
    pub(crate) fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("task_delivery") => Self::TaskDelivery,
            _ => Self::InteractiveChat,
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TaskDelivery => "task_delivery",
            Self::InteractiveChat => "interactive_chat",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryState {
    #[default]
    Accepted,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryCheckpointKind {
    Accepted,
    Milestone,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl DeliveryCheckpointKind {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "accepted" => Ok(Self::Accepted),
            "milestone" => Ok(Self::Milestone),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            _ => anyhow::bail!("unsupported Arkret checkpoint kind '{value}'"),
        }
    }

    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Milestone => "Milestone",
            Self::Blocked => "Blocked",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    #[must_use]
    fn state(self) -> DeliveryState {
        match self {
            Self::Accepted => DeliveryState::Accepted,
            Self::Milestone => DeliveryState::Running,
            Self::Blocked => DeliveryState::Blocked,
            Self::Completed => DeliveryState::Completed,
            Self::Failed => DeliveryState::Failed,
            Self::Cancelled => DeliveryState::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryVisibility {
    #[default]
    RemotePublic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryArtifact {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryCheckpoint {
    pub checkpoint_id: Uuid,
    pub binding_id: Uuid,
    pub sequence: u64,
    pub kind: DeliveryCheckpointKind,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<DeliveryArtifact>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    pub source_revision: u64,
    pub visibility: DeliveryVisibility,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArkretExecutionBinding {
    pub binding_id: Uuid,
    pub local_session_id: String,
    #[serde(flatten)]
    pub conversation: RemoteConversationKey,
    pub source_event_id: String,
    pub source_sender_did: String,
    pub agent_sender_did: String,
    pub mode: ArkretDeliveryMode,
    pub state: DeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ingested_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_published_checkpoint_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_published_event_id: Option<String>,
    pub public_summary_revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteContextEvent {
    pub event_id: String,
    pub sender_did: String,
    pub sender_kind: String,
    pub body: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteContextSnapshot {
    #[serde(default)]
    pub events: Vec<RemoteContextEvent>,
    #[serde(default)]
    pub filtered_events: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_unavailable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryOutboxState {
    Pending,
    Published,
    Retryable,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryOutboxItem {
    checkpoint: DeliveryCheckpoint,
    rendered_body: String,
    state: DeliveryOutboxState,
    attempts: u32,
    /// Exact SDK producer submission retained before the account queue or
    /// network sees it. A checkpoint UUID never substitutes for an Event id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen_submission: Option<garth::FrozenMessageSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(default = "default_initiated_by")]
    initiated_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_attempt_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

fn default_initiated_by() -> String {
    "agent_policy".to_owned()
}

#[derive(Debug, Clone)]
struct PendingDelivery {
    binding: ArkretExecutionBinding,
    checkpoint: DeliveryCheckpoint,
    rendered_body: String,
    initiated_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryStoreFile {
    version: u32,
    #[serde(default)]
    bindings: BTreeMap<Uuid, ArkretExecutionBinding>,
    #[serde(default, with = "conversation_context_entries")]
    remote_context: BTreeMap<RemoteConversationKey, RemoteContextSnapshot>,
    #[serde(default)]
    outbox: BTreeMap<Uuid, DeliveryOutboxItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_reply_ledger: Option<NativeReplyLedger>,
}

// Structured conversation keys are persisted as entries, never JSON object keys.
mod conversation_context_entries {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        entries: &BTreeMap<RemoteConversationKey, RemoteContextSnapshot>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        entries.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<RemoteConversationKey, RemoteContextSnapshot>, D::Error> {
        let entries =
            Vec::<(RemoteConversationKey, RemoteContextSnapshot)>::deserialize(deserializer)?;
        let mut result = BTreeMap::new();
        for (key, value) in entries {
            key.validate().map_err(serde::de::Error::custom)?;
            if result.insert(key, value).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate remote conversation entry",
                ));
            }
        }
        Ok(result)
    }
}

impl Default for DeliveryStoreFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            bindings: BTreeMap::new(),
            remote_context: BTreeMap::new(),
            outbox: BTreeMap::new(),
            native_reply_ledger: None,
        }
    }
}

// Process-local custody only. These types are not a protocol or diagnostics surface.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeReplySlot {
    pub conversation: RemoteConversationKey,
    pub source: arkret::CommittedEventRef,
    pub scope: arkret::ScopeRef,
    pub strand_id: arkret::StrandId,
    pub agent: arkret::AccountId,
    pub controller: arkret::AccountId,
    pub caller: arkret::AccountId,
}

impl NativeReplySlot {
    fn validate(&self) -> anyhow::Result<()> {
        self.conversation.validate()?;
        self.agent.validate()?;
        self.controller.validate()?;
        self.caller.validate()?;
        anyhow::ensure!(
            matches!(
                self.scope,
                arkret::ScopeRef::Realm { .. } | arkret::ScopeRef::Circle { .. }
            ),
            "native reply requires a shared scope"
        );
        anyhow::ensure!(
            arkret::CommitStreamRef::from_scope(&self.scope, None)? == self.source.stream_ref
                && self.source.stream_ref == self.conversation.stream_ref
                && self.strand_id.as_str() == self.conversation.strand_id
                && self.source.stream_position > 0,
            "native reply slot does not bind its source and target"
        );
        Ok(())
    }

    fn key(&self) -> anyhow::Result<String> {
        Ok(arkret::canonical::canonical_sha256(self)?)
    }

    fn lane(&self) -> anyhow::Result<String> {
        // A source can be consumed at most once for an Agent in one stream,
        // including across controller/config changes and different Strands.
        Ok(arkret::canonical::canonical_sha256(&(
            &self.agent,
            &self.source.stream_ref,
        ))?)
    }
}

pub(crate) enum NativeReplyPreparation {
    // Process-only suppression, NOT a reply or accepted receipt. Only sources
    // already behind a retained real onboarding cutoff can use this result.
    SuppressedBeforeBootstrap,
    Ready,
    StartedUnknown,
    OutputRetained {
        text: String,
        submission: Option<garth::FrozenMessageSubmission>,
    },
    Accepted {
        receipt: arkret::CommittedEventRef,
    },
}

// Not Clone, Debug or serde: only a successful durable CAS can issue this capability.
pub(crate) struct NativeReplyHandoff {
    key: String,
    input_digest: arkret::Hash,
    attempt: Uuid,
    endpoint: String,
    provider_request_digest: arkret::Hash,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NativeReplyState {
    Ready,
    HandoffStarted,
    Unknown,
    OutputRetained,
    Accepted,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeReplyEntry {
    slot: NativeReplySlot,
    input_digest: arkret::Hash,
    state: NativeReplyState,
    attempt: Option<Uuid>,
    endpoint: Option<String>,
    provider_request_digest: Option<arkret::Hash>,
    output: Option<String>,
    output_digest: Option<arkret::Hash>,
    submission: Option<garth::FrozenMessageSubmission>,
    submission_digest: Option<arkret::Hash>,
    accepted: Option<arkret::RealmCommit>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeReplyLedger {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    onboarding_receipts: BTreeMap<String, NativeReplyOnboardingReceipt>,
    // Initialized from real onboarding stream observations, never remote_context.
    high_water: BTreeMap<String, u64>,
    entries: BTreeMap<String, NativeReplyEntry>,
}

// This is local private custody metadata, not an exported protocol receipt.
// The explicit validated configuration owner supplies the canonical digest;
// cached input and a missing ledger never constitute onboarding authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeReplyOnboardingReceipt {
    pub agent: arkret::AccountId,
    pub controller: arkret::AccountId,
    pub onboarding_digest: arkret::Hash,
    pub bootstraps: Vec<arkret::CommittedEventRef>,
    // Real retained ledger cutoffs. Existing lanes may be behind a freshly
    // observed head; keep their original watermark without inventing an
    // accepted reference for that position.
    cutoffs: BTreeMap<String, u64>,
}

impl NativeReplyOnboardingReceipt {
    fn key(&self) -> anyhow::Result<String> {
        Ok(arkret::canonical::canonical_sha256(&(
            &self.agent,
            &self.controller,
            &self.onboarding_digest,
        ))?)
    }

    fn validated_lanes(&self) -> anyhow::Result<BTreeMap<String, arkret::CommittedEventRef>> {
        self.agent.validate()?;
        self.controller.validate()?;
        native_require_sha256(&self.onboarding_digest)?;
        anyhow::ensure!(
            self.bootstraps.len() <= MAX_REMOTE_CONTEXT_EVENTS,
            "native onboarding requires a bounded batch"
        );
        let mut lanes = BTreeMap::new();
        for bootstrap in &self.bootstraps {
            anyhow::ensure!(
                bootstrap.stream_position > 0,
                "native bootstrap has no accepted position"
            );
            anyhow::ensure!(
                matches!(
                    bootstrap.stream_ref,
                    arkret::CommitStreamRef::Realm { .. } | arkret::CommitStreamRef::Circle { .. }
                ),
                "native bootstrap requires a shared stream"
            );
            let lane = arkret::canonical::canonical_sha256(&(&self.agent, &bootstrap.stream_ref))?;
            anyhow::ensure!(
                lanes.insert(lane, bootstrap.clone()).is_none(),
                "native onboarding contains a duplicate stream"
            );
        }
        Ok(lanes)
    }
}

impl std::fmt::Debug for NativeReplyLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeReplyLedger([private])")
    }
}

fn native_hash(bytes: &[u8]) -> anyhow::Result<arkret::Hash> {
    Ok(arkret::Hash::new(arkret::canonical::sha256_digest(bytes))?)
}

fn native_require_sha256(digest: &arkret::Hash) -> anyhow::Result<()> {
    anyhow::ensure!(
        digest.as_str().starts_with("sha256:"),
        "native digest requires canonical SHA256"
    );
    Ok(())
}

fn native_endpoint(endpoint: &str) -> anyhow::Result<()> {
    // The factory chooses this endpoint; it must still be passed exactly to the
    // physical transport. Validation here does not resolve or authorize it.
    let parsed = reqwest::Url::parse(endpoint)
        .map_err(|_| anyhow::anyhow!("native provider endpoint is invalid"))?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "https" | "http")
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none(),
        "native provider endpoint is invalid"
    );
    Ok(())
}

fn native_submission_matches(
    slot: &NativeReplySlot,
    submission: &garth::FrozenMessageSubmission,
) -> anyhow::Result<()> {
    ArkretExecutionBindingStore::validate_submission_target(submission, &slot.conversation)?;
    let event = &submission.request().submission.event;
    anyhow::ensure!(
        event.kind == arkret::EventKind::MessageCreate
            && submission.state() == garth::MessageSubmissionState::Queued
            && event.scope_ref == slot.scope
            && event.actor_id.as_account_id() == Some(&slot.agent),
        "native submission does not bind its Agent and fixed reply target"
    );
    Ok(())
}

impl NativeReplyLedger {
    fn validate(&self) -> anyhow::Result<()> {
        let mut controllers = BTreeMap::new();
        for (key, receipt) in &self.onboarding_receipts {
            let agent_key = arkret::canonical::canonical_sha256(&receipt.agent)?;
            if let Some(original) = controllers.insert(agent_key, receipt.controller.clone()) {
                anyhow::ensure!(
                    original == receipt.controller,
                    "native onboarding controller changed"
                );
            }
            anyhow::ensure!(
                *key == receipt.key()?,
                "native onboarding receipt key mismatch"
            );
            let lanes = receipt.validated_lanes()?;
            anyhow::ensure!(
                self.onboarding_receipts
                    .values()
                    .any(|first| first.agent == receipt.agent
                        && first.controller == receipt.controller
                        && first.bootstraps.is_empty()),
                "native receipt has no retained exact first configuration"
            );
            anyhow::ensure!(
                receipt.cutoffs.len() == lanes.len(),
                "native onboarding receipt cutoff set mismatch"
            );
            for lane in lanes.keys() {
                let cutoff = receipt
                    .cutoffs
                    .get(lane)
                    .ok_or_else(|| anyhow::anyhow!("native onboarding receipt cutoff is absent"))?;
                anyhow::ensure!(
                    self.high_water
                        .get(lane)
                        .is_some_and(|water| water >= cutoff),
                    "native onboarding receipt watermark mismatch"
                );
            }
        }
        for lane in self.high_water.keys() {
            anyhow::ensure!(
                self.onboarding_receipts
                    .values()
                    .any(|receipt| receipt.cutoffs.contains_key(lane)),
                "native watermark has no retained onboarding receipt"
            );
        }
        for (key, entry) in &self.entries {
            entry.slot.validate()?;
            self.require_slot_onboarding(&entry.slot)?;
            native_require_sha256(&entry.input_digest)?;
            if let Some(digest) = &entry.provider_request_digest {
                native_require_sha256(digest)?;
            }
            anyhow::ensure!(*key == entry.slot.key()?, "native slot key mismatch");
            anyhow::ensure!(
                self.high_water
                    .get(&entry.slot.lane()?)
                    .is_some_and(|position| *position >= entry.slot.source.stream_position),
                "native source watermark mismatch"
            );
            let started = entry.state != NativeReplyState::Ready;
            anyhow::ensure!(
                entry.attempt.is_some() == started
                    && entry.endpoint.is_some() == started
                    && entry.provider_request_digest.is_some() == started,
                "native handoff state is incomplete"
            );
            if let Some(endpoint) = &entry.endpoint {
                native_endpoint(endpoint)?;
            }
            let has_output = matches!(
                entry.state,
                NativeReplyState::OutputRetained | NativeReplyState::Accepted
            );
            anyhow::ensure!(
                entry.output.is_some() == has_output && entry.output_digest.is_some() == has_output,
                "native output state is incomplete"
            );
            if let Some(output) = &entry.output {
                anyhow::ensure!(
                    !output.is_empty() && output.len() <= 1024 * 1024,
                    "native output bound exceeded"
                );
                anyhow::ensure!(
                    Some(native_hash(output.as_bytes())?) == entry.output_digest,
                    "native output digest mismatch"
                );
            }
            anyhow::ensure!(
                entry.submission.is_some() == entry.submission_digest.is_some()
                    && (entry.submission.is_none() || has_output),
                "native submission state is incomplete"
            );
            if let Some(submission) = &entry.submission {
                native_submission_matches(&entry.slot, submission)?;
                anyhow::ensure!(
                    Some(native_hash(submission.canonical_submission_bytes())?)
                        == entry.submission_digest,
                    "native submission digest mismatch"
                );
            }
            anyhow::ensure!(
                entry.accepted.is_some() == (entry.state == NativeReplyState::Accepted),
                "native accepted state is incomplete"
            );
            if let Some(commit) = &entry.accepted {
                commit.validate_content_address()?;
                let mut frozen = entry.submission.clone().ok_or_else(|| {
                    anyhow::anyhow!("native accepted reply has no frozen submission")
                })?;
                frozen.apply_outcome(arkret::AuthoritySubmitOutcome::Accepted {
                    status: arkret::AuthorityCommitStatus::Committed,
                    commit: commit.clone(),
                })?;
            }
        }
        Ok(())
    }

    fn require_slot_onboarding(&self, slot: &NativeReplySlot) -> anyhow::Result<()> {
        let lane = slot.lane()?;
        anyhow::ensure!(
            self.onboarding_receipts
                .values()
                .any(|receipt| receipt.agent == slot.agent
                    && receipt.controller == slot.controller
                    && receipt.cutoffs.contains_key(&lane)),
            "native slot has no exact Agent/controller shared onboarding receipt"
        );
        Ok(())
    }

    fn entry_mut(
        &mut self,
        slot: &NativeReplySlot,
        input: &arkret::Hash,
    ) -> anyhow::Result<&mut NativeReplyEntry> {
        let entry = self
            .entries
            .get_mut(&slot.key()?)
            .ok_or_else(|| anyhow::anyhow!("native reply was not prepared"))?;
        anyhow::ensure!(
            entry.slot == *slot && entry.input_digest == *input,
            "native reply input changed"
        );
        Ok(entry)
    }
}

fn native_handoff_entry<'a>(
    state: &'a mut DeliveryStoreFile,
    handoff: &NativeReplyHandoff,
) -> anyhow::Result<&'a mut NativeReplyEntry> {
    let entry = state
        .native_reply_ledger
        .as_mut()
        .and_then(|ledger| ledger.entries.get_mut(&handoff.key))
        .ok_or_else(|| anyhow::anyhow!("native handoff is not retained"))?;
    anyhow::ensure!(
        entry.input_digest == handoff.input_digest
            && entry.attempt == Some(handoff.attempt)
            && entry.endpoint.as_ref() == Some(&handoff.endpoint)
            && entry.provider_request_digest.as_ref() == Some(&handoff.provider_request_digest),
        "native physical handoff binding changed"
    );
    Ok(entry)
}

fn native_create_parent(path: &Path, home: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("native delivery path has no parent"))?;
    // Include the home entry and any missing ancestors, but stop at the
    // existing directory that contains them. Unchanged system ancestors do
    // not need a durability barrier and may forbid writable handles.
    let directory_path = |directory: &Path| {
        if directory.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            directory.to_owned()
        }
    };
    let mut boundary = directory_path(home.parent().unwrap_or(home));
    loop {
        match std::fs::metadata(&boundary) {
            Ok(metadata) => {
                anyhow::ensure!(metadata.is_dir(), "native home parent is not a directory");
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                boundary = directory_path(boundary.parent().ok_or_else(|| {
                    anyhow::anyhow!("native home has no existing parent directory")
                })?);
            }
            Err(error) => return Err(error.into()),
        }
    }
    std::fs::create_dir_all(parent)?;
    // Private binding initialization may already have created the home and
    // ledger directories without syncing them; include those entries too.
    let mut directories = Vec::new();
    for directory in parent.ancestors() {
        let directory = directory_path(directory);
        let reached_boundary = directory == boundary;
        directories.push(directory);
        if reached_boundary {
            return Ok(directories);
        }
    }
    anyhow::bail!("native delivery directory is outside its home boundary")
}

fn native_sync_directory(path: &Path) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FlushFileBuffers requires GENERIC_WRITE even for a directory handle.
        options.write(true);
        options.custom_flags(0x02000000); // FILE_FLAG_BACKUP_SEMANTICS
    }
    options.open(path)?.sync_all()?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ArkretExecutionBindingStore {
    path: PathBuf,
    home: PathBuf,
}

impl ArkretExecutionBindingStore {
    #[must_use]
    pub(crate) fn new(savfox_home: &Path) -> Self {
        Self {
            home: savfox_home.to_owned(),
            path: savfox_home
                .join("gateway")
                .join("arkret-delivery")
                .join("bindings.json"),
        }
    }

    /// Lock a separate inode so atomic replacement of bindings.json cannot
    /// let another gateway process choose a different first submission.
    async fn lock(&self) -> anyhow::Result<(tokio::sync::MutexGuard<'static, ()>, std::fs::File)> {
        let process_guard = store_lock().lock().await;
        let path = self.path.with_extension("lock");
        let file = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let file = options.open(&path)?;
            file.lock()?;
            Ok(file)
        })
        .await??;
        Ok((process_guard, file))
    }

    async fn load_unlocked(&self) -> anyhow::Result<DeliveryStoreFile> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) if bytes.is_empty() => Ok(DeliveryStoreFile::default()),
            Ok(bytes) => {
                let state: DeliveryStoreFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", self.path.display()))?;
                anyhow::ensure!(
                    state.version == STORE_VERSION,
                    "unsupported Arkret delivery store version {}",
                    state.version
                );
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(DeliveryStoreFile::default())
            }
            Err(error) => Err(error).with_context(|| format!("read {}", self.path.display())),
        }
    }

    async fn save_unlocked(&self, state: &DeliveryStoreFile) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        // No await between replacement and directory fsync: the caller's
        // existing separate-inode FileLock cannot be dropped by cancellation
        // while an orphan async writer later overwrites the native ledger.
        savfox_utils::fs::write_atomically(&self.path, &bytes, Some(0o600))
            .with_context(|| format!("persist {}", self.path.display()))?;
        native_sync_directory(
            self.path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("delivery path has no parent"))?,
        )
    }

    /// ONLY the original explicit first configuration owner may call this:
    /// previous configuration is absent, the complete accepted pairing and
    /// immutable full Agent/controller binding have already been verified.
    /// The canonical intent includes those original facts. File absence is
    /// never that authority; the listener/cold factory MUST NOT call this.
    /// An empty receipt permits Welcome/private maintenance, NOT model Ready.
    pub(crate) async fn native_initialize_first_config(
        &self,
        agent: arkret::AccountId,
        controller: arkret::AccountId,
        onboarding_digest: arkret::Hash,
    ) -> anyhow::Result<NativeReplyOnboardingReceipt> {
        let candidate = NativeReplyOnboardingReceipt {
            agent,
            controller,
            onboarding_digest,
            bootstraps: Vec::new(),
            cutoffs: BTreeMap::new(),
        };
        candidate.validated_lanes()?;
        let key = candidate.key()?;
        self.native_transaction(false, move |state| {
            let ledger = state
                .native_reply_ledger
                .get_or_insert_with(|| NativeReplyLedger {
                    onboarding_receipts: BTreeMap::new(),
                    high_water: BTreeMap::new(),
                    entries: BTreeMap::new(),
                });
            if let Some(original) = ledger.onboarding_receipts.get(&key) {
                anyhow::ensure!(
                    *original == candidate,
                    "native first configuration intent changed"
                );
                return Ok(original.clone());
            }
            anyhow::ensure!(
                !ledger
                    .onboarding_receipts
                    .values()
                    .any(|receipt| receipt.agent == candidate.agent),
                "native first configuration was already retained"
            );
            ledger.onboarding_receipts.insert(key, candidate.clone());
            Ok(candidate)
        })
        .await
    }

    /// Called only by the explicit configuration/onboarding owner after it has
    /// validated the complete candidate and obtained all real accepted heads.
    /// One transaction initializes every new lane and its receipt. The digest
    /// binds that original configuration intent, not a subsequently observed
    /// head. An exact unknown retry returns the FIRST receipt; fresh heads on
    /// the retry never reset the original cutoff or existing watermarks.
    /// The listener/cold factory must still use required-existing resume only.
    pub(crate) async fn native_initialize_batch(
        &self,
        agent: arkret::AccountId,
        controller: arkret::AccountId,
        onboarding_digest: arkret::Hash,
        bootstraps: Vec<arkret::CommittedEventRef>,
    ) -> anyhow::Result<NativeReplyOnboardingReceipt> {
        let mut candidate = NativeReplyOnboardingReceipt {
            agent,
            controller,
            onboarding_digest,
            bootstraps,
            cutoffs: BTreeMap::new(),
        };
        let lanes = candidate.validated_lanes()?;
        anyhow::ensure!(
            !lanes.is_empty(),
            "native shared onboarding requires nonempty accepted heads"
        );
        // Canonical ordering makes the retained receipt independent of caller
        // pagination order. Reject duplicate streams before ANY mutation.
        candidate.bootstraps = lanes.values().cloned().collect();
        let receipt_key = candidate.key()?;
        self.native_transaction(true, move |state| {
            let ledger = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native first configuration receipt is missing"))?;
            anyhow::ensure!(
                ledger
                    .onboarding_receipts
                    .values()
                    .any(|receipt| receipt.agent == candidate.agent
                        && receipt.controller == candidate.controller
                        && receipt.bootstraps.is_empty()),
                "native first configuration has no exact Agent/controller receipt"
            );
            if let Some(original) = ledger.onboarding_receipts.get(&receipt_key) {
                anyhow::ensure!(
                    original.agent == candidate.agent
                        && original.controller == candidate.controller
                        && original.onboarding_digest == candidate.onboarding_digest
                        && !original.bootstraps.is_empty(),
                    "native onboarding intent changed"
                );
                return Ok(original.clone());
            }
            for (lane, bootstrap) in lanes {
                // Existing lanes may have already consumed newer events, or be
                // behind a fresh observation. Preserve their ACTUAL cutoff.
                let cutoff = *ledger
                    .high_water
                    .entry(lane.clone())
                    .or_insert(bootstrap.stream_position);
                candidate.cutoffs.insert(lane, cutoff);
            }
            ledger
                .onboarding_receipts
                .insert(receipt_key, candidate.clone());
            Ok(candidate)
        })
        .await
    }

    /// Required-existing immutable first receipt lookup for subsequent owner
    /// Save. Current scope/key edits must NOT replace its original digest.
    /// This is private custody recovery, not a current authorization verdict.
    pub(crate) async fn native_first_configuration_receipt(
        &self,
        agent: arkret::AccountId,
        controller: arkret::AccountId,
    ) -> anyhow::Result<NativeReplyOnboardingReceipt> {
        agent.validate()?;
        controller.validate()?;
        self.native_transaction(true, move |state| {
            let ledger = state
                .native_reply_ledger
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native first configuration receipt is missing"))?;
            let mut matches = ledger.onboarding_receipts.values().filter(|receipt| {
                receipt.agent == agent
                    && receipt.controller == controller
                    && receipt.bootstraps.is_empty()
            });
            let first = matches.next().ok_or_else(|| {
                anyhow::anyhow!("native exact first configuration receipt is missing")
            })?;
            anyhow::ensure!(
                matches.next().is_none(),
                "native first configuration receipt is ambiguous"
            );
            Ok(first.clone())
        })
        .await
    }

    /// Exact configuration retry lookup, before observing replacement heads.
    /// This never initializes a lane; missing/corrupt/private-only stores fail.
    /// The owner must separately validate its current configuration authority.
    pub(crate) async fn native_onboarding_receipt(
        &self,
        agent: arkret::AccountId,
        controller: arkret::AccountId,
        onboarding_digest: arkret::Hash,
    ) -> anyhow::Result<Option<NativeReplyOnboardingReceipt>> {
        agent.validate()?;
        controller.validate()?;
        native_require_sha256(&onboarding_digest)?;
        let key = arkret::canonical::canonical_sha256(&(&agent, &controller, &onboarding_digest))?;
        self.native_transaction(true, move |state| {
            let ledger = state
                .native_reply_ledger
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?;
            anyhow::ensure!(
                ledger
                    .onboarding_receipts
                    .values()
                    .any(|receipt| receipt.agent == agent
                        && receipt.controller == controller
                        && receipt.bootstraps.is_empty()),
                "native first configuration has no exact Agent/controller receipt"
            );
            Ok(ledger.onboarding_receipts.get(&key).cloned())
        })
        .await
    }

    /// Required at cold startup, before reading cached inputs. A vanished or
    /// empty store, or an old private-only store, cannot authorize a new send.
    pub(crate) async fn native_resume_required_existing(&self) -> anyhow::Result<()> {
        self.native_transaction(true, |state| {
            let ledger = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?;
            for entry in ledger.entries.values_mut() {
                if entry.state == NativeReplyState::HandoffStarted {
                    entry.state = NativeReplyState::Unknown;
                }
            }
            Ok(())
        })
        .await
    }

    /// Only the verified live event consumer may introduce a new slot. Cached
    /// remote_context replay is not that consumer. The input digest binds the
    /// complete accepted/decrypted input and the fixed reply instructions.
    pub(crate) async fn native_prepare(
        &self,
        slot: NativeReplySlot,
        input_digest: arkret::Hash,
    ) -> anyhow::Result<NativeReplyPreparation> {
        slot.validate()?;
        native_require_sha256(&input_digest)?;
        self.native_transaction(true, move |state| {
            let ledger = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?;
            ledger.require_slot_onboarding(&slot)?;
            let lane = slot.lane()?;
            let first_cutoff = ledger
                .onboarding_receipts
                .values()
                .filter(|receipt| {
                    receipt.agent == slot.agent && receipt.controller == slot.controller
                })
                .filter_map(|receipt| receipt.cutoffs.get(&lane).copied())
                .min()
                .ok_or_else(|| anyhow::anyhow!("native shared bootstrap cutoff is missing"))?;
            if slot.source.stream_position <= first_cutoff {
                return Ok(NativeReplyPreparation::SuppressedBeforeBootstrap);
            }
            let key = slot.key()?;
            if !ledger.entries.contains_key(&key) {
                let water = ledger
                    .high_water
                    .get_mut(&slot.lane()?)
                    .ok_or_else(|| anyhow::anyhow!("native lane is not initialized"))?;
                anyhow::ensure!(
                    slot.source.stream_position > *water,
                    "native source is stale or already consumed"
                );
                *water = slot.source.stream_position;
                ledger.entries.insert(
                    key.clone(),
                    NativeReplyEntry {
                        slot: slot.clone(),
                        input_digest: input_digest.clone(),
                        state: NativeReplyState::Ready,
                        attempt: None,
                        endpoint: None,
                        provider_request_digest: None,
                        output: None,
                        output_digest: None,
                        submission: None,
                        submission_digest: None,
                        accepted: None,
                    },
                );
            }
            let entry = ledger.entry_mut(&slot, &input_digest)?;
            Ok(match entry.state {
                NativeReplyState::Ready => NativeReplyPreparation::Ready,
                NativeReplyState::HandoffStarted | NativeReplyState::Unknown => {
                    NativeReplyPreparation::StartedUnknown
                }
                NativeReplyState::OutputRetained => NativeReplyPreparation::OutputRetained {
                    text: entry
                        .output
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("native output missing"))?,
                    submission: entry.submission.clone(),
                },
                NativeReplyState::Accepted => {
                    let commit = entry
                        .accepted
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("native receipt missing"))?;
                    NativeReplyPreparation::Accepted {
                        receipt: arkret::CommittedEventRef {
                            event_id: commit.event_ref.clone(),
                            commit_id: commit.commit_id.clone(),
                            stream_ref: commit.stream_ref.clone(),
                            stream_position: commit.stream_position,
                        },
                    }
                }
            })
        })
        .await
    }

    /// Called AFTER the complete provider Request is frozen and a native held
    /// current/time guard was acquired. Only a returned capability permits one
    /// physical transport attempt. Recheck that guard after fsync and before
    /// the physical send; failure/cancellation cannot reset this CAS.
    pub(crate) async fn native_claim_handoff(
        &self,
        slot: NativeReplySlot,
        input_digest: arkret::Hash,
        endpoint: String,
        provider_request_digest: arkret::Hash,
    ) -> anyhow::Result<NativeReplyHandoff> {
        slot.validate()?;
        native_endpoint(&endpoint)?;
        native_require_sha256(&input_digest)?;
        native_require_sha256(&provider_request_digest)?;
        self.native_transaction(true, move |state| {
            let entry = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?
                .entry_mut(&slot, &input_digest)?;
            anyhow::ensure!(
                entry.state == NativeReplyState::Ready,
                "native physical handoff is not retryable"
            );
            let attempt = Uuid::now_v7();
            entry.state = NativeReplyState::HandoffStarted;
            entry.attempt = Some(attempt);
            entry.endpoint = Some(endpoint.clone());
            entry.provider_request_digest = Some(provider_request_digest.clone());
            Ok(NativeReplyHandoff {
                key: slot.key()?,
                input_digest,
                attempt,
                endpoint,
                provider_request_digest,
            })
        })
        .await
    }

    /// Error, cancellation, EOF and uncertain physical delivery are terminal
    /// for model invocation, even if no response bytes were observed.
    pub(crate) async fn native_mark_unknown(
        &self,
        handoff: NativeReplyHandoff,
    ) -> anyhow::Result<()> {
        self.native_transaction(true, move |state| {
            let entry = native_handoff_entry(state, &handoff)?;
            anyhow::ensure!(
                matches!(
                    entry.state,
                    NativeReplyState::HandoffStarted | NativeReplyState::Unknown
                ),
                "native output was already retained"
            );
            entry.state = NativeReplyState::Unknown;
            Ok(())
        })
        .await
    }

    /// Called only with a completed, bounded, tools-free response while the
    /// original physical stream/current guard is still held. A late completed
    /// response may be retained for the SAME attempt; it never allows a resend.
    pub(crate) async fn native_retain_output(
        &self,
        handoff: NativeReplyHandoff,
        text: String,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !text.is_empty() && text.len() <= 1024 * 1024,
            "native reply output bound exceeded"
        );
        self.native_transaction(true, move |state| {
            let entry = native_handoff_entry(state, &handoff)?;
            if let Some(retained) = &entry.output {
                anyhow::ensure!(retained == &text, "native reply output changed");
                return Ok(retained.clone());
            }
            anyhow::ensure!(
                matches!(
                    entry.state,
                    NativeReplyState::HandoffStarted | NativeReplyState::Unknown
                ),
                "native handoff was not started"
            );
            entry.output_digest = Some(native_hash(text.as_bytes())?);
            entry.output = Some(text.clone());
            entry.state = NativeReplyState::OutputRetained;
            Ok(text)
        })
        .await
    }

    /// The original MLS authoring owner constructs this exact signed carrier
    /// from the retained output. This ledger binds bytes/target, not encrypted
    /// plaintext equivalence or producer authority. Those remain original
    /// authoring and native admission responsibilities. First signature wins.
    pub(crate) async fn native_retain_submission(
        &self,
        slot: NativeReplySlot,
        input_digest: arkret::Hash,
        output_digest: arkret::Hash,
        submission: garth::FrozenMessageSubmission,
    ) -> anyhow::Result<garth::FrozenMessageSubmission> {
        slot.validate()?;
        native_submission_matches(&slot, &submission)?;
        self.native_transaction(true, move |state| {
            let entry = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?
                .entry_mut(&slot, &input_digest)?;
            anyhow::ensure!(
                entry.output_digest.as_ref() == Some(&output_digest),
                "native submission output changed"
            );
            let digest = native_hash(submission.canonical_submission_bytes())?;
            if let Some(retained) = &entry.submission {
                anyhow::ensure!(
                    entry.submission_digest.as_ref() == Some(&digest)
                        && retained.canonical_submission_bytes()
                            == submission.canonical_submission_bytes(),
                    "native signed submission changed"
                );
                return Ok(retained.clone());
            }
            anyhow::ensure!(
                entry.state == NativeReplyState::OutputRetained,
                "native output was not retained"
            );
            entry.submission_digest = Some(digest);
            entry.submission = Some(submission.clone());
            Ok(submission)
        })
        .await
    }

    /// Accepts a FULL commit already signature-validated by the original
    /// authority receiver. A caller-provided CommittedEventRef is insufficient.
    /// Structural/content-address and exact frozen Event binding are rechecked.
    pub(crate) async fn native_mark_accepted(
        &self,
        slot: NativeReplySlot,
        input_digest: arkret::Hash,
        commit: arkret::RealmCommit,
    ) -> anyhow::Result<()> {
        slot.validate()?;
        commit.validate_content_address()?;
        self.native_transaction(true, move |state| {
            let entry = state
                .native_reply_ledger
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native ledger is not initialized"))?
                .entry_mut(&slot, &input_digest)?;
            let mut frozen = entry
                .submission
                .clone()
                .ok_or_else(|| anyhow::anyhow!("native reply has no retained signed submission"))?;
            frozen.apply_outcome(arkret::AuthoritySubmitOutcome::Accepted {
                status: arkret::AuthorityCommitStatus::Committed,
                commit: commit.clone(),
            })?;
            if let Some(previous) = &entry.accepted {
                anyhow::ensure!(previous == &commit, "native accepted receipt changed");
                return Ok(());
            }
            anyhow::ensure!(
                entry.state == NativeReplyState::OutputRetained,
                "native reply cannot be settled"
            );
            entry.accepted = Some(commit);
            entry.state = NativeReplyState::Accepted;
            Ok(())
        })
        .await
    }

    async fn native_transaction<R: Send + 'static>(
        &self,
        require_existing: bool,
        mutate: impl FnOnce(&mut DeliveryStoreFile) -> anyhow::Result<R> + Send + 'static,
    ) -> anyhow::Result<R> {
        let _process_guard = store_lock().lock().await;
        let path = self.path.clone();
        let home = self.home.clone();
        // This closure owns the separate-inode FileLock through read, CAS,
        // atomic replacement AND directory fsync. Cancelling the async waiter
        // cannot release that lock while a background write is still pending.
        tokio::task::spawn_blocking(move || -> anyhow::Result<R> {
            let directories = native_create_parent(&path, &home)?;
            let mut options = std::fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let file_lock = options.open(path.with_extension("lock"))?;
            file_lock.lock()?;
            let mut state: DeliveryStoreFile = match std::fs::read(&path) {
                Ok(bytes) if !bytes.is_empty() => serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("native delivery store is invalid"))?,
                Ok(_) => anyhow::bail!("native delivery store is empty"),
                Err(error) if !require_existing && error.kind() == std::io::ErrorKind::NotFound => {
                    DeliveryStoreFile::default()
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    anyhow::bail!("native delivery store is missing")
                }
                Err(error) => return Err(error.into()),
            };
            anyhow::ensure!(
                state.version == STORE_VERSION,
                "unsupported native delivery store version"
            );
            if let Some(ledger) = &state.native_reply_ledger {
                ledger.validate()?;
            }
            let result = mutate(&mut state)?;
            if let Some(ledger) = &state.native_reply_ledger {
                ledger.validate()?;
            }
            let bytes = serde_json::to_vec_pretty(&state)
                .map_err(|_| anyhow::anyhow!("native delivery serialization failed"))?;
            savfox_utils::fs::write_atomically(&path, &bytes, Some(0o600))?;
            for directory in directories {
                native_sync_directory(&directory)?;
            }
            // file_lock drops only here, after all durability barriers.
            Ok(result)
        })
        .await?
    }

    pub(crate) async fn ensure_binding(
        &self,
        local_session_id: &str,
        conversation: RemoteConversationKey,
        source_event_id: &str,
        source_sender_did: &str,
        agent_sender_did: &str,
        mode: ArkretDeliveryMode,
    ) -> anyhow::Result<(ArkretExecutionBinding, bool)> {
        conversation.validate()?;
        let request_scoped = mode.request_scoped(&conversation.stream_ref);
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        if let Some(binding) = state.bindings.values_mut().find(|binding| {
            binding.conversation == conversation
                && binding.mode == mode
                && (!request_scoped || binding.source_event_id == source_event_id)
        }) {
            anyhow::ensure!(
                binding.local_session_id == local_session_id,
                "Arkret conversation is already bound to another local execution session"
            );
            binding.last_ingested_event_id = Some(source_event_id.to_owned());
            binding.updated_at = Utc::now();
            let binding = binding.clone();
            self.save_unlocked(&state).await?;
            return Ok((binding, false));
        }
        anyhow::ensure!(
            !state.bindings.values().any(|binding| {
                binding.local_session_id == local_session_id
                    && (binding.conversation != conversation
                        || binding.mode != mode
                        || (request_scoped && binding.source_event_id != source_event_id))
            }),
            "local execution session is already bound to another Arkret delivery target"
        );
        let now = Utc::now();
        let binding = ArkretExecutionBinding {
            binding_id: Uuid::now_v7(),
            local_session_id: local_session_id.to_owned(),
            conversation,
            source_event_id: source_event_id.to_owned(),
            source_sender_did: source_sender_did.to_owned(),
            agent_sender_did: agent_sender_did.to_owned(),
            mode,
            state: DeliveryState::Accepted,
            last_ingested_event_id: Some(source_event_id.to_owned()),
            last_published_checkpoint_id: None,
            last_published_event_id: None,
            public_summary_revision: 0,
            created_at: now,
            updated_at: now,
        };
        state.bindings.insert(binding.binding_id, binding.clone());
        self.save_unlocked(&state).await?;
        Ok((binding, true))
    }

    pub(crate) async fn binding_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<ArkretExecutionBinding>> {
        let (_guard, _file_lock) = self.lock().await?;
        Ok(self
            .load_unlocked()
            .await?
            .bindings
            .into_values()
            .find(|binding| binding.local_session_id == session_id))
    }

    pub(crate) async fn binding_for_conversation(
        &self,
        conversation: &RemoteConversationKey,
    ) -> anyhow::Result<Option<ArkretExecutionBinding>> {
        let (_guard, _file_lock) = self.lock().await?;
        Ok(self
            .load_unlocked()
            .await?
            .bindings
            .into_values()
            .filter(|binding| &binding.conversation == conversation)
            .min_by_key(|binding| binding.created_at))
    }

    /// Preserve the original route, but isolate a newly selected delivery mode
    /// from its private rollout. Only the verified remote snapshot is shared.
    pub(crate) async fn routing_scope_for_mode(
        &self,
        conversation: &RemoteConversationKey,
        mode: ArkretDeliveryMode,
    ) -> anyhow::Result<String> {
        conversation.validate()?;
        let scope = conversation.routing_scope();
        Ok(match self.binding_for_conversation(conversation).await? {
            Some(binding) if binding.mode != mode => format!("{scope}:{}", mode.as_str()),
            _ => scope,
        })
    }

    pub(crate) async fn routing_scope_for_request(
        &self,
        conversation: &RemoteConversationKey,
        mode: ArkretDeliveryMode,
        owner_route: Option<&str>,
        request_id: &str,
    ) -> anyhow::Result<String> {
        let mut scope = self.routing_scope_for_mode(conversation, mode).await?;
        if let Some(owner_route) = owner_route {
            scope.push_str(owner_route);
        }
        if mode.request_scoped(&conversation.stream_ref) {
            scope.push_str(&format!(":request:{request_id}"));
        }
        Ok(scope)
    }

    pub(crate) async fn last_published_rendered(
        &self,
        binding_id: Uuid,
    ) -> anyhow::Result<Option<String>> {
        let (_guard, _file_lock) = self.lock().await?;
        Ok(self
            .load_unlocked()
            .await?
            .outbox
            .into_values()
            .filter(|item| {
                item.checkpoint.binding_id == binding_id
                    && matches!(item.state, DeliveryOutboxState::Published)
            })
            .max_by_key(|item| item.checkpoint.sequence)
            .map(|item| item.rendered_body))
    }

    pub(crate) async fn hydrate_event(
        &self,
        conversation: RemoteConversationKey,
        event: RemoteContextEvent,
    ) -> anyhow::Result<()> {
        conversation.validate()?;
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let snapshot = state.remote_context.entry(conversation).or_default();
        if snapshot
            .events
            .iter()
            .any(|item| item.event_id == event.event_id)
        {
            return Ok(());
        }
        snapshot.events.push(event);
        snapshot.events.sort_by_key(|event| event.received_at);
        if snapshot.events.len() > MAX_REMOTE_CONTEXT_EVENTS {
            let overflow = snapshot.events.len() - MAX_REMOTE_CONTEXT_EVENTS;
            snapshot.events.drain(0..overflow);
            snapshot.truncated = true;
        }
        self.save_unlocked(&state).await
    }

    pub(crate) async fn remote_snapshot(
        &self,
        conversation: &RemoteConversationKey,
    ) -> anyhow::Result<RemoteContextSnapshot> {
        let (_guard, _file_lock) = self.lock().await?;
        Ok(self
            .load_unlocked()
            .await?
            .remote_context
            .remove(conversation)
            .unwrap_or_default())
    }

    pub(crate) async fn mark_history_unavailable(
        &self,
        conversation: RemoteConversationKey,
        reason: &str,
    ) -> anyhow::Result<()> {
        conversation.validate()?;
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let snapshot = state.remote_context.entry(conversation).or_default();
        snapshot.history_unavailable = Some(reason.chars().take(256).collect());
        self.save_unlocked(&state).await
    }

    pub(crate) async fn enqueue_checkpoint(
        &self,
        binding_id: Uuid,
        kind: DeliveryCheckpointKind,
        title: String,
        summary: String,
        verification: Vec<String>,
        blockers: Vec<String>,
        next_actions: Vec<String>,
        initiated_by: &str,
    ) -> anyhow::Result<(DeliveryCheckpoint, String)> {
        let summary = sanitize_public_text(&summary)?;
        let mut verification = sanitize_public_list(verification)?;
        if kind == DeliveryCheckpointKind::Completed && verification.is_empty() {
            verification.push(
                "Not independently verified; no verification evidence was provided.".to_owned(),
            );
        }
        let blockers = sanitize_public_list(blockers)?;
        let next_actions = sanitize_public_list(next_actions)?;
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let binding = state
            .bindings
            .get_mut(&binding_id)
            .context("Arkret execution binding not found")?;
        anyhow::ensure!(
            !matches!(
                binding.state,
                DeliveryState::Completed | DeliveryState::Failed | DeliveryState::Cancelled
            ) || matches!(
                kind,
                DeliveryCheckpointKind::Failed | DeliveryCheckpointKind::Cancelled
            ),
            "cannot publish a new checkpoint after the binding reached a terminal state"
        );
        let sequence = state
            .outbox
            .values()
            .filter(|item| item.checkpoint.binding_id == binding_id)
            .map(|item| item.checkpoint.sequence)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let checkpoint = DeliveryCheckpoint {
            checkpoint_id: Uuid::now_v7(),
            binding_id,
            sequence,
            kind,
            title: sanitize_public_text(&title)?,
            summary,
            artifacts: Vec::new(),
            verification,
            blockers,
            next_actions,
            source_revision: binding.public_summary_revision.saturating_add(1),
            visibility: DeliveryVisibility::RemotePublic,
            created_at: Utc::now(),
        };
        let rendered = render_checkpoint(&checkpoint);
        anyhow::ensure!(
            !state.outbox.values().any(|item| {
                item.checkpoint.binding_id == binding_id && item.rendered_body == rendered
            }),
            "checkpoint has no material change from an existing delivery"
        );
        binding.state = kind.state();
        binding.public_summary_revision = checkpoint.source_revision;
        binding.updated_at = Utc::now();
        state.outbox.insert(
            checkpoint.checkpoint_id,
            DeliveryOutboxItem {
                checkpoint: checkpoint.clone(),
                rendered_body: rendered.clone(),
                state: DeliveryOutboxState::Pending,
                attempts: 0,
                frozen_submission: None,
                remote_event_id: None,
                last_error: None,
                initiated_by: initiated_by.to_owned(),
                next_attempt_at: None,
                updated_at: Utc::now(),
            },
        );
        self.save_unlocked(&state).await?;
        Ok((checkpoint, rendered))
    }

    pub(crate) async fn checkpoint_submission(
        &self,
        checkpoint_id: Uuid,
        conversation: &RemoteConversationKey,
        rendered_body: &str,
    ) -> anyhow::Result<Option<garth::FrozenMessageSubmission>> {
        let (_guard, _file_lock) = self.lock().await?;
        let state = self.load_unlocked().await?;
        let item = Self::bound_checkpoint(&state, checkpoint_id, conversation, rendered_body)?;
        if let Some(submission) = &item.frozen_submission {
            Self::validate_submission_target(submission, conversation)?;
        }
        Ok(item.frozen_submission.clone())
    }

    /// The first signed submission wins durably, including concurrent calls.
    /// Later attempts replay those exact bytes instead of re-encrypting or
    /// assigning a fresh created_at and content-derived Event identity.
    pub(crate) async fn retain_checkpoint_submission(
        &self,
        checkpoint_id: Uuid,
        conversation: &RemoteConversationKey,
        rendered_body: &str,
        submission: garth::FrozenMessageSubmission,
    ) -> anyhow::Result<garth::FrozenMessageSubmission> {
        Self::validate_submission_target(&submission, conversation)?;
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let item = Self::bound_checkpoint(&state, checkpoint_id, conversation, rendered_body)?;
        if let Some(retained) = &item.frozen_submission {
            Self::validate_submission_target(retained, conversation)?;
            return Ok(retained.clone());
        }
        state
            .outbox
            .get_mut(&checkpoint_id)
            .expect("bound checkpoint checked")
            .frozen_submission = Some(submission.clone());
        self.save_unlocked(&state).await?;
        Ok(submission)
    }

    fn validate_submission_target(
        submission: &garth::FrozenMessageSubmission,
        conversation: &RemoteConversationKey,
    ) -> anyhow::Result<()> {
        let event = &submission.request().submission.event;
        anyhow::ensure!(
            arkret::CommitStreamRef::from_scope(&event.scope_ref, None)? == conversation.stream_ref
                && event
                    .payload
                    .get("strand_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(conversation.strand_id.as_str()),
            "frozen checkpoint submission does not match its delivery target"
        );
        Ok(())
    }

    fn bound_checkpoint<'a>(
        state: &'a DeliveryStoreFile,
        checkpoint_id: Uuid,
        conversation: &RemoteConversationKey,
        rendered_body: &str,
    ) -> anyhow::Result<&'a DeliveryOutboxItem> {
        let item = state
            .outbox
            .get(&checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint is not in the durable delivery outbox"))?;
        let binding = state
            .bindings
            .get(&item.checkpoint.binding_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint has no durable execution binding"))?;
        anyhow::ensure!(
            &binding.conversation == conversation && item.rendered_body == rendered_body,
            "checkpoint replay differs from its bound account, conversation, or public body"
        );
        Ok(item)
    }

    pub(crate) async fn mark_published(
        &self,
        checkpoint_id: Uuid,
        remote_event_id: &str,
    ) -> anyhow::Result<()> {
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let binding_id = {
            let item = state
                .outbox
                .get_mut(&checkpoint_id)
                .context("Arkret delivery checkpoint not found")?;
            item.state = DeliveryOutboxState::Published;
            item.remote_event_id = Some(remote_event_id.to_owned());
            item.last_error = None;
            item.next_attempt_at = None;
            item.updated_at = Utc::now();
            item.checkpoint.binding_id
        };
        if let Some(binding) = state.bindings.get_mut(&binding_id) {
            binding.last_published_checkpoint_id = Some(checkpoint_id);
            binding.last_published_event_id = Some(remote_event_id.to_owned());
            binding.updated_at = Utc::now();
        }
        self.save_unlocked(&state).await
    }

    pub(crate) async fn mark_retryable(
        &self,
        checkpoint_id: Uuid,
        error: &str,
    ) -> anyhow::Result<()> {
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let item = state
            .outbox
            .get_mut(&checkpoint_id)
            .context("Arkret delivery checkpoint not found")?;
        item.state = DeliveryOutboxState::Retryable;
        item.attempts = item.attempts.saturating_add(1);
        item.last_error = Some(error.chars().take(512).collect());
        item.updated_at = Utc::now();
        let delay_seconds = 2_i64.saturating_pow(item.attempts.min(8)).clamp(2, 300);
        item.next_attempt_at = Some(item.updated_at + chrono::Duration::seconds(delay_seconds));
        self.save_unlocked(&state).await
    }

    async fn pending_for_account(
        &self,
        channel_config_id: &str,
        account_id: &str,
    ) -> anyhow::Result<Vec<PendingDelivery>> {
        let (_guard, _file_lock) = self.lock().await?;
        let state = self.load_unlocked().await?;
        let now = Utc::now();
        let mut pending = state
            .outbox
            .values()
            .filter(|item| {
                matches!(
                    item.state,
                    DeliveryOutboxState::Pending | DeliveryOutboxState::Retryable
                ) && item.next_attempt_at.is_none_or(|at| at <= now)
            })
            .filter_map(|item| {
                let binding = state.bindings.get(&item.checkpoint.binding_id)?;
                (binding.conversation.channel_config_id == channel_config_id
                    && binding.conversation.account_id == account_id)
                    .then(|| PendingDelivery {
                        binding: binding.clone(),
                        checkpoint: item.checkpoint.clone(),
                        rendered_body: item.rendered_body.clone(),
                        initiated_by: item.initiated_by.clone(),
                    })
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|item| item.checkpoint.sequence);
        Ok(pending)
    }

    pub(crate) async fn acknowledge_echo(&self, remote_event_id: &str) -> anyhow::Result<bool> {
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        let Some(checkpoint_id) = state.outbox.iter().find_map(|(id, item)| {
            (item.remote_event_id.as_deref() == Some(remote_event_id)).then_some(*id)
        }) else {
            return Ok(false);
        };
        if let Some(item) = state.outbox.get_mut(&checkpoint_id) {
            item.state = DeliveryOutboxState::Published;
            item.updated_at = Utc::now();
        }
        self.save_unlocked(&state).await?;
        Ok(true)
    }

    pub(crate) async fn diagnostics_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let (_guard, _file_lock) = self.lock().await?;
        let state = self.load_unlocked().await?;
        let Some(binding) = state
            .bindings
            .values()
            .find(|binding| binding.local_session_id == session_id)
        else {
            return Ok(None);
        };
        let pending = state
            .outbox
            .values()
            .filter(|item| {
                item.checkpoint.binding_id == binding.binding_id
                    && matches!(
                        item.state,
                        DeliveryOutboxState::Pending | DeliveryOutboxState::Retryable
                    )
            })
            .count();
        let published = state
            .outbox
            .values()
            .filter(|item| {
                item.checkpoint.binding_id == binding.binding_id
                    && matches!(item.state, DeliveryOutboxState::Published)
            })
            .count();
        let snapshot = state
            .remote_context
            .get(&binding.conversation)
            .cloned()
            .unwrap_or_default();
        let last_retry = state
            .outbox
            .values()
            .filter(|item| item.checkpoint.binding_id == binding.binding_id)
            .filter_map(|item| {
                item.last_error
                    .as_ref()
                    .map(|error| (item.updated_at, error, item.next_attempt_at))
            })
            .max_by_key(|(updated_at, ..)| *updated_at);
        Ok(Some(serde_json::json!({
            "bindingId": binding.binding_id,
            "mode": binding.mode,
            "state": binding.state,
            "conversation": binding.conversation,
            "sourceSenderDid": binding.source_sender_did,
            "agentSenderDid": binding.agent_sender_did,
            "lastIngestedEventId": binding.last_ingested_event_id,
            "lastPublishedCheckpointId": binding.last_published_checkpoint_id,
            "lastPublishedEventId": binding.last_published_event_id,
            "pendingOutbox": pending,
            "publishedCheckpoints": published,
            "lastOutboxError": last_retry.as_ref().map(|(_, error, _)| *error),
            "nextOutboxRetryAt": last_retry.and_then(|(_, _, next_attempt_at)| next_attempt_at),
            "hydratedEvents": snapshot.events.len(),
            "hydrationTruncated": snapshot.truncated,
            "historyUnavailable": snapshot.history_unavailable,
        })))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeliveryCorrelation {
    pub checkpoint_id: Uuid,
    pub sequence: u64,
    pub source_event_id: String,
    pub initiated_by: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishCheckpointRequest {
    pub kind: DeliveryCheckpointKind,
    pub title: String,
    pub summary: String,
    pub verification: Vec<String>,
    pub blockers: Vec<String>,
    pub next_actions: Vec<String>,
    pub initiated_by: &'static str,
}

pub(crate) async fn publish_checkpoint(
    savfox_home: &Path,
    binding: &ArkretExecutionBinding,
    request: PublishCheckpointRequest,
) -> anyhow::Result<(DeliveryCheckpoint, String, String)> {
    anyhow::ensure!(
        binding.mode == ArkretDeliveryMode::TaskDelivery,
        "Arkret checkpoint publishing requires task_delivery mode"
    );
    anyhow::ensure!(
        matches!(
            binding.conversation.stream_ref,
            arkret::CommitStreamRef::Realm { .. }
        ),
        "automatic task checkpoints require an ordinary Realm delivery target"
    );
    let store = ArkretExecutionBindingStore::new(savfox_home);
    let (checkpoint, rendered) = store
        .enqueue_checkpoint(
            binding.binding_id,
            request.kind,
            request.title,
            request.summary,
            request.verification,
            request.blockers,
            request.next_actions,
            request.initiated_by,
        )
        .await?;
    let correlation = DeliveryCorrelation {
        checkpoint_id: checkpoint.checkpoint_id,
        sequence: checkpoint.sequence,
        source_event_id: binding.source_event_id.clone(),
        initiated_by: request.initiated_by.to_owned(),
    };
    match crate::channels::arkret::send_to_arkret_account(
        &savfox_home.to_path_buf(),
        &binding.conversation.realm_id,
        Some(&binding.conversation.strand_id),
        &rendered,
        None,
        Some(&binding.conversation.channel_config_id),
        Some(&binding.conversation.account_id),
        Some(&correlation),
    )
    .await
    {
        Ok(remote_event_id) => {
            store
                .mark_published(checkpoint.checkpoint_id, &remote_event_id)
                .await?;
            Ok((checkpoint, rendered, remote_event_id))
        }
        Err(error) => {
            store
                .mark_retryable(checkpoint.checkpoint_id, &error.to_string())
                .await?;
            Err(error)
        }
    }
}

/// Replay due checkpoint deliveries after the bound Arkret account has
/// recovered its session and MLS state. The retained frozen SDK submission
/// makes a crash after remote acceptance safe: the replay has the same Event
/// identity and bytes, independent of the retry time or MLS sender generation.
pub(crate) async fn resume_pending_checkpoints(
    savfox_home: &Path,
    channel_config_id: &str,
    account_id: &str,
) -> anyhow::Result<usize> {
    let store = ArkretExecutionBindingStore::new(savfox_home);
    let pending = store
        .pending_for_account(channel_config_id, account_id)
        .await?;
    let mut published = 0;
    for item in pending {
        if !matches!(
            item.binding.conversation.stream_ref,
            arkret::CommitStreamRef::Realm { .. }
        ) {
            store
                .mark_retryable(
                    item.checkpoint.checkpoint_id,
                    "automatic task checkpoints require an ordinary Realm delivery target",
                )
                .await?;
            continue;
        }
        let correlation = DeliveryCorrelation {
            checkpoint_id: item.checkpoint.checkpoint_id,
            sequence: item.checkpoint.sequence,
            source_event_id: item.binding.source_event_id.clone(),
            initiated_by: item.initiated_by,
        };
        match crate::channels::arkret::send_to_arkret_account(
            &savfox_home.to_path_buf(),
            &item.binding.conversation.realm_id,
            Some(&item.binding.conversation.strand_id),
            &item.rendered_body,
            None,
            Some(&item.binding.conversation.channel_config_id),
            Some(&item.binding.conversation.account_id),
            Some(&correlation),
        )
        .await
        {
            Ok(remote_event_id) => {
                store
                    .mark_published(item.checkpoint.checkpoint_id, &remote_event_id)
                    .await?;
                published += 1;
            }
            Err(error) => {
                store
                    .mark_retryable(item.checkpoint.checkpoint_id, &error.to_string())
                    .await?;
                warn!(
                    binding_id = %item.binding.binding_id,
                    checkpoint_id = %item.checkpoint.checkpoint_id,
                    "arkret: pending delivery replay remains retryable: {error:#}"
                );
            }
        }
    }
    Ok(published)
}

#[must_use]
pub(crate) fn render_checkpoint(checkpoint: &DeliveryCheckpoint) -> String {
    let mut lines = vec![format!("[Status] {}", checkpoint.kind.label())];
    if !checkpoint.title.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("Result: {}", checkpoint.title.trim()));
    }
    if !checkpoint.summary.trim().is_empty() {
        lines.push(format!("Delivery: {}", checkpoint.summary.trim()));
    }
    if !checkpoint.verification.is_empty() {
        lines.push(format!(
            "Verification: {}",
            checkpoint.verification.join("; ")
        ));
    }
    if !checkpoint.blockers.is_empty() {
        lines.push(format!("Blockers: {}", checkpoint.blockers.join("; ")));
    }
    if !checkpoint.next_actions.is_empty() {
        lines.push(format!("Next: {}", checkpoint.next_actions.join("; ")));
    }
    lines.join("\n")
}

pub(crate) fn sanitize_public_text(input: &str) -> anyhow::Result<String> {
    let mut output = Vec::new();
    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let contains_secret_shape = [
            "authorization:",
            "bearer ",
            "access_token",
            "refresh_token",
            "session_grant",
            "private_key",
            "api_key",
            "password=",
            "secret=",
            "-----begin private key",
            "xoxb-",
            "ghp_",
            "akia",
            "chain-of-thought",
            "tool stdout",
            "tool stderr",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
            || lower
                .split(|character: char| character.is_whitespace() || character == '`')
                .any(|word| word.starts_with("sk-") && word.len() >= 12);
        if contains_secret_shape {
            output.push("[sensitive detail redacted]".to_owned());
            continue;
        }
        let contains_local_path = lower.contains("/users/")
            || lower.contains("/home/")
            || lower.contains("\\users\\")
            || line.as_bytes().windows(3).any(|window| {
                window[0].is_ascii_alphabetic()
                    && window[1] == b':'
                    && matches!(window[2], b'\\' | b'/')
            });
        if contains_local_path {
            output.push("[local path redacted]".to_owned());
            continue;
        }
        output.push(line.to_owned());
    }
    let compact = output.join(" ");
    anyhow::ensure!(
        !compact.is_empty(),
        "checkpoint contains no publishable content"
    );
    Ok(compact.chars().take(MAX_PUBLIC_SUMMARY_CHARS).collect())
}

pub(crate) fn sanitize_public_list(values: Vec<String>) -> anyhow::Result<Vec<String>> {
    values
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .map(|value| sanitize_public_text(&value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(strand: &str) -> RemoteConversationKey {
        RemoteConversationKey {
            channel_config_id: "support".to_owned(),
            account_id: "agent-account".to_owned(),
            realm_id: canonical_conversation().realm_id,
            strand_id: strand.to_owned(),
            stream_ref: canonical_conversation().stream_ref,
        }
    }

    fn canonical_conversation() -> RemoteConversationKey {
        let event = arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [17; 32]);
        RemoteConversationKey {
            channel_config_id: "support".to_owned(),
            account_id: "agent-account".to_owned(),
            realm_id: arkret::RealmId::from_event_id(&event).to_string(),
            strand_id: arkret::StrandId::from_event_id(&event).to_string(),
            stream_ref: arkret::CommitStreamRef::Realm {
                realm_id: arkret::RealmId::from_event_id(&event),
            },
        }
    }

    fn frozen_submission(
        target: &RemoteConversationKey,
        body: &str,
        seconds: i64,
    ) -> garth::FrozenMessageSubmission {
        let payload = arkret::MessageCreatePayload::with_content(
            arkret::StrandId::new(target.strand_id.clone()).unwrap(),
            "discussion",
            arkret::ContentBlock::text(body),
        );
        let actor = arkret::ActorId::account(arkret::AccountId::new(
            arkret::DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
            arkret::DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        ));
        let event = arkret::TypedEventDraft::<arkret::event_spec::MessageCreate>::new(
            arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new(target.realm_id.clone()).unwrap(),
            },
            actor,
            payload,
        )
        .unwrap()
        .author_with_digest_suite(
            DateTime::from_timestamp(seconds, 0).unwrap(),
            arkret::canonical::DigestSuite::Sha256,
        )
        .unwrap();
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
            ed25519_dalek::SigningKey::from_bytes(&[71; 32]),
            "did:web:agent.example#runtime-1",
        );
        garth::MessageAuthoringSession::from_authored_event(event)
            .unwrap()
            .sign(&signer, arkret::signatures::SignEventOptions::new())
            .unwrap()
    }

    fn native_slot(position: u64) -> NativeReplySlot {
        let conversation = canonical_conversation();
        let agent = arkret::AccountId::new(
            arkret::DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
            arkret::DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        );
        let controller = arkret::AccountId::new(
            arkret::DidCoreId::new("ak:did_core:web:controller.example").unwrap(),
            agent.station_id.clone(),
        );
        NativeReplySlot {
            source: arkret::CommittedEventRef {
                event_id: arkret::EventId::from_digest(
                    arkret::canonical::DigestSuite::Sha256,
                    [34; 32],
                ),
                commit_id: arkret::RealmCommitId::from_digest([35; 32]),
                stream_ref: conversation.stream_ref.clone(),
                stream_position: position,
            },
            scope: arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new(conversation.realm_id.clone()).unwrap(),
            },
            strand_id: arkret::StrandId::new(conversation.strand_id.clone()).unwrap(),
            conversation,
            agent,
            controller: controller.clone(),
            caller: controller,
        }
    }

    async fn initialize_native_test_config(
        store: &ArkretExecutionBindingStore,
    ) -> NativeReplyOnboardingReceipt {
        let slot = native_slot(1);
        store
            .native_initialize_first_config(
                slot.agent,
                slot.controller,
                native_hash(b"original accepted pairing configuration").unwrap(),
            )
            .await
            .unwrap()
    }

    async fn prepared_native_store(
        home: &Path,
    ) -> (ArkretExecutionBindingStore, NativeReplySlot, arkret::Hash) {
        let store = ArkretExecutionBindingStore::new(home);
        let slot = native_slot(2);
        initialize_native_test_config(&store).await;
        store
            .native_initialize_batch(
                slot.agent.clone(),
                slot.controller.clone(),
                native_hash(b"original shared configuration").unwrap(),
                vec![native_slot(1).source],
            )
            .await
            .unwrap();
        let input = native_hash(b"complete original input").unwrap();
        assert!(matches!(
            store
                .native_prepare(slot.clone(), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::Ready
        ));
        (store, slot, input)
    }

    // These are local ledger crash/CAS tests, not accepted-history authority or
    // a live provider/process-kill proof. The source references are test inputs;
    // signed output uses the existing real SDK Message authoring helper.
    #[tokio::test]
    async fn native_ledger_resume_refuses_missing_empty_and_private_only_store() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        assert!(store.native_resume_required_existing().await.is_err());
        std::fs::write(&store.path, []).unwrap();
        assert!(store.native_resume_required_existing().await.is_err());
        std::fs::remove_file(&store.path).unwrap();
        store
            .ensure_binding(
                "private",
                canonical_conversation(),
                "source",
                "sender",
                "agent",
                ArkretDeliveryMode::InteractiveChat,
            )
            .await
            .unwrap();
        assert!(store.native_resume_required_existing().await.is_err());
        initialize_native_test_config(&store).await;
        assert!(
            store
                .native_initialize_first_config(
                    native_slot(1).agent,
                    native_slot(1).controller,
                    native_hash(b"replacement first intent").unwrap()
                )
                .await
                .is_err()
        );
        store.native_resume_required_existing().await.unwrap();
    }

    #[tokio::test]
    async fn native_ledger_bootstrap_rejects_cached_source_and_input_rebinding() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let before = std::fs::read(&store.path).unwrap();
        assert!(matches!(
            store
                .native_prepare(native_slot(1), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::SuppressedBeforeBootstrap
        ));
        assert!(std::fs::read(&store.path).unwrap() == before);
        assert!(
            store
                .native_prepare(slot.clone(), native_hash(b"changed input").unwrap())
                .await
                .is_err()
        );
        let mut rebound = slot.clone();
        rebound.caller = slot.agent.clone();
        assert!(store.native_prepare(rebound, input.clone()).await.is_err());
        let mut cross_target = slot.clone();
        cross_target.scope = arkret::ScopeRef::Realm {
            realm_id: arkret::RealmId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [98; 32],
            )),
        };
        assert!(store.native_prepare(cross_target, input).await.is_err());
    }

    fn native_other_bootstrap() -> arkret::CommittedEventRef {
        let mut bootstrap = native_slot(1).source;
        bootstrap.stream_ref = arkret::CommitStreamRef::Realm {
            realm_id: arkret::RealmId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [96; 32],
            )),
        };
        bootstrap
    }

    #[tokio::test]
    async fn native_ledger_onboarding_batch_is_atomic_and_retry_returns_original_receipt() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        let agent = native_slot(1).agent;
        let digest = native_hash(b"complete explicit validated config").unwrap();
        initialize_native_test_config(&store).await;
        let first = store
            .native_initialize_batch(
                agent.clone(),
                native_slot(1).controller,
                digest.clone(),
                vec![native_slot(1).source, native_other_bootstrap()],
            )
            .await
            .unwrap();
        store
            .native_prepare(native_slot(2), native_hash(b"new actual input").unwrap())
            .await
            .unwrap();
        let mut later = native_slot(9).source;
        later.event_id =
            arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [97; 32]);
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        let retry = cold
            .native_initialize_batch(
                agent,
                native_slot(1).controller,
                digest,
                vec![later, native_other_bootstrap()],
            )
            .await
            .unwrap();
        assert!(retry == first);
        assert!(
            cold.native_onboarding_receipt(
                first.agent.clone(),
                first.controller.clone(),
                first.onboarding_digest.clone()
            )
            .await
            .unwrap()
                == Some(first)
        );
        assert!(matches!(
            cold.native_prepare(native_slot(1), native_hash(b"cached input").unwrap())
                .await
                .unwrap(),
            NativeReplyPreparation::SuppressedBeforeBootstrap
        ));
        let state = cold.load_unlocked().await.unwrap();
        let ledger = state.native_reply_ledger.unwrap();
        assert_eq!(ledger.high_water.len(), 2);
        assert_eq!(ledger.onboarding_receipts.len(), 2);
        assert_eq!(
            ledger.high_water.get(&native_slot(2).lane().unwrap()),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn native_ledger_onboarding_adds_new_lanes_without_resetting_watermarks() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, _) = prepared_native_store(home.path()).await;
        let receipt = store
            .native_initialize_batch(
                slot.agent.clone(),
                slot.controller.clone(),
                native_hash(b"explicit added scope").unwrap(),
                vec![native_slot(1).source, native_other_bootstrap()],
            )
            .await
            .unwrap();
        assert_eq!(receipt.bootstraps.len(), 2);
        let before = std::fs::read(&store.path).unwrap();
        // A new explicit configuration can observe a later head for an
        // existing lane. It MUST NOT skip unconsumed events in that lane.
        let mut third = native_other_bootstrap();
        third.stream_ref = arkret::CommitStreamRef::Realm {
            realm_id: arkret::RealmId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [95; 32],
            )),
        };
        store
            .native_initialize_batch(
                slot.agent.clone(),
                slot.controller.clone(),
                native_hash(b"next explicit configuration").unwrap(),
                vec![third, native_slot(9).source],
            )
            .await
            .unwrap();
        assert!(std::fs::read(&store.path).unwrap() != before);
        let state = store.load_unlocked().await.unwrap();
        assert_eq!(
            state
                .native_reply_ledger
                .unwrap()
                .high_water
                .get(&slot.lane().unwrap()),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn native_ledger_onboarding_duplicate_stream_and_rebinding_fail_without_partial_write() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        let agent = native_slot(1).agent;
        let digest = native_hash(b"explicit config").unwrap();
        assert!(
            store
                .native_initialize_batch(
                    agent.clone(),
                    native_slot(1).controller,
                    digest.clone(),
                    vec![native_slot(1).source, native_slot(2).source]
                )
                .await
                .is_err()
        );
        assert!(!store.path.exists());
        assert!(store.native_resume_required_existing().await.is_err());
        initialize_native_test_config(&store).await;
        store
            .native_initialize_batch(
                agent.clone(),
                native_slot(1).controller,
                digest,
                vec![native_slot(1).source],
            )
            .await
            .unwrap();
        let before = std::fs::read(&store.path).unwrap();
        let mut invalid = native_other_bootstrap();
        invalid.stream_ref = arkret::CommitStreamRef::Sidecar {
            realm_id: native_slot(1).source.stream_ref.realm_id().clone(),
            sidecar_id: arkret::SidecarId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [94; 32],
            )),
        };
        assert!(
            store
                .native_initialize_batch(
                    agent,
                    native_slot(1).controller,
                    native_hash(b"changed config").unwrap(),
                    vec![native_other_bootstrap(), invalid]
                )
                .await
                .is_err()
        );
        assert!(std::fs::read(&store.path).unwrap() == before);
        let failed: anyhow::Result<()> = store
            .native_transaction(true, |state| {
                state
                    .native_reply_ledger
                    .as_mut()
                    .unwrap()
                    .high_water
                    .insert("uncommitted lane".to_owned(), 17);
                anyhow::bail!("simulate failure after first in-memory lane mutation")
            })
            .await;
        assert!(failed.is_err());
        assert!(std::fs::read(&store.path).unwrap() == before);
    }

    #[tokio::test]
    async fn native_ledger_first_empty_receipt_never_grants_model_ready() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        let receipt = initialize_native_test_config(&store).await;
        assert!(receipt.bootstraps.is_empty() && receipt.cutoffs.is_empty());
        let before = std::fs::read(&store.path).unwrap();
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        assert!(
            cold.native_prepare(native_slot(2), native_hash(b"actual input").unwrap())
                .await
                .is_err()
        );
        assert!(std::fs::read(&store.path).unwrap() == before);
        let ledger = cold
            .load_unlocked()
            .await
            .unwrap()
            .native_reply_ledger
            .unwrap();
        assert!(ledger.high_water.is_empty() && ledger.entries.is_empty());
    }

    #[tokio::test]
    async fn native_ledger_first_config_exact_retry_and_cold_missing_remain_distinct() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        let first = initialize_native_test_config(&store).await;
        let before = std::fs::read(&store.path).unwrap();
        let cold = ArkretExecutionBindingStore::new(home.path());
        let retry = cold
            .native_initialize_first_config(
                first.agent.clone(),
                first.controller.clone(),
                first.onboarding_digest.clone(),
            )
            .await
            .unwrap();
        assert!(retry == first);
        assert!(
            cold.native_first_configuration_receipt(first.agent.clone(), first.controller.clone())
                .await
                .unwrap()
                == first
        );
        assert!(std::fs::read(&store.path).unwrap() == before);
        std::fs::remove_file(&store.path).unwrap();
        assert!(
            cold.native_first_configuration_receipt(first.agent.clone(), first.controller.clone())
                .await
                .is_err()
        );
        assert!(
            cold.native_onboarding_receipt(
                first.agent.clone(),
                first.controller.clone(),
                first.onboarding_digest.clone()
            )
            .await
            .is_err()
        );
        assert!(
            cold.native_initialize_batch(
                first.agent,
                first.controller,
                native_hash(b"explicit new shared scope").unwrap(),
                vec![native_slot(1).source]
            )
            .await
            .is_err()
        );
        assert!(
            cold.native_prepare(native_slot(2), native_hash(b"cached input").unwrap())
                .await
                .is_err()
        );
        assert!(!store.path.exists());
    }

    #[tokio::test]
    async fn native_ledger_controller_rebinding_cannot_initialize_or_resume_a_lane() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let before = std::fs::read(&store.path).unwrap();
        let changed = slot.agent.clone();
        assert!(
            store
                .native_initialize_first_config(
                    slot.agent.clone(),
                    changed.clone(),
                    native_hash(b"changed owner").unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            store
                .native_initialize_batch(
                    slot.agent.clone(),
                    changed.clone(),
                    native_hash(b"changed shared owner").unwrap(),
                    vec![native_other_bootstrap()]
                )
                .await
                .is_err()
        );
        let mut rebound = slot;
        rebound.controller = changed;
        assert!(store.native_prepare(rebound, input).await.is_err());
        assert!(std::fs::read(&store.path).unwrap() == before);
    }

    #[tokio::test]
    async fn native_ledger_only_prebootstrap_sources_are_suppressed_and_never_issued_handoffs() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        initialize_native_test_config(&store).await;
        let digest = native_hash(b"original shared scope").unwrap();
        store
            .native_initialize_batch(
                native_slot(1).agent,
                native_slot(1).controller,
                digest,
                vec![native_slot(3).source],
            )
            .await
            .unwrap();
        let input = native_hash(b"actual live input").unwrap();
        let before = std::fs::read(&store.path).unwrap();
        assert!(matches!(
            store
                .native_prepare(native_slot(2), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::SuppressedBeforeBootstrap
        ));
        assert!(
            store
                .native_claim_handoff(
                    native_slot(2),
                    input.clone(),
                    "https://provider.example/v1/responses".into(),
                    native_hash(b"frozen request").unwrap()
                )
                .await
                .is_err()
        );
        assert!(std::fs::read(&store.path).unwrap() == before);
        let mut missing_lane = native_slot(2);
        missing_lane.source = native_other_bootstrap();
        let missing_realm = missing_lane.source.stream_ref.realm_id().clone();
        missing_lane.scope = arkret::ScopeRef::Realm {
            realm_id: missing_realm.clone(),
        };
        missing_lane.conversation.realm_id = missing_realm.to_string();
        missing_lane.conversation.stream_ref = missing_lane.source.stream_ref.clone();
        missing_lane.validate().unwrap();
        // A structurally exact but uninitialized lane is still an error.
        // These local typed test refs do NOT claim accepted-history proof.
        assert!(
            store
                .native_prepare(missing_lane, input.clone())
                .await
                .is_err()
        );
        assert!(matches!(
            store
                .native_prepare(native_slot(4), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::Ready
        ));
        let handoff = store
            .native_claim_handoff(
                native_slot(4),
                input.clone(),
                "https://provider.example/v1/responses".into(),
                native_hash(b"frozen request").unwrap(),
            )
            .await
            .unwrap();
        drop(handoff);
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        assert!(matches!(
            cold.native_prepare(native_slot(4), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::StartedUnknown
        ));
        assert!(
            cold.native_claim_handoff(
                native_slot(4),
                input,
                "https://provider.example/v1/responses".into(),
                native_hash(b"frozen request").unwrap()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn native_ledger_cas_and_cold_crash_never_repeat_physical_send() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let endpoint = "https://provider.example/v1/responses".to_owned();
        let body = native_hash(b"exact frozen provider request").unwrap();
        let second = ArkretExecutionBindingStore::new(home.path());
        let (one, two) = tokio::join!(
            store.native_claim_handoff(slot.clone(), input.clone(), endpoint.clone(), body.clone()),
            second.native_claim_handoff(
                slot.clone(),
                input.clone(),
                endpoint.clone(),
                body.clone()
            ),
        );
        assert!(one.is_ok() ^ two.is_ok());
        // Lost capability models a crash/unknown after CAS, even before send.
        drop(one);
        drop(two);
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        assert!(matches!(
            cold.native_prepare(slot.clone(), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::StartedUnknown
        ));
        assert!(
            cold.native_claim_handoff(slot, input, endpoint, body)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn native_ledger_output_and_original_signed_submission_survive_cold_reopen() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let handoff = store
            .native_claim_handoff(
                slot.clone(),
                input.clone(),
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap(),
            )
            .await
            .unwrap();
        let output = store
            .native_retain_output(handoff, "one response".to_owned())
            .await
            .unwrap();
        let output_digest = native_hash(output.as_bytes()).unwrap();
        let submission = frozen_submission(&slot.conversation, &output, 1_735_689_600);
        let expected = submission.canonical_submission_bytes().to_vec();
        store
            .native_retain_submission(
                slot.clone(),
                input.clone(),
                output_digest.clone(),
                submission.clone(),
            )
            .await
            .unwrap();
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        match cold
            .native_prepare(slot.clone(), input.clone())
            .await
            .unwrap()
        {
            NativeReplyPreparation::OutputRetained {
                text,
                submission: Some(retained),
            } => {
                assert!(text == output);
                assert!(retained.canonical_submission_bytes() == expected);
            }
            _ => panic!("cold reply state mismatch"),
        }
        // A new timestamp/producer signature cannot replace a lost final outcome.
        let changed = frozen_submission(&slot.conversation, &output, 1_735_689_601);
        assert!(
            cold.native_retain_submission(
                slot.clone(),
                input.clone(),
                output_digest.clone(),
                changed
            )
            .await
            .is_err()
        );
        assert!(
            cold.native_retain_submission(
                slot.clone(),
                input.clone(),
                native_hash(b"other response").unwrap(),
                submission
            )
            .await
            .is_err()
        );
        assert!(
            cold.native_claim_handoff(
                slot,
                input,
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn native_ledger_corrupted_output_is_not_a_resume_authority() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let handoff = store
            .native_claim_handoff(
                slot.clone(),
                input,
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap(),
            )
            .await
            .unwrap();
        store
            .native_retain_output(handoff, "original response".to_owned())
            .await
            .unwrap();
        let mut state: DeliveryStoreFile =
            serde_json::from_slice(&std::fs::read(&store.path).unwrap()).unwrap();
        state
            .native_reply_ledger
            .as_mut()
            .unwrap()
            .entries
            .get_mut(&slot.key().unwrap())
            .unwrap()
            .output = Some("tampered response".to_owned());
        std::fs::write(&store.path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(store.native_resume_required_existing().await.is_err());
    }

    #[tokio::test]
    async fn native_ledger_cancelled_writer_keeps_file_lock_through_private_interleave() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let handoff = store
            .native_claim_handoff(
                slot.clone(),
                input.clone(),
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap(),
            )
            .await
            .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            writer_store
                .native_transaction(true, move |state| {
                    native_handoff_entry(state, &handoff)?.state = NativeReplyState::Unknown;
                    entered_tx
                        .send(())
                        .map_err(|_| anyhow::anyhow!("test waiter cancelled early"))?;
                    release_rx.recv()?;
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        writer.abort();
        let _ = writer.await;
        let private_store = store.clone();
        let mut private_writer = tokio::spawn(async move {
            private_store
                .ensure_binding(
                    "private",
                    canonical_conversation(),
                    "source",
                    "sender",
                    "agent",
                    ArkretDeliveryMode::InteractiveChat,
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut private_writer)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        private_writer.await.unwrap().unwrap();
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        assert!(matches!(
            cold.native_prepare(slot.clone(), input.clone())
                .await
                .unwrap(),
            NativeReplyPreparation::StartedUnknown
        ));
        assert!(
            cold.native_claim_handoff(
                slot,
                input,
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap()
            )
            .await
            .is_err()
        );
        assert!(cold.binding_for_session("private").await.unwrap().is_some());
    }

    fn native_test_signed_commit(
        submission: &garth::FrozenMessageSubmission,
        position: u64,
    ) -> arkret::RealmCommit {
        // Real signature under a test key; this is NOT a native authority-chain
        // fixture. Production's original verified receiver owns that check.
        let key = ed25519_dalek::SigningKey::from_bytes(&[79; 32]);
        let method = arkret::DidUrl::new("did:web:station.example#authority").unwrap();
        let at = DateTime::from_timestamp(1_735_689_602, 0).unwrap();
        let event = &submission.request().submission.event;
        let mut commit = arkret::RealmCommit {
            commit_id: arkret::RealmCommitId::from_digest([0; 32]),
            realm_id: event.realm_id.clone(),
            stream_ref: arkret::CommitStreamRef::from_scope(&event.scope_ref, None).unwrap(),
            stream_position: position,
            previous_commit_ref: Some(native_slot(2).source.commit_id),
            event_ref: event.event_id.clone(),
            governance_generation: 0,
            authority_ref: arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(
                native_slot(1).source.event_id,
            ),
            committed_at: at,
            producer_signer_fact_digest: None,
            signature: arkret::signatures::detached_object::sign_detached_object(
                &serde_json::json!({}),
                arkret::DetachedSignatureContext::RealmCommit,
                method.clone(),
                at,
                &key,
            )
            .unwrap(),
        };
        let mut body = serde_json::to_value(&commit).unwrap();
        body.as_object_mut().unwrap().remove("commit_id");
        body.as_object_mut().unwrap().remove("signature");
        commit.commit_id = arkret::RealmCommitId::from_digest(arkret::canonical::sha256_bytes(
            &arkret::canonical::canonical_json_bytes(&body).unwrap(),
        ));
        let unsigned = arkret::canonical::unsigned_value(&commit, &["signature"]).unwrap();
        commit.signature = arkret::signatures::detached_object::sign_detached_object(
            &unsigned,
            arkret::DetachedSignatureContext::RealmCommit,
            method,
            at,
            &key,
        )
        .unwrap();
        commit.validate_content_address().unwrap();
        arkret::signatures::detached_object::verify_detached_object_signature(
            &commit.signature,
            &unsigned,
            arkret::DetachedSignatureContext::RealmCommit,
            &arkret::signatures::PublicKeyMaterial::Ed25519Raw {
                bytes: key.verifying_key().to_bytes().to_vec(),
            },
        )
        .unwrap();
        commit
    }

    #[tokio::test]
    async fn native_ledger_settlement_binds_exact_signed_event_and_receipt() {
        let home = tempfile::tempdir().unwrap();
        let (store, slot, input) = prepared_native_store(home.path()).await;
        let handoff = store
            .native_claim_handoff(
                slot.clone(),
                input.clone(),
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap(),
            )
            .await
            .unwrap();
        let output = store
            .native_retain_output(handoff, "settled response".to_owned())
            .await
            .unwrap();
        let submission = frozen_submission(&slot.conversation, &output, 1_735_689_600);
        let retained = store
            .native_retain_submission(
                slot.clone(),
                input.clone(),
                native_hash(output.as_bytes()).unwrap(),
                submission,
            )
            .await
            .unwrap();
        let commit = native_test_signed_commit(&retained, 3);
        let different_event = frozen_submission(&slot.conversation, &output, 1_735_689_601);
        assert!(
            store
                .native_mark_accepted(
                    slot.clone(),
                    input.clone(),
                    native_test_signed_commit(&different_event, 3)
                )
                .await
                .is_err()
        );
        store
            .native_mark_accepted(slot.clone(), input.clone(), commit.clone())
            .await
            .unwrap();
        assert!(
            store
                .native_mark_accepted(
                    slot.clone(),
                    input.clone(),
                    native_test_signed_commit(&retained, 4)
                )
                .await
                .is_err()
        );
        let cold = ArkretExecutionBindingStore::new(home.path());
        cold.native_resume_required_existing().await.unwrap();
        match cold
            .native_prepare(slot.clone(), input.clone())
            .await
            .unwrap()
        {
            NativeReplyPreparation::Accepted { receipt } => {
                assert!(
                    receipt.event_id == commit.event_ref
                        && receipt.commit_id == commit.commit_id
                        && receipt.stream_position == 3
                );
            }
            _ => panic!("settled reply state mismatch"),
        }
        assert!(
            cold.native_claim_handoff(
                slot,
                input,
                "https://provider.example/v1/responses".to_owned(),
                native_hash(b"request").unwrap()
            )
            .await
            .is_err()
        );
    }

    async fn canonical_checkpoint(
        store: &ArkretExecutionBindingStore,
        target: &RemoteConversationKey,
    ) -> (DeliveryCheckpoint, String) {
        let (binding, _) = store
            .ensure_binding(
                "session-one",
                target.clone(),
                &arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [34; 32])
                    .to_string(),
                "ak:did_core:web:controller.example",
                "ak:did_core:web:agent.example",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .unwrap();
        store
            .enqueue_checkpoint(
                binding.binding_id,
                DeliveryCheckpointKind::Milestone,
                "Ready".to_owned(),
                "A deterministic public result.".to_owned(),
                vec!["local storage regression".to_owned()],
                Vec::new(),
                Vec::new(),
                "operator_via_agent",
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn retained_submission_replays_after_restart_before_account_enqueue() {
        let home = tempfile::tempdir().unwrap();
        let target = canonical_conversation();
        let store = ArkretExecutionBindingStore::new(home.path());
        let (checkpoint, body) = canonical_checkpoint(&store, &target).await;
        let first = frozen_submission(&target, &body, 1_790_000_000);
        let expected_bytes = first.canonical_submission_bytes().to_vec();
        let expected_id = first.request().submission.event.event_id.clone();
        store
            .retain_checkpoint_submission(checkpoint.checkpoint_id, &target, &body, first)
            .await
            .unwrap();
        drop(store);
        // Simulate the crash between retaining the producer bytes and enqueue.
        let reopened = ArkretExecutionBindingStore::new(home.path());
        let replay = reopened
            .checkpoint_submission(checkpoint.checkpoint_id, &target, &body)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay.canonical_submission_bytes(), expected_bytes);
        assert_eq!(replay.request().submission.event.event_id, expected_id);
        let account_path = home.path().join("account.json");
        let outbound = garth::OutboundEngine::new(
            garth::FileStore::open(&account_path).unwrap(),
            garth::SystemClock,
        );
        outbound
            .enqueue(replay.into_queued_submission())
            .await
            .unwrap();
        drop(outbound);
        let queue = garth::OutboundEngine::new(
            garth::FileStore::open(account_path).unwrap(),
            garth::SystemClock,
        );
        let snapshot = queue.snapshot().await.unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].event_id(), &expected_id);
        assert_eq!(
            arkret::canonical::canonical_json_bytes(&snapshot.items[0].submission.request).unwrap(),
            expected_bytes
        );
    }

    #[tokio::test]
    async fn concurrent_checkpoint_authors_reuse_one_durable_submission() {
        let home = tempfile::tempdir().unwrap();
        let target = canonical_conversation();
        let first_store = ArkretExecutionBindingStore::new(home.path());
        let other_store = ArkretExecutionBindingStore::new(home.path());
        let (checkpoint, body) = canonical_checkpoint(&first_store, &target).await;
        let first = frozen_submission(&target, &body, 1_790_000_000);
        let other = frozen_submission(&target, &body, 1_790_000_001);
        assert_ne!(
            first.request().submission.event.event_id,
            other.request().submission.event.event_id
        );
        let (left, right) = tokio::join!(
            first_store.retain_checkpoint_submission(
                checkpoint.checkpoint_id,
                &target,
                &body,
                first
            ),
            other_store.retain_checkpoint_submission(
                checkpoint.checkpoint_id,
                &target,
                &body,
                other
            ),
        );
        assert_eq!(
            left.unwrap().canonical_submission_bytes(),
            right.unwrap().canonical_submission_bytes()
        );
    }

    #[tokio::test]
    async fn checkpoint_retention_rejects_wrong_target_or_changed_public_body() {
        let home = tempfile::tempdir().unwrap();
        let target = canonical_conversation();
        let store = ArkretExecutionBindingStore::new(home.path());
        let (checkpoint, body) = canonical_checkpoint(&store, &target).await;
        let signed = frozen_submission(&target, &body, 1_790_000_000);
        let mut other_account = target.clone();
        other_account.account_id = "other-account".to_owned();
        assert!(
            store
                .retain_checkpoint_submission(
                    checkpoint.checkpoint_id,
                    &other_account,
                    &body,
                    signed.clone()
                )
                .await
                .is_err()
        );
        assert!(
            store
                .retain_checkpoint_submission(
                    checkpoint.checkpoint_id,
                    &target,
                    "changed",
                    signed.clone()
                )
                .await
                .is_err()
        );
        let mut other_strand = target.clone();
        other_strand.strand_id = arkret::StrandId::from_event_id(&arkret::EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            [51; 32],
        ))
        .to_string();
        let wrong_scope = frozen_submission(&other_strand, &body, 1_790_000_000);
        assert!(
            store
                .retain_checkpoint_submission(checkpoint.checkpoint_id, &target, &body, wrong_scope)
                .await
                .is_err()
        );
        assert!(
            store
                .checkpoint_submission(checkpoint.checkpoint_id, &target, &body)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn native_sidecar_history_and_execution_stay_isolated_from_source_strand_after_reopen() {
        let home = tempfile::tempdir().unwrap();
        let shared = canonical_conversation();
        let mut private = shared.clone();
        private.stream_ref = arkret::CommitStreamRef::Sidecar {
            realm_id: arkret::RealmId::new(private.realm_id.clone()).unwrap(),
            sidecar_id: arkret::SidecarId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [55; 32],
            )),
        };
        assert_ne!(private.routing_scope(), shared.routing_scope());
        let store = ArkretExecutionBindingStore::new(home.path());
        for (key, body, session) in [
            (shared.clone(), "shared history", "shared-session"),
            (private.clone(), "private history", "private-session"),
        ] {
            store
                .hydrate_event(
                    key.clone(),
                    RemoteContextEvent {
                        event_id: body.to_owned(),
                        sender_did: "controller".to_owned(),
                        sender_kind: "human".to_owned(),
                        body: body.to_owned(),
                        received_at: Utc::now(),
                    },
                )
                .await
                .unwrap();
            store
                .ensure_binding(
                    session,
                    key,
                    body,
                    "controller",
                    "agent",
                    ArkretDeliveryMode::InteractiveChat,
                )
                .await
                .unwrap();
        }
        let reopened = ArkretExecutionBindingStore::new(home.path());
        assert_eq!(
            reopened.remote_snapshot(&shared).await.unwrap().events[0].body,
            "shared history"
        );
        assert_eq!(
            reopened.remote_snapshot(&private).await.unwrap().events[0].body,
            "private history"
        );
        assert_eq!(
            reopened
                .binding_for_conversation(&shared)
                .await
                .unwrap()
                .unwrap()
                .local_session_id,
            "shared-session"
        );
        assert_eq!(
            reopened
                .binding_for_conversation(&private)
                .await
                .unwrap()
                .unwrap()
                .local_session_id,
            "private-session"
        );
        let shared_submission = frozen_submission(&shared, "reply", 1_790_000_000);
        assert!(
            ArkretExecutionBindingStore::validate_submission_target(&shared_submission, &private)
                .is_err()
        );
        let mut wrong_realm = private.clone();
        wrong_realm.realm_id = arkret::RealmId::from_event_id(&arkret::EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            [56; 32],
        ))
        .to_string();
        assert!(wrong_realm.validate().is_err());
    }

    #[test]
    fn conversation_key_isolates_every_remote_dimension() {
        let base = conversation("ak:strand:one");
        let mut variants = Vec::new();
        let mut value = base.clone();
        value.channel_config_id = "sales".to_owned();
        variants.push(value);
        let mut value = base.clone();
        value.account_id = "other-account".to_owned();
        variants.push(value);
        let mut value = base.clone();
        value.realm_id = "ak:realm:two".to_owned();
        variants.push(value);
        variants.push(conversation("ak:strand:two"));
        assert!(
            variants
                .into_iter()
                .all(|value| { value != base && value.routing_scope() != base.routing_scope() })
        );
    }

    #[test]
    fn renderer_redacts_credentials_and_local_paths() {
        let rendered = sanitize_public_text(
            "done\nAuthorization: Bearer secret\nsk-12345678901234567890\nD:\\work\\private\\result.txt",
        )
        .expect("sanitized");
        assert!(rendered.contains("done"));
        assert!(rendered.contains("[sensitive detail redacted]"));
        assert!(rendered.contains("[local path redacted]"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("sk-123"));
        assert!(!rendered.contains("D:\\"));
    }

    #[tokio::test]
    async fn binding_store_isolates_strands_and_survives_reopen() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ArkretExecutionBindingStore::new(home.path());
        let (first, created) = store
            .ensure_binding(
                "session-one",
                conversation("ak:strand:one"),
                "ak:event:one",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .expect("create first binding");
        assert!(created);
        let (second, created) = store
            .ensure_binding(
                "session-two",
                conversation("ak:strand:two"),
                "ak:event:two",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .expect("create second binding");
        assert!(created);
        assert_ne!(first.binding_id, second.binding_id);

        let reopened = ArkretExecutionBindingStore::new(home.path());
        assert_eq!(
            reopened
                .binding_for_conversation(&conversation("ak:strand:one"))
                .await
                .expect("reload binding")
                .expect("binding exists")
                .local_session_id,
            "session-one"
        );
        let error = reopened
            .ensure_binding(
                "session-one",
                conversation("ak:strand:three"),
                "ak:event:three",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .expect_err("one active session cannot cross delivery targets");
        assert!(error.to_string().contains("another Arkret delivery target"));
    }

    #[tokio::test]
    async fn delivery_mode_change_isolates_private_sessions_and_preserves_original_route() {
        let home = tempfile::tempdir().expect("tempdir");
        let conversation = conversation("ak:strand:one");
        let store = ArkretExecutionBindingStore::new(home.path());
        let original_scope = store
            .routing_scope_for_mode(&conversation, ArkretDeliveryMode::TaskDelivery)
            .await
            .unwrap();
        let (private, _) = store
            .ensure_binding(
                "private-session",
                conversation.clone(),
                "ak:event:one",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .unwrap();
        let interactive_scope = store
            .routing_scope_for_mode(&conversation, ArkretDeliveryMode::InteractiveChat)
            .await
            .unwrap();
        assert_ne!(interactive_scope, original_scope);
        assert!(
            store
                .ensure_binding(
                    "private-session",
                    conversation.clone(),
                    "ak:event:two",
                    "did:example:human",
                    "did:example:agent",
                    ArkretDeliveryMode::InteractiveChat,
                )
                .await
                .is_err()
        );
        let (interactive, created) = store
            .ensure_binding(
                "interactive-session",
                conversation.clone(),
                "ak:event:two",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::InteractiveChat,
            )
            .await
            .unwrap();
        assert!(created);
        assert_ne!(interactive.binding_id, private.binding_id);
        let reopened = ArkretExecutionBindingStore::new(home.path());
        assert_eq!(
            reopened
                .routing_scope_for_mode(&conversation, ArkretDeliveryMode::TaskDelivery)
                .await
                .unwrap(),
            original_scope
        );
        assert_eq!(
            reopened
                .routing_scope_for_mode(&conversation, ArkretDeliveryMode::InteractiveChat)
                .await
                .unwrap(),
            interactive_scope
        );
        assert_eq!(
            reopened
                .binding_for_session("private-session")
                .await
                .unwrap()
                .unwrap()
                .mode,
            ArkretDeliveryMode::TaskDelivery
        );
        assert_eq!(
            reopened
                .binding_for_session("interactive-session")
                .await
                .unwrap()
                .unwrap()
                .mode,
            ArkretDeliveryMode::InteractiveChat
        );
        let (continued, created) = reopened
            .ensure_binding(
                "interactive-session",
                conversation,
                "ak:event:three",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::InteractiveChat,
            )
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(continued.binding_id, interactive.binding_id);
    }

    #[tokio::test]
    async fn interactive_chat_continues_one_scope_across_requests() {
        let home = tempfile::tempdir().unwrap();
        let store = ArkretExecutionBindingStore::new(home.path());
        let conversation = canonical_conversation();
        let first = store
            .routing_scope_for_request(
                &conversation,
                ArkretDeliveryMode::InteractiveChat,
                Some("owner-route"),
                "ak:event:first",
            )
            .await
            .unwrap();
        let continued = store
            .routing_scope_for_request(
                &conversation,
                ArkretDeliveryMode::InteractiveChat,
                Some("owner-route"),
                "ak:event:continued",
            )
            .await
            .unwrap();
        assert_eq!(
            first, continued,
            "the same Chat must retain its local execution session across messages"
        );
        let (original, created) = store
            .ensure_binding(
                "chat-session",
                conversation.clone(),
                "ak:event:first",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::InteractiveChat,
            )
            .await
            .unwrap();
        assert!(created);
        let reopened = ArkretExecutionBindingStore::new(home.path());
        let (continued, created) = reopened
            .ensure_binding(
                "chat-session",
                conversation,
                "ak:event:continued",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::InteractiveChat,
            )
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(continued.binding_id, original.binding_id);
        assert_eq!(continued.source_event_id, original.source_event_id);
        assert_eq!(
            continued.last_ingested_event_id.as_deref(),
            Some("ak:event:continued")
        );
    }

    #[tokio::test]
    async fn task_and_sidecar_requests_keep_distinct_sessions_and_exact_retry_bindings() {
        let shared = canonical_conversation();
        let mut sidecar = shared.clone();
        sidecar.stream_ref = arkret::CommitStreamRef::Sidecar {
            realm_id: arkret::RealmId::new(shared.realm_id.clone()).unwrap(),
            sidecar_id: arkret::SidecarId::from_event_id(&arkret::EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [58; 32],
            )),
        };
        for (conversation, mode) in [
            (shared, ArkretDeliveryMode::TaskDelivery),
            (sidecar, ArkretDeliveryMode::InteractiveChat),
        ] {
            let home = tempfile::tempdir().unwrap();
            let store = ArkretExecutionBindingStore::new(home.path());
            let mut routes = Vec::new();
            let mut bindings = Vec::new();
            for (session, request) in [
                ("first-session", "ak:event:first"),
                ("second-session", "ak:event:second"),
            ] {
                routes.push(
                    store
                        .routing_scope_for_request(
                            &conversation,
                            mode,
                            Some("owner-route"),
                            request,
                        )
                        .await
                        .unwrap(),
                );
                let (binding, created) = store
                    .ensure_binding(
                        session,
                        conversation.clone(),
                        request,
                        "did:example:human",
                        "did:example:agent",
                        mode,
                    )
                    .await
                    .unwrap();
                assert!(created);
                bindings.push(binding);
            }
            assert_ne!(routes[0], routes[1]);
            assert_ne!(bindings[0].binding_id, bindings[1].binding_id);
            let reopened = ArkretExecutionBindingStore::new(home.path());
            let (retried, created) = reopened
                .ensure_binding(
                    "first-session",
                    conversation.clone(),
                    "ak:event:first",
                    "did:example:human",
                    "did:example:agent",
                    mode,
                )
                .await
                .unwrap();
            assert!(!created);
            assert_eq!(retried.binding_id, bindings[0].binding_id);
            assert_eq!(retried.source_event_id, bindings[0].source_event_id);
            assert_eq!(retried.conversation, bindings[0].conversation);
            assert!(
                reopened
                    .ensure_binding(
                        "first-session",
                        conversation,
                        "ak:event:third",
                        "did:example:human",
                        "did:example:agent",
                        mode,
                    )
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn pending_checkpoint_is_durable_and_scoped_to_bound_account() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ArkretExecutionBindingStore::new(home.path());
        let (binding, _) = store
            .ensure_binding(
                "session-one",
                conversation("ak:strand:one"),
                "ak:event:one",
                "did:example:human",
                "did:example:agent",
                ArkretDeliveryMode::TaskDelivery,
            )
            .await
            .expect("binding");
        let (checkpoint, _) = store
            .enqueue_checkpoint(
                binding.binding_id,
                DeliveryCheckpointKind::Milestone,
                "Phase complete".to_owned(),
                "The stable result is ready.".to_owned(),
                vec!["targeted tests passed".to_owned()],
                Vec::new(),
                vec!["continue".to_owned()],
                "operator_via_agent",
            )
            .await
            .expect("enqueue");

        let reopened = ArkretExecutionBindingStore::new(home.path());
        let pending = reopened
            .pending_for_account("support", "agent-account")
            .await
            .expect("load pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].checkpoint.checkpoint_id,
            checkpoint.checkpoint_id
        );
        assert!(
            reopened
                .pending_for_account("support", "different-account")
                .await
                .expect("load other account")
                .is_empty()
        );
        reopened
            .mark_published(checkpoint.checkpoint_id, "ak:event:published")
            .await
            .expect("mark checkpoint published");
        assert_eq!(
            reopened
                .last_published_rendered(binding.binding_id)
                .await
                .expect("load last public checkpoint")
                .as_deref(),
            Some(pending[0].rendered_body.as_str())
        );
    }
}
