//! Agent Sidecar exchange typed-binding consumption & production.
//!
//! Normative source: `arkret-spec/spec/v1/zh/models/sidecar.md` §7.2.1–§7.2.2.
//! The runtime obtains exchange identity **only** from the decrypted
//! `role=request` Event binding; it must never be inferred from `reply_to`,
//! message bodies or arrival order. Every parse/validation failure fails
//! closed: the Event is handled as an ordinary private message and never as an
//! exchange participant.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use arkret::{
    AgentSidecarEventExchangeBinding, AgentSidecarExchangeBindingRole, AgentSidecarExchangeId,
    EventId, MessageMetadata,
};
use serde_json::Value;

use super::account_store::safe_file_stem;
use super::crypto_state::account_scope_id;

/// Event `refs[].role` used to causally order exchange Events after the
/// request Event (`zh/models/sidecar.md` §7.2.1 items 2–3).
pub const EVENT_REF_ROLE_AFTER: &str = "after";

/// Verified exchange identity carried from the inbound request Event to the
/// reply pipeline. Exchange/request identity originates from the accepted
/// request Event; the optional assignment id is present only when the same
/// verified Event establishes this runtime as coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarExchangeContext {
    pub exchange_id: String,
    pub request_event_id: String,
    /// Initial or reassigned coordinator assignment Event id when this
    /// runtime is the verified coordinator. `None` keeps the reply visible
    /// without requesting terminal close.
    pub coordinator_assignment_event_id: Option<String>,
}

/// Fail-closed extraction of the exchange binding from decrypted
/// `encrypted_metadata` plaintext. Any non-object plaintext, missing key,
/// schema mismatch, unknown role or field-validation failure yields `None`
/// and the message is treated as an ordinary private message.
#[must_use]
pub fn sidecar_binding_from_metadata_plaintext(
    plaintext: &Value,
) -> Option<AgentSidecarEventExchangeBinding> {
    let metadata: MessageMetadata = serde_json::from_value(plaintext.clone()).ok()?;
    metadata.sidecar_exchange_binding()
}

/// Outcome of the runtime-side consumption gate for one decrypted binding
/// (`zh/models/sidecar.md` §7.2.2). Idempotency (`exchange_id` already
/// consumed / mapped to a different Event id) is checked separately via
/// [`SidecarExchangeStore::admit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarRequestGate {
    /// Not a `role=request` binding (or the binding fails closed validation):
    /// the Event is not an executable Sidecar request. Response/internal
    /// bindings produced by other Agents also land here.
    NotARequest,
    /// A valid request binding whose `addressed_agent_ids` does not contain
    /// this runtime's principal. The request must be treated as nonexistent:
    /// no execution, no shared error (§7.2.2).
    NotAddressed,
    /// A valid request binding addressed to this runtime principal.
    Addressed(SidecarExchangeContext),
}

/// Apply the §7.2.2 pre-execution checks that are decidable from the binding
/// alone: `role == request` with a valid closed `request_context`, and this
/// runtime's principal being a member of `addressed_agent_ids`.
#[must_use]
pub fn gate_inbound_request_binding(
    binding: &AgentSidecarEventExchangeBinding,
    request_event_id: &str,
    principal_id: &str,
) -> SidecarRequestGate {
    if binding.role != AgentSidecarExchangeBindingRole::Request {
        return SidecarRequestGate::NotARequest;
    }
    // `MessageMetadata::sidecar_exchange_binding` already validated, but the
    // gate re-validates so it stays fail-closed for any caller.
    if binding.validate().is_err() {
        return SidecarRequestGate::NotARequest;
    }
    let Some(request_context) = binding.request_context.as_ref() else {
        return SidecarRequestGate::NotARequest;
    };
    let principal = principal_id.trim();
    let addressed = request_context
        .addressed_agent_ids
        .iter()
        .any(|did| did.as_str() == principal);
    if !addressed {
        return SidecarRequestGate::NotAddressed;
    }
    let coordinator_assignment_event_id = request_context
        .effective_coordinator()
        .ok()
        .filter(|coordinator| coordinator.as_str() == principal)
        .map(|_| request_event_id.to_owned());
    SidecarRequestGate::Addressed(SidecarExchangeContext {
        exchange_id: binding.exchange_id.as_str().to_owned(),
        request_event_id: request_event_id.to_owned(),
        coordinator_assignment_event_id,
    })
}

/// Result of durably observing one scoped Sidecar request identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarExchangeAdmission {
    /// First observation of this scoped exchange id; the identity audit is now
    /// durable. This does not authorize execution.
    Recorded,
    /// The identical mapping already exists (for example after a runtime
    /// restart). It is an exact replay and must not execute again.
    AlreadyObserved,
    /// The scoped exchange id is already mapped to a **different** request
    /// Event id.
    /// §7.2.1: fail closed — never execute a second time for the same
    /// exchange id, even when body or `request_context` differ.
    Conflict { existing_request_event_id: String },
}

/// Durable request-identity audit, one local file per account scope (sibling
/// of the garth account store).
///
/// The normative idempotency domain is
/// `(controller_id, private_strand_id, exchange_id)`. Observing a request is
/// deliberately separate from authorizing or consuming it: callers first
/// preserve the identity here, then apply the complete runtime authorization
/// gate. Writes are serialized per path and use the workspace atomic writer,
/// so concurrent deliveries cannot both observe an empty exchange slot.
#[derive(Debug, Clone)]
pub struct SidecarExchangeStore {
    path: PathBuf,
}

const SIDECAR_EXCHANGE_AUDIT_SCHEMA: &str = "savfox.arkret.sidecar_exchange_audit.v1";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarExchangeFile {
    schema: String,
    #[serde(default)]
    controllers: SidecarExchangeControllers,
}

type SidecarExchangeControllers =
    BTreeMap<String, BTreeMap<String, BTreeMap<String, SidecarExchangeRecord>>>;

impl Default for SidecarExchangeFile {
    fn default() -> Self {
        Self {
            schema: SIDECAR_EXCHANGE_AUDIT_SCHEMA.to_owned(),
            controllers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarExchangeRecord {
    request_event_id: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    conflicting_request_event_ids: BTreeSet<String>,
}

fn sidecar_exchange_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("Sidecar exchange lock registry");
    Arc::clone(
        locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

impl SidecarExchangeStore {
    #[must_use]
    pub fn for_account(savfox_home: &Path, channel_id: &str, account_id: &str) -> Self {
        let scope_id = account_scope_id(channel_id, account_id);
        let path = savfox_home
            .join("gateway")
            .join("arkret-account-state")
            .join(format!(
                "{}-sidecar-exchange-audit-v1.json",
                safe_file_stem(&scope_id)
            ));
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Validate and record one request identity in the normative scoped
    /// idempotency domain. This method never grants permission to execute.
    pub fn record_request_identity(
        &self,
        controller_id: &str,
        private_strand_id: &str,
        exchange_id: &str,
        request_event_id: &str,
    ) -> anyhow::Result<SidecarExchangeAdmission> {
        let controller_id = arkret::Did::new(controller_id.to_owned())?;
        let private_strand_id = arkret::StrandId::new(private_strand_id.to_owned())?;
        let exchange_id = AgentSidecarExchangeId::new(exchange_id.to_owned())?;
        let request_event_id = EventId::new(request_event_id.to_owned())?;

        let lock = sidecar_exchange_lock(&self.path);
        let _guard = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Sidecar exchange audit lock poisoned"))?;
        let mut state = self.load()?;
        let exchanges = state
            .controllers
            .entry(controller_id.to_string())
            .or_default()
            .entry(private_strand_id.to_string())
            .or_default();
        if let Some(existing) = exchanges.get_mut(exchange_id.as_str()) {
            if existing.request_event_id == request_event_id.as_str() {
                return Ok(SidecarExchangeAdmission::AlreadyObserved);
            }
            existing
                .conflicting_request_event_ids
                .insert(request_event_id.to_string());
            let existing_request_event_id = existing.request_event_id.clone();
            self.save(&state)?;
            return Ok(SidecarExchangeAdmission::Conflict {
                existing_request_event_id,
            });
        }
        exchanges.insert(
            exchange_id.to_string(),
            SidecarExchangeRecord {
                request_event_id: request_event_id.to_string(),
                conflicting_request_event_ids: BTreeSet::new(),
            },
        );
        self.save(&state)?;
        Ok(SidecarExchangeAdmission::Recorded)
    }

    fn load(&self) -> anyhow::Result<SidecarExchangeFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(SidecarExchangeFile::default()),
            Ok(bytes) => {
                let state: SidecarExchangeFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", self.path.display()))?;
                anyhow::ensure!(
                    state.schema == SIDECAR_EXCHANGE_AUDIT_SCHEMA,
                    "unsupported Sidecar exchange audit schema '{}'",
                    state.schema
                );
                Ok(state)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(SidecarExchangeFile::default())
            }
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }

    fn save(&self, state: &SidecarExchangeFile) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        savfox_utils::fs::write_atomically(&self.path, &bytes, Some(0o600))
            .with_context(|| format!("write {}", self.path.display()))
    }
}

const REPLY_TARGET_EXCHANGE_KEY: &str = "|sidecar_exchange=";
const REPLY_TARGET_REQUEST_KEY: &str = "|request_event=";
const REPLY_TARGET_COORDINATOR_ASSIGNMENT_KEY: &str = "|coordinator_assignment=";

/// Encode the verified exchange context into the channel-runtime
/// `reply_target` string so it travels with the message that started the
/// pipeline. The strand id stays the prefix so existing
/// `starts_with("ak:strand:")` routing keeps working. `|` cannot appear in a
/// strand id, an exchange id (`[A-Za-z0-9._~=-]`) or an Event id.
#[must_use]
pub fn encode_sidecar_reply_target(strand_id: &str, context: &SidecarExchangeContext) -> String {
    let mut encoded = format!(
        "{strand_id}{REPLY_TARGET_EXCHANGE_KEY}{}{REPLY_TARGET_REQUEST_KEY}{}",
        context.exchange_id, context.request_event_id
    );
    if let Some(assignment) = context.coordinator_assignment_event_id.as_deref() {
        encoded.push_str(REPLY_TARGET_COORDINATOR_ASSIGNMENT_KEY);
        encoded.push_str(assignment);
    }
    encoded
}

/// Split a `reply_target` produced by [`encode_sidecar_reply_target`] back
/// into `(strand_id, Option<context>)`. Plain strand ids and malformed
/// suffixes fail closed to `(strand_id, None)`.
#[must_use]
pub fn split_sidecar_reply_target(value: &str) -> (String, Option<SidecarExchangeContext>) {
    let Some((strand_id, rest)) = value.split_once(REPLY_TARGET_EXCHANGE_KEY) else {
        return (value.to_owned(), None);
    };
    let Some((exchange_id, request_and_assignment)) = rest.split_once(REPLY_TARGET_REQUEST_KEY)
    else {
        return (strand_id.to_owned(), None);
    };
    let (request_event_id, coordinator_assignment_event_id) =
        match request_and_assignment.split_once(REPLY_TARGET_COORDINATOR_ASSIGNMENT_KEY) {
            Some((request_event_id, assignment_event_id))
                if !request_event_id.is_empty() && !assignment_event_id.is_empty() =>
            {
                (request_event_id, Some(assignment_event_id.to_owned()))
            }
            Some(_) => return (strand_id.to_owned(), None),
            None => (request_and_assignment, None),
        };
    if exchange_id.is_empty() || request_event_id.is_empty() {
        return (strand_id.to_owned(), None);
    }
    (
        strand_id.to_owned(),
        Some(SidecarExchangeContext {
            exchange_id: exchange_id.to_owned(),
            request_event_id: request_event_id.to_owned(),
            coordinator_assignment_event_id,
        }),
    )
}

/// Build the `MessageMetadata` plaintext (JSON) carrying the
/// `role=user_facing_response` binding for the Agent's user-visible reply
/// (`zh/models/sidecar.md` §7.2.1 item 2).
///
/// This runtime only delivers the final user-visible reply to Arkret;
/// intermediate/tool output never reaches the Arkret channel, so
/// `role=internal` bindings are intentionally not produced here. If such
/// messages are ever sent to Arkret they MUST carry `role=internal` with the
/// same `exchange_id`/`request_event_id` (§7.2.1 item 3).
///
/// Callers MUST encrypt the returned value into `encrypted_metadata` with the
/// same MLS group as `encrypted_content`; the binding must never appear in
/// plaintext `metadata` (forbidden-wire-fields `sidecar_exchange_binding`).
pub fn build_user_facing_response_metadata(
    context: &SidecarExchangeContext,
) -> anyhow::Result<MessageMetadata> {
    let exchange_id = AgentSidecarExchangeId::new(context.exchange_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid Sidecar exchange id: {err}"))?;
    let request_event_id = EventId::new(context.request_event_id.clone())
        .map_err(|err| anyhow::anyhow!("invalid Sidecar request Event id: {err}"))?;
    let mut binding =
        AgentSidecarEventExchangeBinding::user_facing_response(exchange_id, request_event_id)
            .map_err(|err| anyhow::anyhow!("build user_facing_response binding: {err}"))?;
    if let Some(assignment_event_id) = context.coordinator_assignment_event_id.as_deref() {
        binding = binding
            .with_completion(EventId::new(assignment_event_id.to_owned()).map_err(|err| {
                anyhow::anyhow!("invalid Sidecar coordinator assignment Event id: {err}")
            })?)
            .map_err(|err| anyhow::anyhow!("attach Sidecar completion request: {err}"))?;
    }
    let mut metadata = MessageMetadata::default();
    metadata
        .set_sidecar_exchange_binding(&binding)
        .map_err(|err| anyhow::anyhow!("mount Sidecar exchange binding: {err}"))?;
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use arkret::{
        AgentSidecarExchangeCompletionPolicy, AgentSidecarExchangeRequestContext,
        AgentSidecarSourceTrackRef, Did, Hlc, NonEmptyString, RealmId, StrandId,
    };
    use serde_json::json;

    use super::*;

    const AGENT_DID: &str = "did:webvh:example.org:agents:support";
    const OTHER_DID: &str = "did:webvh:example.org:agents:other";
    const EXCHANGE_ID: &str = "01904100-0000-7000-8000-0000000000aa";
    const REQUEST_EVENT_ID: &str = "ak:event:01904100-0000-7000-8000-000000000031";
    const REALM_ID: &str = "ak:realm:01904100-0000-7000-8000-000000000001";
    const STRAND_ID: &str = "ak:strand:01904100-0000-7000-8000-000000000011";

    fn request_context(addressed: &[&str]) -> AgentSidecarExchangeRequestContext {
        AgentSidecarExchangeRequestContext {
            source_track_ref: AgentSidecarSourceTrackRef {
                realm_id: RealmId::new(REALM_ID).unwrap(),
                strand_id: StrandId::new(STRAND_ID).unwrap(),
                track_name: "discussion".to_owned(),
            },
            source_hlc: Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            client_order_key: NonEmptyString::new("device-1-1").unwrap(),
            addressed_agent_ids: addressed
                .iter()
                .map(|did| Did::new((*did).to_owned()).unwrap())
                .collect(),
            completion_policy: AgentSidecarExchangeCompletionPolicy::Coordinator,
            coordinator_agent_id: (addressed.len() > 1)
                .then(|| Did::new(addressed[0].to_owned()).unwrap()),
            source_frontier_anchor: None,
        }
    }

    fn request_binding(addressed: &[&str]) -> AgentSidecarEventExchangeBinding {
        AgentSidecarEventExchangeBinding::request(
            AgentSidecarExchangeId::new(EXCHANGE_ID).unwrap(),
            request_context(addressed),
        )
        .unwrap()
    }

    fn metadata_plaintext_with_binding(binding: &AgentSidecarEventExchangeBinding) -> Value {
        let mut metadata = MessageMetadata::default();
        metadata.set_sidecar_exchange_binding(binding).unwrap();
        serde_json::to_value(&metadata).unwrap()
    }

    #[test]
    fn binding_parse_fails_closed_without_key() {
        assert_eq!(sidecar_binding_from_metadata_plaintext(&json!({})), None);
        assert_eq!(
            sidecar_binding_from_metadata_plaintext(&json!({"fields": {}})),
            None
        );
        assert_eq!(
            sidecar_binding_from_metadata_plaintext(&json!("not an object")),
            None
        );
    }

    #[test]
    fn binding_parse_fails_closed_on_bad_schema_or_unknown_role() {
        let bad_schema = json!({
            "sidecar_exchange_binding": {
                "schema": "ak.schema.agent_sidecar_event_exchange_binding.v99",
                "exchange_id": EXCHANGE_ID,
                "role": "request"
            }
        });
        assert_eq!(sidecar_binding_from_metadata_plaintext(&bad_schema), None);

        let unknown_role = json!({
            "sidecar_exchange_binding": {
                "schema": "ak.schema.agent_sidecar_event_exchange_binding.v1",
                "exchange_id": EXCHANGE_ID,
                "role": "supervisor",
                "request_event_id": REQUEST_EVENT_ID
            }
        });
        assert_eq!(sidecar_binding_from_metadata_plaintext(&unknown_role), None);

        // role=request without request_context fails closed validation.
        let missing_context = json!({
            "sidecar_exchange_binding": {
                "schema": "ak.schema.agent_sidecar_event_exchange_binding.v1",
                "exchange_id": EXCHANGE_ID,
                "role": "request"
            }
        });
        assert_eq!(
            sidecar_binding_from_metadata_plaintext(&missing_context),
            None
        );
    }

    #[test]
    fn binding_parse_accepts_valid_request_binding() {
        let plaintext = metadata_plaintext_with_binding(&request_binding(&[AGENT_DID]));
        let binding = sidecar_binding_from_metadata_plaintext(&plaintext).expect("binding");
        assert_eq!(binding.role, AgentSidecarExchangeBindingRole::Request);
        assert_eq!(binding.exchange_id.as_str(), EXCHANGE_ID);
    }

    #[test]
    fn gate_rejects_non_addressed_principal() {
        let binding = request_binding(&[OTHER_DID]);
        assert_eq!(
            gate_inbound_request_binding(&binding, REQUEST_EVENT_ID, AGENT_DID),
            SidecarRequestGate::NotAddressed
        );
    }

    #[test]
    fn gate_admits_addressed_principal_with_request_event_identity() {
        let binding = request_binding(&[OTHER_DID, AGENT_DID]);
        let SidecarRequestGate::Addressed(context) =
            gate_inbound_request_binding(&binding, REQUEST_EVENT_ID, AGENT_DID)
        else {
            panic!("expected addressed gate");
        };
        assert_eq!(context.exchange_id, EXCHANGE_ID);
        assert_eq!(context.request_event_id, REQUEST_EVENT_ID);
        assert_eq!(context.coordinator_assignment_event_id, None);
        let SidecarRequestGate::Addressed(coordinator_context) = gate_inbound_request_binding(
            &request_binding(&[AGENT_DID]),
            REQUEST_EVENT_ID,
            AGENT_DID,
        ) else {
            panic!("expected coordinator gate");
        };
        assert_eq!(
            coordinator_context
                .coordinator_assignment_event_id
                .as_deref(),
            Some(REQUEST_EVENT_ID)
        );
        assert_eq!(
            gate_inbound_request_binding(
                &request_binding(&[AGENT_DID]),
                REQUEST_EVENT_ID,
                &AGENT_DID.to_ascii_uppercase(),
            ),
            SidecarRequestGate::NotAddressed,
            "principal identity matching is bit-identical, never case-folded"
        );
    }

    #[test]
    fn gate_treats_response_bindings_as_non_requests() {
        let binding = AgentSidecarEventExchangeBinding::user_facing_response(
            AgentSidecarExchangeId::new(EXCHANGE_ID).unwrap(),
            EventId::new(REQUEST_EVENT_ID.to_owned()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            gate_inbound_request_binding(&binding, REQUEST_EVENT_ID, AGENT_DID),
            SidecarRequestGate::NotARequest
        );
    }

    #[test]
    fn exchange_store_is_idempotent_and_fail_closed_across_reopen() {
        let home = std::env::temp_dir().join(format!(
            "savfox-sidecar-exchange-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let store = SidecarExchangeStore::for_account(&home, "c1", "a1");
        assert_eq!(
            store
                .record_request_identity(OTHER_DID, STRAND_ID, EXCHANGE_ID, REQUEST_EVENT_ID)
                .unwrap(),
            SidecarExchangeAdmission::Recorded
        );
        assert_eq!(
            store
                .record_request_identity(OTHER_DID, STRAND_ID, EXCHANGE_ID, REQUEST_EVENT_ID)
                .unwrap(),
            SidecarExchangeAdmission::AlreadyObserved
        );

        // Restart replay: a fresh store handle over the same file must still
        // report the exchange as observed.
        let reopened = SidecarExchangeStore::for_account(&home, "c1", "a1");
        assert_eq!(
            reopened
                .record_request_identity(OTHER_DID, STRAND_ID, EXCHANGE_ID, REQUEST_EVENT_ID)
                .unwrap(),
            SidecarExchangeAdmission::AlreadyObserved
        );

        // Same exchange id with a different request Event id: fail closed.
        let other_event = "ak:event:01904100-0000-7000-8000-000000000032";
        assert_eq!(
            reopened
                .record_request_identity(OTHER_DID, STRAND_ID, EXCHANGE_ID, other_event)
                .unwrap(),
            SidecarExchangeAdmission::Conflict {
                existing_request_event_id: REQUEST_EVENT_ID.to_owned(),
            }
        );

        // The same exchange id in another normative scope is independent.
        let other_strand = "ak:strand:01904100-0000-7000-8000-000000000012";
        assert_eq!(
            reopened
                .record_request_identity(OTHER_DID, other_strand, EXCHANGE_ID, other_event)
                .unwrap(),
            SidecarExchangeAdmission::Recorded
        );
        assert_eq!(
            reopened
                .record_request_identity(AGENT_DID, STRAND_ID, EXCHANGE_ID, other_event)
                .unwrap(),
            SidecarExchangeAdmission::Recorded
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn exchange_store_serializes_concurrent_first_observation() {
        let home = std::env::temp_dir().join(format!(
            "savfox-sidecar-exchange-concurrent-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let store = SidecarExchangeStore::for_account(&home, "c1", "a1");
        let handles = (0..8)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .record_request_identity(
                            OTHER_DID,
                            STRAND_ID,
                            EXCHANGE_ID,
                            REQUEST_EVENT_ID,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == SidecarExchangeAdmission::Recorded)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == SidecarExchangeAdmission::AlreadyObserved)
                .count(),
            7
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reply_target_round_trips_and_fails_closed() {
        let context = SidecarExchangeContext {
            exchange_id: EXCHANGE_ID.to_owned(),
            request_event_id: REQUEST_EVENT_ID.to_owned(),
            coordinator_assignment_event_id: Some(REQUEST_EVENT_ID.to_owned()),
        };
        let encoded = encode_sidecar_reply_target(STRAND_ID, &context);
        assert!(encoded.starts_with("ak:strand:"));
        let (strand, parsed) = split_sidecar_reply_target(&encoded);
        assert_eq!(strand, STRAND_ID);
        assert_eq!(parsed, Some(context));

        assert_eq!(
            split_sidecar_reply_target(STRAND_ID),
            (STRAND_ID.to_owned(), None)
        );
        let truncated = format!("{STRAND_ID}{REPLY_TARGET_EXCHANGE_KEY}{EXCHANGE_ID}");
        assert_eq!(
            split_sidecar_reply_target(&truncated),
            (STRAND_ID.to_owned(), None)
        );
    }

    #[test]
    fn user_facing_response_metadata_carries_binding_and_validates() {
        let context = SidecarExchangeContext {
            exchange_id: EXCHANGE_ID.to_owned(),
            request_event_id: REQUEST_EVENT_ID.to_owned(),
            coordinator_assignment_event_id: Some(REQUEST_EVENT_ID.to_owned()),
        };
        let plaintext = build_user_facing_response_metadata(&context).expect("metadata");
        let plaintext = serde_json::to_value(plaintext).expect("serialize metadata");
        let binding = sidecar_binding_from_metadata_plaintext(&plaintext).expect("binding");
        assert_eq!(
            binding.role,
            AgentSidecarExchangeBindingRole::UserFacingResponse
        );
        assert_eq!(binding.exchange_id.as_str(), EXCHANGE_ID);
        assert_eq!(
            binding.request_event_id.as_ref().map(|id| id.as_str()),
            Some(REQUEST_EVENT_ID)
        );
        assert_eq!(binding.completes_exchange, Some(true));
        assert_eq!(
            binding
                .coordinator_assignment_event_id
                .as_ref()
                .map(EventId::as_str),
            Some(REQUEST_EVENT_ID)
        );
    }

    #[test]
    fn user_facing_response_metadata_rejects_invalid_identity() {
        let bad = SidecarExchangeContext {
            exchange_id: "short".to_owned(),
            request_event_id: REQUEST_EVENT_ID.to_owned(),
            coordinator_assignment_event_id: None,
        };
        assert!(build_user_facing_response_metadata(&bad).is_err());
    }
}
