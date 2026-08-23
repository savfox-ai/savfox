//! Adapt Arkret account-stream Events and Notifications into savfox internal
//! commands.
//!
//! Frame-level parsing (NDJSON `Delta` / `CatchupComplete` / etc.) is provided
//! by the Arkret SDK
//! (`arkret::AccountSubscribeFrame` / [`AccountSubscribeFrame::from_ndjson_line`]).
//! This module owns the host-dispatch boundary: given an SDK Event or a
//! Notification projection, decide whether Savfox should dispatch and how.

use arkret::EventPayloadExt as _;
use garth::{DecodedInbound, InboundDecoder};
use serde_json::Value;

use super::config::ArkretAccountConfig;
use super::crypto_state::{
    extract_encrypted_metadata_payload_from_message_content,
    extract_encrypted_payload_from_message_content, message_content_has_encrypted_carrier,
};
use super::sidecar::{SidecarExchangeContext, SidecarRequestOrdering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretInboundEvent {
    pub account_id: String,
    pub event_id: String,
    pub realm_id: String,
    /// Host-facing conversation type derived from the containing Realm sync
    /// projection. Event envelopes do not carry the Realm role themselves.
    pub chat_type: Option<String>,
    /// Joined participant count from the containing Realm sync projection.
    pub participant_count: Option<u32>,
    pub strand_id: Option<String>,
    pub sender_did: String,
    pub body: String,
    pub thread_root_id: Option<String>,
    /// Actor DIDs carried by structured `content.mentions` nodes.
    ///
    /// The gateway keeps this data through the host-dispatch boundary so a
    /// personal-agent runtime can explain whether the event explicitly
    /// addressed its Arkret principal instead of routing on body text alone.
    pub mentioned_actor_ids: Vec<String>,
    /// Verified Agent Sidecar exchange identity (`zh/models/sidecar.md`
    /// §7.2.1–§7.2.2), present only when this event carried a valid
    /// `role=request` binding addressed to this runtime's principal and the
    /// exchange passed the durable idempotency gate. `None` means the event is
    /// an ordinary (non-exchange) message.
    pub sidecar_exchange: Option<SidecarExchangeContext>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArkretInboundParseResult {
    pub events: Vec<ArkretInboundEvent>,
    pub skipped: Vec<ArkretInboundSkippedEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretInboundSkippedEvent {
    pub account_id: String,
    pub event_id: Option<String>,
    pub realm_id: Option<String>,
    pub chat_type: Option<String>,
    pub participant_count: Option<u32>,
    pub sender_did: Option<String>,
    pub strand_id: Option<String>,
    pub reply_to: Option<String>,
    pub encrypted_payload: Option<arkret::EncryptedPayload>,
    /// Parallel `encrypted_metadata` carrier (same envelope shape as
    /// `encrypted_content`); its plaintext may hold the Sidecar exchange
    /// binding (`message_metadata.sidecar_exchange_binding`).
    pub encrypted_metadata_payload: Option<arkret::EncryptedPayload>,
    /// Canonical-request ordering keys read from the Event envelope
    /// (`zh/models/sidecar.md` §7.2.1: smallest `actor_seq`, then
    /// bytewise-maximum `event_digest`). Present whenever the envelope parsed;
    /// absent for malformed events and Notification projections, which can
    /// therefore never admit a Sidecar request.
    pub request_ordering: Option<SidecarRequestOrdering>,
    pub reason: ArkretInboundSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArkretInboundSkipReason {
    KindNotMessageCreate,
    MissingRequiredField(&'static str),
    EncryptedContent,
    UnsupportedContentKind(String),
    UnsupportedNotificationKind(String),
    LoopbackFromAccount,
    ContentReadNotGranted,
    EmptyBody,
    /// A valid Sidecar `role=request` binding whose `addressed_agent_ids`
    /// does not contain this account's principal: the request is treated as
    /// nonexistent — no execution, no error (`zh/models/sidecar.md` §7.2.2).
    SidecarNotAddressed,
    /// `ak.agent.sidecar.exchange.control` delivered on a private Strand this
    /// runtime is an MLS member of. It is never dispatched to the agent, but
    /// it is the only source of terminal exchange state (§7.2.3), so the
    /// gateway decrypts and folds it instead of discarding it as a non-message
    /// kind.
    SidecarExchangeControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArkretInboundEventOutcome {
    Dispatchable(Box<ArkretInboundEvent>),
    Skip(Box<ArkretInboundSkippedEvent>),
}

/// Extract a `ak.message.create` Event payload into a savfox inbound command.
///
/// Returns `None` for non-message events, redacted/missing content, or
/// unsupported content kinds (anything other than `ak.content.text`).
#[must_use]
pub fn extract_message_event(event: &Value, account_id: &str) -> Option<ArkretInboundEvent> {
    match classify_message_event(event, account_id) {
        ArkretInboundEventOutcome::Dispatchable(event) => Some(*event),
        ArkretInboundEventOutcome::Skip(_) => None,
    }
}

/// Classify one Arkret event for the account-mode inbound path.
///
/// Encrypted payload carriers are detected explicitly and reported as
/// [`ArkretInboundSkipReason::EncryptedContent`]. Savfox does not yet keep the
/// Arkret MLS/session state required to decrypt these payloads, so callers must
/// fail closed instead of treating them as generic unsupported content.
#[must_use]
pub fn classify_message_event(event: &Value, account_id: &str) -> ArkretInboundEventOutcome {
    let Ok(event_envelope) = sdk_event_from_value(event) else {
        return classify_malformed_event(event, account_id);
    };
    let decoded = InboundDecoder::new().try_decode_event(event_envelope.clone());
    match decoded {
        Ok(DecodedInbound::Message(message)) => match message.payload {
            arkret::MessageEventPayload::Create(payload) => {
                classify_sdk_message_create(&message.event, &payload, account_id)
            }
            arkret::MessageEventPayload::Revise(_)
            | arkret::MessageEventPayload::Redact(_)
            | arkret::MessageEventPayload::ReactionAdd(_)
            | arkret::MessageEventPayload::ReactionRemove(_) => skip_sdk_event(
                &message.event,
                account_id,
                ArkretInboundSkipReason::KindNotMessageCreate,
            ),
        },
        Ok(DecodedInbound::Event(event_envelope)) => {
            if event_envelope.kind == arkret::EventKind::AgentSidecarExchangeControl {
                return classify_sidecar_exchange_control(&event_envelope, account_id);
            }
            skip_sdk_event(
                &event_envelope,
                account_id,
                ArkretInboundSkipReason::KindNotMessageCreate,
            )
        }
        Err(_) if message_content_has_encrypted_carrier(&event_envelope.payload) => {
            skip_encrypted_sdk_event(&event_envelope, account_id)
        }
        Err(_) => skip_sdk_event(
            &event_envelope,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("message payload"),
        ),
    }
}

fn sdk_event_from_value(event: &Value) -> Result<arkret::Event, serde_json::Error> {
    serde_json::from_value(event.clone())
}

fn classify_sdk_message_create(
    event: &arkret::Event,
    payload: &arkret::MessageCreatePayload,
    account_id: &str,
) -> ArkretInboundEventOutcome {
    if payload.encrypted_content.is_some() || message_content_has_encrypted_carrier(&event.payload)
    {
        return skip_encrypted_sdk_event(event, account_id);
    }
    let Some(content) = payload.content.as_ref() else {
        return skip_sdk_event(
            event,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("content"),
        );
    };
    let content_kind = content.kind.as_str();
    if content_kind == "ak.content.encrypted" {
        return skip_encrypted_sdk_event(event, account_id);
    }
    if content_kind != "ak.content.text" {
        return skip_sdk_event(
            event,
            account_id,
            ArkretInboundSkipReason::UnsupportedContentKind(content_kind.to_owned()),
        );
    }
    let body = content.body.as_str();
    let strand_id = Some(payload.strand_id.as_str().to_owned());
    let thread_root_id = payload.reply_to.clone();
    let mentioned_actor_ids = structured_mention_actor_ids(content);

    ArkretInboundEventOutcome::Dispatchable(Box::new(ArkretInboundEvent {
        account_id: account_id.to_owned(),
        event_id: event.event_id.as_str().to_owned(),
        realm_id: event.realm_id.as_str().to_owned(),
        chat_type: None,
        participant_count: None,
        strand_id,
        sender_did: event.actor_id.as_str().to_owned(),
        body: body.to_owned(),
        thread_root_id,
        mentioned_actor_ids,
        sidecar_exchange: None,
    }))
}

fn structured_mention_actor_ids(content: &arkret::ContentBlock) -> Vec<String> {
    let mut actor_ids = content
        .extra
        .get("mentions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mention| {
            mention
                .get("subject_id")
                .or_else(|| mention.get("actor_id"))
                .or_else(|| mention.get("did"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|actor_id| !actor_id.is_empty())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    actor_ids.sort_unstable();
    actor_ids.dedup();
    actor_ids
}

fn classify_malformed_event(event: &Value, account_id: &str) -> ArkretInboundEventOutcome {
    let Some(kind) = event.get("kind").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("kind"),
        );
    };
    if kind != "ak.message.create" {
        return skip_event(
            event,
            account_id,
            ArkretInboundSkipReason::KindNotMessageCreate,
        );
    }
    skip_event(
        event,
        account_id,
        ArkretInboundSkipReason::MissingRequiredField("event envelope"),
    )
}

/// Decide whether the parsed event should be dispatched to the agent pipeline.
///
/// Rules:
/// * Drop messages sent by the listening account itself (loop guard).
#[must_use]
pub fn should_dispatch_event(event: &ArkretInboundEvent, account: &ArkretAccountConfig) -> bool {
    dispatch_skip_reason(event, account).is_none()
}

#[must_use]
pub fn account_allows_event_read(account: &ArkretAccountConfig) -> bool {
    account
        .requested_scope
        .iter()
        .any(|action| action == "ak.event.read")
}

fn dispatch_skip_reason(
    event: &ArkretInboundEvent,
    account: &ArkretAccountConfig,
) -> Option<ArkretInboundSkipReason> {
    if event.sender_did.eq_ignore_ascii_case(&account.principal_id) {
        return Some(ArkretInboundSkipReason::LoopbackFromAccount);
    }
    if !account_allows_event_read(account) {
        return Some(ArkretInboundSkipReason::ContentReadNotGranted);
    }
    if event.body.trim().is_empty() {
        return Some(ArkretInboundSkipReason::EmptyBody);
    }
    None
}

/// Walk a `Delta` frame body's `realms` object and extract every dispatchable
/// `ak.message.create` event for the given account.
///
/// The shape expected is the v1 account subscribe `delta` shape:
/// `realms.<realm_id>.timeline.events[] : Event`. Tolerates missing nested
/// fields by returning an empty list rather than failing.
#[must_use]
pub fn parse_delta_frame_for_account(
    realms_value: &Value,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let mut events = Vec::new();
    let mut skipped = Vec::new();
    let Some(realms) = realms_value.as_object() else {
        return ArkretInboundParseResult { events, skipped };
    };
    for (_realm_id, realm_body) in realms {
        let timeline = realm_body
            .get("timeline")
            .and_then(|t| t.get("events"))
            .and_then(Value::as_array);
        let Some(timeline) = timeline else { continue };
        for raw_event in timeline {
            match classify_message_event(raw_event, &account.id) {
                ArkretInboundEventOutcome::Dispatchable(parsed) => {
                    if let Some(reason) = dispatch_skip_reason(&parsed, account) {
                        skipped.push(ArkretInboundSkippedEvent {
                            account_id: parsed.account_id.clone(),
                            event_id: Some(parsed.event_id.clone()),
                            realm_id: Some(parsed.realm_id.clone()),
                            chat_type: parsed.chat_type.clone(),
                            participant_count: parsed.participant_count,
                            sender_did: Some(parsed.sender_did.clone()),
                            strand_id: parsed.strand_id.clone(),
                            reply_to: parsed.thread_root_id.clone(),
                            encrypted_payload: None,
                            encrypted_metadata_payload: None,
                            request_ordering: None,
                            reason,
                        });
                    } else {
                        events.push(*parsed);
                    }
                }
                ArkretInboundEventOutcome::Skip(event) => skipped.push(*event),
            }
        }
    }
    ArkretInboundParseResult { events, skipped }
}

/// Walk an account-stream `notifications` delta and extract task-oriented
/// notifications that should wake the configured savfox agent.
///
/// The Arkret v1 account stream carries notification projection deltas at the
/// top level as either `{ "events": [...] }`, `{ "items": [...] }`, or a raw
/// array. Savfox only dispatches `assignment` and `schedule` notifications here:
/// message/mention notifications are already represented by visible
/// `ak.message.create` timeline events, and dispatching both would double-wake
/// the agent.
#[must_use]
pub fn parse_notification_delta_for_account(
    notifications_value: &Value,
    account: &ArkretAccountConfig,
) -> ArkretInboundParseResult {
    let mut events = Vec::new();
    let mut skipped = Vec::new();
    let Some(notifications) = notification_items(notifications_value) else {
        return ArkretInboundParseResult { events, skipped };
    };
    for raw_notification in notifications {
        match classify_notification_event(raw_notification, &account.id, &account.principal_id) {
            ArkretInboundEventOutcome::Dispatchable(parsed) => {
                if let Some(reason) = dispatch_skip_reason(&parsed, account) {
                    skipped.push(ArkretInboundSkippedEvent {
                        account_id: parsed.account_id.clone(),
                        event_id: Some(parsed.event_id.clone()),
                        realm_id: Some(parsed.realm_id.clone()),
                        chat_type: parsed.chat_type.clone(),
                        participant_count: parsed.participant_count,
                        sender_did: Some(parsed.sender_did.clone()),
                        strand_id: parsed.strand_id.clone(),
                        reply_to: parsed.thread_root_id.clone(),
                        encrypted_payload: None,
                        encrypted_metadata_payload: None,
                        request_ordering: None,
                        reason,
                    });
                } else {
                    events.push(*parsed);
                }
            }
            ArkretInboundEventOutcome::Skip(event) => skipped.push(*event),
        }
    }
    ArkretInboundParseResult { events, skipped }
}

fn notification_items(value: &Value) -> Option<&Vec<Value>> {
    if value.is_null() {
        return None;
    }
    value
        .get("events")
        .and_then(Value::as_array)
        .or_else(|| value.get("items").and_then(Value::as_array))
        .or_else(|| value.as_array())
}

fn classify_notification_event(
    notification: &Value,
    account_id: &str,
    principal_id: &str,
) -> ArkretInboundEventOutcome {
    let Some(notification_kind) = notification_kind(notification) else {
        return skip_notification(
            notification,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("notification_kind"),
        );
    };
    if !matches!(notification_kind, "assignment" | "schedule") {
        return skip_notification(
            notification,
            account_id,
            ArkretInboundSkipReason::UnsupportedNotificationKind(notification_kind.to_owned()),
        );
    }
    let Some(event_id) = notification_identity(notification) else {
        return skip_notification(
            notification,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("notification_id|source_event_id"),
        );
    };
    let Some(realm_id) = notification_string(notification, &["realm_id"]) else {
        return skip_notification(
            notification,
            account_id,
            ArkretInboundSkipReason::MissingRequiredField("realm_id"),
        );
    };
    let body = notification_body(notification_kind, notification);
    let sender_did = notification_sender(notification, principal_id);
    ArkretInboundEventOutcome::Dispatchable(Box::new(ArkretInboundEvent {
        account_id: account_id.to_owned(),
        event_id,
        realm_id,
        chat_type: None,
        participant_count: None,
        strand_id: notification_strand_id(notification),
        sender_did,
        body,
        thread_root_id: None,
        mentioned_actor_ids: Vec::new(),
        sidecar_exchange: None,
    }))
}

fn notification_kind(notification: &Value) -> Option<&str> {
    notification_string_ref(notification, &["notification_kind", "kind"])
}

fn notification_identity(notification: &Value) -> Option<String> {
    notification_string(notification, &["notification_id", "id"])
        .or_else(|| notification_string(notification, &["source_event_id", "event_id"]))
}

fn notification_sender(notification: &Value, principal_id: &str) -> String {
    notification_string(
        notification,
        &["source_actor_id", "sender_actor_id", "sender"],
    )
    .or_else(|| {
        notification_string(notification, &["actor_id"])
            .filter(|actor| !actor.eq_ignore_ascii_case(principal_id) && actor.starts_with("did:"))
    })
    .unwrap_or_else(|| "arkret:notification".to_owned())
}

fn notification_strand_id(notification: &Value) -> Option<String> {
    notification_string(notification, &["strand_id"]).or_else(|| {
        notification_string(notification, &["source_ref"])
            .filter(|source_ref| source_ref.starts_with("ak:strand:"))
    })
}

fn notification_body(notification_kind: &str, notification: &Value) -> String {
    let mut parts = Vec::new();
    let summary = notification_string(notification, &["body", "preview", "title"])
        .filter(|value| !value.trim().is_empty());
    match notification_kind {
        "assignment" => {
            parts.push("Arkret assignment notification: you were assigned to a Strand.".to_owned())
        }
        "schedule" => parts.push(
            "Arkret schedule notification: a due date or calendar schedule changed.".to_owned(),
        ),
        _ => parts.push(format!("Arkret {notification_kind} notification.")),
    }
    if let Some(summary) = summary {
        parts.push(format!("Summary: {summary}"));
    }
    if let Some(strand_id) = notification_strand_id(notification) {
        parts.push(format!("strand_id: {strand_id}"));
    }
    if let Some(source_event_id) =
        notification_string(notification, &["source_event_id", "event_id"])
    {
        parts.push(format!("source_event_id: {source_event_id}"));
    }
    if let Some(source_actor_id) = notification_string(
        notification,
        &["source_actor_id", "sender_actor_id", "sender", "actor_id"],
    ) {
        parts.push(format!("source_actor_id: {source_actor_id}"));
    }
    parts.join("\n")
}

fn notification_string(notification: &Value, keys: &[&str]) -> Option<String> {
    notification_string_ref(notification, keys).map(str::to_owned)
}

fn notification_string_ref<'a>(notification: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        notification
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn skip_event(
    event: &Value,
    account_id: &str,
    reason: ArkretInboundSkipReason,
) -> ArkretInboundEventOutcome {
    ArkretInboundEventOutcome::Skip(Box::new(ArkretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: event
            .get("event_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        realm_id: event
            .get("realm_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        chat_type: None,
        participant_count: None,
        sender_did: event
            .get("actor_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        strand_id: None,
        reply_to: None,
        encrypted_payload: None,
        encrypted_metadata_payload: None,
        request_ordering: None,
        reason,
    }))
}

/// Read the §7.2.1 canonical-request ordering keys off an Event envelope.
///
/// The digest is recomputed from the envelope rather than read from any
/// payload field, so a producer cannot bias the sibling tie-break. A digest
/// that cannot be computed yields `None`, which makes the request unadmittable.
fn request_ordering_from_event(event: &arkret::Event) -> Option<SidecarRequestOrdering> {
    Some(SidecarRequestOrdering {
        actor_seq: event.actor_seq,
        event_digest: event
            .event_digest_with_digest_suite(super::DIGEST_SUITE)
            .ok()?,
    })
}

/// `ak.agent.sidecar.exchange.control` carries only private-Strand routing and
/// ciphertext on the wire (§7.2.3). Surface both so the gateway can decrypt and
/// fold it; the plaintext never reaches the agent pipeline.
fn classify_sidecar_exchange_control(
    event: &arkret::Event,
    account_id: &str,
) -> ArkretInboundEventOutcome {
    let payload: Option<arkret::AgentSidecarExchangeControlPayload> =
        serde_json::to_value(&event.payload)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok());
    let (strand_id, encrypted_payload) = match payload {
        Some(payload) => (
            match payload.source_context_ref {
                arkret_models_collaboration::sidecar_operations::SidecarContextRef::Strand {
                    strand_id,
                } => Some(strand_id.as_str().to_owned()),
                arkret_models_collaboration::sidecar_operations::SidecarContextRef::Relation {
                    ..
                } => None,
            },
            super::crypto_state::encrypted_payload_for_event(event, &payload.encrypted_payload),
        ),
        None => (None, None),
    };
    ArkretInboundEventOutcome::Skip(Box::new(ArkretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: Some(event.event_id.as_str().to_owned()),
        realm_id: Some(event.realm_id.as_str().to_owned()),
        chat_type: None,
        participant_count: None,
        sender_did: Some(event.actor_id.as_str().to_owned()),
        strand_id,
        reply_to: None,
        encrypted_payload,
        encrypted_metadata_payload: None,
        request_ordering: request_ordering_from_event(event),
        reason: ArkretInboundSkipReason::SidecarExchangeControl,
    }))
}

fn skip_sdk_event(
    event: &arkret::Event,
    account_id: &str,
    reason: ArkretInboundSkipReason,
) -> ArkretInboundEventOutcome {
    ArkretInboundEventOutcome::Skip(Box::new(ArkretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: Some(event.event_id.as_str().to_owned()),
        realm_id: Some(event.realm_id.as_str().to_owned()),
        chat_type: None,
        participant_count: None,
        sender_did: Some(event.actor_id.as_str().to_owned()),
        strand_id: None,
        reply_to: None,
        encrypted_payload: None,
        encrypted_metadata_payload: None,
        request_ordering: request_ordering_from_event(event),
        reason,
    }))
}

fn skip_notification(
    notification: &Value,
    account_id: &str,
    reason: ArkretInboundSkipReason,
) -> ArkretInboundEventOutcome {
    ArkretInboundEventOutcome::Skip(Box::new(ArkretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: notification_identity(notification),
        realm_id: notification_string(notification, &["realm_id"]),
        chat_type: None,
        participant_count: None,
        sender_did: notification_string(
            notification,
            &["source_actor_id", "sender_actor_id", "sender", "actor_id"],
        ),
        strand_id: notification_strand_id(notification),
        reply_to: None,
        encrypted_payload: None,
        encrypted_metadata_payload: None,
        request_ordering: None,
        reason,
    }))
}

fn skip_encrypted_sdk_event(event: &arkret::Event, account_id: &str) -> ArkretInboundEventOutcome {
    let message = event.as_message_create().ok();
    ArkretInboundEventOutcome::Skip(Box::new(ArkretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: Some(event.event_id.as_str().to_owned()),
        realm_id: Some(event.realm_id.as_str().to_owned()),
        chat_type: None,
        participant_count: None,
        sender_did: Some(event.actor_id.as_str().to_owned()),
        strand_id: message
            .as_ref()
            .map(|payload| payload.strand_id.as_str().to_owned()),
        reply_to: message.and_then(|payload| payload.reply_to),
        encrypted_payload: extract_encrypted_payload_from_message_content(event),
        encrypted_metadata_payload: extract_encrypted_metadata_payload_from_message_content(event),
        request_ordering: request_ordering_from_event(event),
        reason: ArkretInboundSkipReason::EncryptedContent,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_account(_realm: Option<&str>) -> ArkretAccountConfig {
        ArkretAccountConfig {
            mode: crate::arkret::ArkretAccountMode::Agent,
            id: "support".into(),
            principal_id: "did:webvh:example.org:agents:support".into(),
            device_id: "ak:device:01".into(),
            key_ref: None,
            verification_method: None,
            inkson_bootstrap: None,
            authorized_event_ref: None,
            controller_id: Some("did:webvh:example.org:users:alice".into()),
            requested_scope: vec!["ak.event.read".into()],
            listen: true,
            send: true,
        }
    }

    const REALM_1: &str = "ak:realm:01904100-0000-8000-8000-000000000001";
    const REALM_2: &str = "ak:realm:01904100-0000-8000-8000-000000000002";
    const STRAND_1: &str = "ak:strand:01904100-0000-8000-8000-000000000011";
    const STRAND_2: &str = "ak:strand:01904100-0000-8000-8000-000000000012";

    fn message_event(actor: &str, body: &str) -> Value {
        message_event_in_realm(REALM_1, actor, STRAND_1, body)
    }

    fn message_event_in_realm(realm: &str, actor: &str, strand: &str, body: &str) -> Value {
        event_value(
            "ak.message.create",
            realm,
            actor,
            json!({
                "strand_id": strand,
                "track_name": "discussion",
                "content": {
                    "kind": "ak.content.text",
                    "body": body
                }
            }),
        )
    }

    fn sdk_message_event(actor: &str, body: &str) -> Value {
        message_event_in_realm(REALM_1, actor, STRAND_1, body)
    }

    fn event_value(kind: &str, realm: &str, actor: &str, payload: Value) -> Value {
        let event = arkret_wire::test_support::raw_event(
            kind,
            arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new(realm).unwrap(),
            },
            arkret::DidCoreId::new(actor.to_owned()).unwrap(),
            arkret::DidCoreId::new("did:webvh:z6mkfixture:principal-server.example".to_owned())
                .unwrap(),
            1,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            payload,
        )
        .unwrap();
        serde_json::to_value(event).unwrap()
    }

    #[test]
    fn extracts_text_message() {
        let event = message_event("did:webvh:example.org:user-alice", "hello");
        let parsed = extract_message_event(&event, "support").expect("parse");
        assert_eq!(parsed.body, "hello");
        assert_eq!(parsed.realm_id, REALM_1);
        assert_eq!(parsed.strand_id.as_deref(), Some(STRAND_1));
        assert_eq!(parsed.sender_did, "did:webvh:example.org:user-alice");
    }

    #[test]
    fn extracts_sdk_standard_text_message() {
        let event = sdk_message_event("did:webvh:z6mkfixture:alice.example", "hello sdk");
        let parsed = extract_message_event(&event, "support").expect("parse");
        assert_eq!(parsed.body, "hello sdk");
        assert_eq!(parsed.strand_id.as_deref(), Some(STRAND_1));
        assert_eq!(parsed.sender_did, "did:webvh:z6mkfixture:alice.example");
    }

    #[test]
    fn preserves_structured_agent_mention_targets_for_gateway_routing() {
        let agent_id = "did:webvh:example.org:agents:support";
        let event = event_value(
            "ak.message.create",
            REALM_1,
            "did:webvh:z6mkfixture:alice.example",
            json!({
                "strand_id": STRAND_1,
                "track_name": "discussion",
                "content": {
                    "kind": "ak.content.text",
                    "body": "hello support",
                    "mentions": [
                        {"kind": "mention", "subject_id": agent_id},
                        {"kind": "mention", "subject_id": agent_id}
                    ]
                }
            }),
        );

        let parsed = extract_message_event(&event, "support").expect("parse");

        assert_eq!(parsed.mentioned_actor_ids, vec![agent_id]);
    }

    #[test]
    fn ignores_non_message_kind() {
        let event = event_value(
            "ak.strand.update",
            REALM_1,
            "did:webvh:z6mkfixture:alice.example",
            json!({}),
        );
        assert!(extract_message_event(&event, "x").is_none());
    }

    #[test]
    fn ignores_non_text_content() {
        let actor = "did:webvh:z6mkfixture:alice.example";
        let event = event_value(
            "ak.message.create",
            REALM_1,
            actor,
            json!({
                "strand_id": STRAND_1,
                "track_name": "discussion",
                "content": {"kind":"ak.content.image","body":""}
            }),
        );
        assert!(extract_message_event(&event, "x").is_none());
        let ArkretInboundEventOutcome::Skip(skipped) = classify_message_event(&event, "x") else {
            panic!("expected skip");
        };
        assert_eq!(skipped.account_id, "x");
        assert_eq!(skipped.realm_id.as_deref(), Some(REALM_1));
        assert_eq!(skipped.sender_did.as_deref(), Some(actor));
        assert_eq!(
            skipped.reason,
            ArkretInboundSkipReason::UnsupportedContentKind("ak.content.image".into())
        );
    }

    #[test]
    fn classifies_encrypted_content_block() {
        let event = event_value(
            "ak.message.create",
            REALM_1,
            "did:webvh:z6mkfixture:alice.example",
            json!({
                "strand_id": STRAND_1,
                "track_name": "discussion",
                "content": {"kind":"ak.content.encrypted","body":""}
            }),
        );
        let ArkretInboundEventOutcome::Skip(skipped) = classify_message_event(&event, "x") else {
            panic!("expected skip");
        };
        assert_eq!(skipped.reason, ArkretInboundSkipReason::EncryptedContent);
    }

    #[test]
    fn classifies_spec_encrypted_content_carrier() {
        let event = event_value(
            "ak.message.create",
            REALM_1,
            "did:webvh:z6mkfixture:alice.example",
            json!({
                "strand_id": STRAND_1,
                "track_name": "discussion",
                "encrypted_content":{
                    "scheme":"mls_rfc9420",
                    "ciphertext":"..."
                }
            }),
        );
        let ArkretInboundEventOutcome::Skip(skipped) = classify_message_event(&event, "x") else {
            panic!("expected skip");
        };
        assert_eq!(skipped.reason, ArkretInboundSkipReason::EncryptedContent);
    }

    #[test]
    fn should_dispatch_filters_self_messages() {
        let account = make_account(Some(REALM_1));
        let event = message_event(&account.principal_id, "echo");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn should_dispatch_accepts_any_realm_for_user_agent() {
        let account = make_account(Some(REALM_2));
        let event = message_event("did:webvh:z6mkfixture:bob.example", "hi");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn should_dispatch_filters_empty_body() {
        let account = make_account(None);
        let event = message_event("did:webvh:z6mkfixture:bob.example", "   ");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn should_dispatch_filters_missing_event_read_scope() {
        let mut account = make_account(Some(REALM_1));
        account.requested_scope = vec!["ak.self.events.stream.subscribe".into()];
        let event = message_event("did:webvh:z6mkfixture:bob.example", "secret");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");

        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn parse_delta_frame_walks_realms() {
        let account = make_account(Some(REALM_1));
        let realms = json!({
            REALM_1: {
                "timeline": {
                    "events": [
                        message_event("did:webvh:z6mkfixture:bob.example", "hello bob"),
                        message_event(&account.principal_id, "self echo"),
                        message_event("did:webvh:z6mkfixture:carol.example", "")
                    ]
                }
            },
            REALM_2: {
                "timeline": {
                    "events": [message_event_in_realm(
                        REALM_2,
                        "did:webvh:z6mkfixture:dan.example",
                        STRAND_2,
                        "from other realm"
                    )]
                }
            }
        });
        let result = parse_delta_frame_for_account(&realms, &account);
        assert_eq!(result.events.len(), 2);
        assert!(result.events.iter().any(|event| {
            event.sender_did == "did:webvh:z6mkfixture:bob.example" && event.body == "hello bob"
        }));
        assert!(result.events.iter().any(|event| {
            event.sender_did == "did:webvh:z6mkfixture:dan.example"
                && event.body == "from other realm"
        }));
        assert_eq!(result.skipped.len(), 2);
        assert!(
            result
                .skipped
                .iter()
                .any(|event| event.reason == ArkretInboundSkipReason::LoopbackFromAccount)
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|event| event.reason == ArkretInboundSkipReason::EmptyBody)
        );
    }

    #[test]
    fn parse_delta_without_event_read_does_not_dispatch_plaintext() {
        let mut account = make_account(Some(REALM_1));
        account.requested_scope = vec!["ak.self.events.stream.subscribe".into()];
        let realms = json!({
            REALM_1: {
                "timeline": {
                    "events": [message_event("did:webvh:z6mkfixture:bob.example", "plain text")]
                }
            }
        });

        let result = parse_delta_frame_for_account(&realms, &account);

        assert!(result.events.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(
            result.skipped[0].reason,
            ArkretInboundSkipReason::ContentReadNotGranted
        );
    }

    #[test]
    fn parse_delta_records_encrypted_skips() {
        let account = make_account(None);
        let encrypted_event = event_value(
            "ak.message.create",
            REALM_1,
            "did:webvh:z6mkfixture:bob.example",
            json!({
                "strand_id": STRAND_1,
                "track_name": "discussion",
                "encrypted_content":{"scheme":"mls_rfc9420","ciphertext":"..."}
            }),
        );
        let realms = json!({
            REALM_1: {
                "timeline": {
                    "events": [encrypted_event]
                }
            }
        });
        let result = parse_delta_frame_for_account(&realms, &account);
        assert!(result.events.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(
            result.skipped[0].reason,
            ArkretInboundSkipReason::EncryptedContent
        );
    }

    #[test]
    fn parse_notification_delta_dispatches_assignment() {
        let account = make_account(Some("ak:realm:r1"));
        let notifications = json!({
            "events": [{
                "notification_id": "ak:notification:n1",
                "notification_kind": "assignment",
                "realm_id": "ak:realm:r1",
                "strand_id": "ak:strand:s1",
                "source_event_id": "ak:event:rel1",
                "source_actor_id": "did:webvh:example.org:user-alice",
                "body": "You were assigned to a Strand."
            }]
        });

        let result = parse_notification_delta_for_account(&notifications, &account);

        assert_eq!(result.skipped, Vec::new());
        assert_eq!(result.events.len(), 1);
        let event = &result.events[0];
        assert_eq!(event.event_id, "ak:notification:n1");
        assert_eq!(event.realm_id, "ak:realm:r1");
        assert_eq!(
            event.sender_did,
            "did:webvh:example.org:user-alice".to_owned()
        );
        assert_eq!(event.strand_id.as_deref(), Some("ak:strand:s1"));
        assert_eq!(event.thread_root_id, None);
        assert!(event.body.contains("assignment notification"));
        assert!(event.body.contains("source_event_id: ak:event:rel1"));
    }

    #[test]
    fn parse_notification_delta_dispatches_schedule_array_shape() {
        let account = make_account(None);
        let notifications = json!([{
            "id": "ak:notification:n2",
            "kind": "schedule",
            "realm_id": "ak:realm:r1",
            "source_ref": "ak:strand:s2",
            "source_event_id": "ak:event:update1",
            "sender": "did:webvh:example.org:user-bob"
        }]);

        let result = parse_notification_delta_for_account(&notifications, &account);

        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].event_id, "ak:notification:n2");
        assert_eq!(result.events[0].strand_id.as_deref(), Some("ak:strand:s2"));
        assert_eq!(result.events[0].thread_root_id, None);
        assert!(result.events[0].body.contains("schedule notification"));
    }

    #[test]
    fn parse_notification_delta_skips_unsupported_kind() {
        let account = make_account(None);
        let notifications = json!({
            "events": [{
                "notification_id": "ak:notification:n3",
                "notification_kind": "mention",
                "realm_id": "ak:realm:r1",
                "source_event_id": "ak:event:msg1"
            }]
        });

        let result = parse_notification_delta_for_account(&notifications, &account);

        assert!(result.events.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(
            result.skipped[0].reason,
            ArkretInboundSkipReason::UnsupportedNotificationKind("mention".to_owned())
        );
    }

    #[test]
    fn parse_notification_delta_accepts_any_realm_for_user_agent() {
        let account = make_account(Some("ak:realm:allowed"));
        let notifications = json!({
            "events": [{
                "notification_id": "ak:notification:n4",
                "notification_kind": "assignment",
                "realm_id": "ak:realm:other",
                "strand_id": "ak:strand:s4",
                "source_event_id": "ak:event:rel4"
            }]
        });

        let result = parse_notification_delta_for_account(&notifications, &account);

        assert_eq!(result.events.len(), 1);
        assert!(result.skipped.is_empty());
        assert_eq!(result.events[0].realm_id, "ak:realm:other");
    }
}
