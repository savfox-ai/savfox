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
            "{}:{}:{}:{}",
            self.channel_config_id, self.account_id, self.realm_id, stream_digest
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
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ArkretExecutionBindingStore {
    path: PathBuf,
}

impl ArkretExecutionBindingStore {
    #[must_use]
    pub(crate) fn new(savfox_home: &Path) -> Self {
        Self {
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
        savfox_utils::fs::write_atomically_async(&self.path, bytes, Some(0o600))
            .await
            .with_context(|| format!("persist {}", self.path.display()))
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
        let (_guard, _file_lock) = self.lock().await?;
        let mut state = self.load_unlocked().await?;
        if let Some(binding) = state
            .bindings
            .values_mut()
            .find(|binding| binding.conversation == conversation && binding.mode == mode)
        {
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
                    && (binding.conversation != conversation || binding.mode != mode)
                    && !matches!(
                        binding.state,
                        DeliveryState::Completed | DeliveryState::Failed | DeliveryState::Cancelled
                    )
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
        assert!(variants.into_iter().all(|value| value != base));
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
