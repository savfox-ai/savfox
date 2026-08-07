use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use arkret::{AgentPairingBootstrap, DeviceId, Did, DidUrl};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::signer::{ArkretKeyRef, ed25519_runtime_public_key, load_ed25519_signing_key};

pub const DEFAULT_AGENT_RUNTIME_SCOPE: &[&str] = &[
    "ak.self.events.stream.subscribe",
    "ak.self.events.read.scan",
    "ak.self.events.read.frontier",
    "ak.self.events.command.submit",
    "ak.self.keys.keypackages.upload.create",
    "ak.self.keys.keypackages.command.consume",
    "ak.self.keys.keypackages.command.revoke",
    "ak.self.device_messages.query.list",
    "ak.self.device_messages.command.ack",
    "ak.self.signal.command.send",
    "ak.event.read",
    "ak.message.create",
];

const REQUIRED_LISTEN_SCOPE: &[&str] = &[
    "ak.self.events.stream.subscribe",
    "ak.self.events.read.scan",
    "ak.self.events.read.frontier",
    "ak.self.keys.keypackages.upload.create",
    "ak.self.keys.keypackages.command.consume",
    "ak.self.keys.keypackages.command.revoke",
    "ak.self.device_messages.query.list",
    "ak.self.device_messages.command.ack",
    "ak.self.signal.command.send",
    "ak.event.read",
];

const REQUIRED_SEND_SCOPE: &[&str] = &["ak.self.events.command.submit", "ak.message.create"];
const VERIFIED_SCOPE_SCHEMA: &str = "savfox.arkret.verified_runtime_scope.v1";

/// Preserve persisted pre-read-API scope commitments while comparing them by
/// their current service-operation names. The original spelling must remain
/// on the wire because it is part of the immutable pairing authorization.
fn canonical_requested_scope_action(action: &str) -> &str {
    match action {
        "ak.self.events.query.scan" => "ak.self.events.read.scan",
        "ak.self.events.query.frontier" => "ak.self.events.read.frontier",
        _ => action,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedArkretRuntimeScope {
    pub schema: String,
    pub channel_id: String,
    pub account_id: String,
    pub principal_id: String,
    pub authorization_ref: String,
    pub runtime_public_key_digest: String,
    pub actions: Vec<String>,
    pub verified_at: DateTime<Utc>,
}

impl VerifiedArkretRuntimeScope {
    #[must_use]
    pub fn permits(&self, requested: &[String]) -> bool {
        requested.iter().all(|action| {
            let action = canonical_requested_scope_action(action);
            self.actions
                .iter()
                .any(|allowed| canonical_requested_scope_action(allowed) == action)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArkretAccountMode {
    /// CKP-0008 personal agent runtime: Inkson bootstrap, local runtime key,
    /// agent_key_proof session grant, and DPoP-bound self endpoints.
    Agent,
}

impl ArkretAccountMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArkretAccountConfig {
    pub mode: ArkretAccountMode,
    pub id: String,
    pub principal_id: String,
    pub device_id: String,
    /// Local Ed25519 runtime key reference. In personal-agent mode this is the
    /// savfox-owned runtime key authorized by Inkson/controller.
    pub key_ref: Option<ArkretKeyRef>,
    /// Authorized runtime verification method id for agent_key_proof and event
    /// signing.
    pub verification_method: Option<String>,
    /// Inkson pairing/bootstrap metadata consumed by savfox. This never
    /// contains a private key.
    pub inkson_bootstrap: Option<AgentPairingBootstrap>,
    /// Durable `ak.agent.key.authorize` reference proving the runtime key has
    /// been approved by the controller.
    pub authorized_event_ref: Option<String>,
    /// DID of the controller that owns this Agent principal.
    ///
    /// A Native Personal Agent has exactly one controller and that ownership
    /// is immutable provisioning state (`zh/models/sidecar.md` §4), so it
    /// belongs with the other immutable runtime identity facts here rather
    /// than being re-derived per Event. It is required by the Sidecar
    /// consumption gate: §7.2.1 makes a `role=request` binding carried by a
    /// non-controller actor wholly invalid, and without this value the gate
    /// fails closed instead of trusting the Event actor.
    pub controller_id: Option<String>,
    /// Requested service/content runtime scope saved as typed list.
    pub requested_scope: Vec<String>,
    pub listen: bool,
    pub send: bool,
}

#[derive(Debug, Clone)]
pub struct ArkretChannelConfig {
    pub id: String,
    pub base_url: String,
    pub service_id: Option<String>,
    /// Projection policy for newly-created conversation bindings. Missing on
    /// legacy configs means `interactive_chat` to preserve existing behavior.
    pub delivery_mode: String,
    pub accounts: Vec<ArkretAccountConfig>,
}

impl ArkretChannelConfig {
    pub fn from_strict_agent_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> anyhow::Result<Self> {
        let raw = config
            .config
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Arkret agent config must be a JSON object"))?;
        let mode = raw
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Arkret agent config is missing mode='agent'"))?;
        if mode != "agent" {
            anyhow::bail!("Arkret agent config mode must be exactly 'agent'");
        }
        const FIELDS: &[&str] = &[
            "mode",
            "inksonBootstrap",
            "keyRef",
            "verificationMethod",
            "authorizedEventRef",
            "controllerId",
            "requestedScope",
            "deliveryMode",
        ];
        let unknown = raw
            .keys()
            .filter(|key| !FIELDS.contains(&key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            anyhow::bail!(
                "Arkret agent config contains unsupported fields: {}",
                unknown.join(", ")
            );
        }
        let mut enabled = config.clone();
        enabled.enabled = true;
        let parsed = Self::from_channel_config(&enabled).ok_or_else(|| {
            anyhow::anyhow!("Arkret agent config must contain a canonical inksonBootstrap object")
        })?;
        if parsed.accounts.len() != 1 {
            anyhow::bail!("Arkret agent config must contain exactly one account");
        }
        let account = &parsed.accounts[0];
        let key_ref = raw
            .get("keyRef")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("Arkret agent keyRef must be an object"))?;
        let key_ref_fields = key_ref.keys().map(String::as_str).collect::<HashSet<_>>();
        let expected_key_ref_fields = HashSet::from(["kind", "service", "account"]);
        if key_ref_fields != expected_key_ref_fields
            || key_ref.get("kind").and_then(Value::as_str) != Some("keyring")
        {
            anyhow::bail!(
                "Arkret agent keyRef must contain exactly kind='keyring', service, and account"
            );
        }
        if key_ref
            .get("service")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(str::is_empty)
            || key_ref
                .get("account")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_none_or(str::is_empty)
        {
            anyhow::bail!("Arkret agent keyRef service and account must be non-empty strings");
        }
        if !matches!(account.key_ref, Some(ArkretKeyRef::Keyring { .. })) {
            anyhow::bail!("Arkret agent keyRef is invalid");
        }
        let duplicates = duplicate_requested_scope_actions(&account.requested_scope);
        if !duplicates.is_empty() {
            anyhow::bail!(
                "requestedScope contains duplicate actions: {}",
                duplicates.join(", ")
            );
        }
        let unknown = unknown_requested_scope_actions(&account.requested_scope)?;
        if !unknown.is_empty() {
            anyhow::bail!(
                "requestedScope contains unknown canonical actions: {}",
                unknown.join(", ")
            );
        }
        let missing =
            missing_required_scope_actions(&account.requested_scope, account.listen, account.send);
        if !missing.is_empty() {
            anyhow::bail!(
                "requestedScope is missing required runtime actions: {}",
                missing.join(", ")
            );
        }
        Ok(parsed)
    }

    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("arkret") {
            return None;
        }

        let raw = config.config.as_object()?;
        if raw.get("mode").and_then(Value::as_str) != Some("agent") {
            return None;
        }
        let bootstrap = parse_inkson_bootstrap(raw.get("inksonBootstrap"));
        let bootstrap = bootstrap?;
        let principal_id = bootstrap.agent_id.to_string();
        let pairing_request_id = bootstrap.pairing_request_id.to_string();
        let device_id = derive_arkret_device_id(&[&principal_id, &pairing_request_id]);
        // A saved channel slot may be paired again with a replacement runtime
        // or a different Agent. Scope all durable account, cursor and crypto
        // state to the concrete paired endpoint so the new binding can never
        // inherit Realm/MLS state from the previous principal.
        let account_id =
            derive_agent_runtime_account_id(&config.id, &principal_id, &pairing_request_id);
        let account = ArkretAccountConfig {
            mode: ArkretAccountMode::Agent,
            id: account_id,
            principal_id: principal_id.clone(),
            device_id,
            key_ref: raw.get("keyRef").and_then(ArkretKeyRef::from_value),
            verification_method: raw
                .get("verificationMethod")
                .and_then(Value::as_str)
                .map(str::to_owned),
            inkson_bootstrap: Some(bootstrap.clone()),
            authorized_event_ref: raw
                .get("authorizedEventRef")
                .and_then(Value::as_str)
                .map(str::to_owned),
            controller_id: parse_controller_id(raw),
            requested_scope: parse_string_list(raw.get("requestedScope")),
            listen: true,
            send: true,
        };

        Some(Self {
            id: config.id.clone(),
            base_url: bootstrap.arkret_base_url,
            service_id: Some(bootstrap.service_id.to_string()),
            delivery_mode: raw
                .get("deliveryMode")
                .and_then(Value::as_str)
                .unwrap_or("interactive_chat")
                .to_owned(),
            accounts: vec![account],
        })
    }

    /// Validate that the channel has at least one usable account.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.base_url.trim().is_empty() {
            anyhow::bail!("Arkret channel '{}' missing base_url", self.id);
        }
        if let Some(service_id) = self.service_id.as_deref() {
            Did::new(service_id.to_owned()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret channel '{}' service_id must be a valid DID URI, got '{}': {err}",
                    self.id,
                    service_id
                )
            })?;
        }
        if self.accounts.is_empty() {
            anyhow::bail!(
                "Arkret channel '{}' has no accounts; configure at least one controlled account",
                self.id
            );
        }
        if !matches!(
            self.delivery_mode.as_str(),
            "interactive_chat" | "task_delivery"
        ) {
            anyhow::bail!(
                "Arkret channel '{}' deliveryMode must be 'interactive_chat' or 'task_delivery'",
                self.id
            );
        }
        for account in &self.accounts {
            account.validate().with_context(|| {
                format!(
                    "Arkret channel '{}' account '{}' is invalid",
                    self.id, account.id
                )
            })?;
        }
        Ok(())
    }

    /// Find an account by id.
    #[must_use]
    pub fn account(&self, account_id: &str) -> Option<&ArkretAccountConfig> {
        self.accounts.iter().find(|a| a.id == account_id)
    }

    /// Pick the account that should send to the given realm.
    ///
    /// Personal-agent accounts are user-scoped, not Realm-scoped, so the
    /// Arkret realm supplied by the incoming event is only the outbound target.
    #[must_use]
    pub fn select_send_account(&self, _realm_id: &str) -> Option<&ArkretAccountConfig> {
        self.accounts.iter().find(|a| a.send)
    }
}

impl ArkretAccountConfig {
    #[must_use]
    pub fn has_requested_scope(&self, action: &str) -> bool {
        let action = canonical_requested_scope_action(action.trim());
        self.requested_scope
            .iter()
            .any(|scope| canonical_requested_scope_action(scope.trim()) == action)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("Arkret account missing id");
        }
        if self.principal_id.trim().is_empty() {
            anyhow::bail!("Arkret account '{}' missing principal_id (DID)", self.id);
        }
        Did::new(self.principal_id.clone()).map_err(|err| {
            anyhow::anyhow!(
                "Arkret account '{}' principal_id must be a valid DID URI, got '{}': {err}",
                self.id,
                self.principal_id
            )
        })?;
        if !self.device_id.trim().is_empty() {
            DeviceId::new(self.device_id.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret account '{}' device_id must be a valid Arkret device id, got '{}': {err}",
                    self.id,
                    self.device_id
                )
            })?;
        }

        self.validate_agent_runtime()
    }

    fn validate_agent_runtime(&self) -> anyhow::Result<()> {
        if self.inkson_bootstrap.is_none() {
            anyhow::bail!(
                "Arkret agent '{}' missing inksonBootstrap; paste the Inkson pairing link or resolved bootstrap instead of a static session grant",
                self.id
            );
        }
        if self.key_ref.is_none() {
            anyhow::bail!(
                "Arkret agent '{}' missing keyRef for the local agent runtime key",
                self.id
            );
        }
        let verification_method = self
            .verification_method
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                "Arkret agent '{}' missing authorized verificationMethod from the completed agent-key pairing",
                self.id
            )
            })?;
        if !verification_method.eq(&format!("{}#{}", self.principal_id, self.device_id)) {
            anyhow::bail!(
                "Arkret agent '{}' verificationMethod must equal '{}#{}' so the authorized Agent runtime key is bound to its stable Signal/MLS endpoint",
                self.id,
                self.principal_id,
                self.device_id
            );
        }
        let authorization_ref = self
            .authorized_event_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Arkret agent '{}' missing authorizedEventRef for ak.agent.key.authorize; pairing must complete before the channel can run",
                    self.id
                )
            })?;
        arkret::EventId::new(authorization_ref.to_owned()).map_err(|error| {
            anyhow::anyhow!(
                "Arkret agent '{}' authorizedEventRef must be a valid Arkret Event id: {error}",
                self.id
            )
        })?;
        if self.requested_scope.is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' missing requestedScope; the canonical runtime scope must be persisted explicitly",
                self.id
            );
        }
        let duplicate_actions = duplicate_requested_scope_actions(&self.requested_scope);
        if !duplicate_actions.is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' requestedScope contains duplicate actions: {}",
                self.id,
                duplicate_actions.join(", ")
            );
        }
        let unknown_actions = unknown_requested_scope_actions(&self.requested_scope)?;
        if !unknown_actions.is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' requestedScope contains unknown canonical actions: {}",
                self.id,
                unknown_actions.join(", ")
            );
        }
        let missing_actions =
            missing_required_scope_actions(&self.requested_scope, self.listen, self.send);
        if !missing_actions.is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' requestedScope is missing required runtime actions: {}",
                self.id,
                missing_actions.join(", ")
            );
        }
        Ok(())
    }
}

#[must_use]
pub fn duplicate_requested_scope_actions(actions: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    for action in actions {
        let canonical = canonical_requested_scope_action(action.as_str());
        if !seen.insert(canonical) && !duplicates.iter().any(|item| item == action) {
            duplicates.push(action.clone());
        }
    }
    duplicates
}

pub fn unknown_requested_scope_actions(actions: &[String]) -> anyhow::Result<Vec<String>> {
    let mut unknown = Vec::new();
    for action in actions {
        let trimmed = action.trim();
        let canonical = canonical_requested_scope_action(trimmed);
        // requestedScope contains both content capabilities and Arkret service
        // operations, so validate against both SDK-owned canonical registries.
        let is_capability_action = arkret_schema::embedded_capability_action(canonical)
            .map_err(|error| anyhow::anyhow!("load Arkret action registry: {error}"))?
            .is_some();
        let is_service_operation = arkret::ServiceOperationId::from_wire(canonical).is_some();
        if trimmed != action
            || canonical.is_empty()
            || (!is_capability_action && !is_service_operation)
        {
            unknown.push(action.clone());
        }
    }
    Ok(unknown)
}

#[must_use]
pub fn missing_required_scope_actions(actions: &[String], listen: bool, send: bool) -> Vec<String> {
    let required = REQUIRED_LISTEN_SCOPE
        .iter()
        .copied()
        .filter(|_| listen)
        .chain(REQUIRED_SEND_SCOPE.iter().copied().filter(|_| send));
    required
        .filter(|required| {
            !actions
                .iter()
                .any(|action| canonical_requested_scope_action(action) == *required)
        })
        .map(str::to_owned)
        .collect()
}

fn verified_scope_path(
    savfox_home: &std::path::Path,
    channel_id: &str,
    account_id: &str,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(channel_id.as_bytes());
    hasher.update([0]);
    hasher.update(account_id.as_bytes());
    let name = format!("{}.json", hex::encode(hasher.finalize()));
    savfox_home
        .join(savfox_utils::home_dir::GATEWAY_SUBDIR)
        .join("arkret-authority")
        .join(name)
}

pub async fn save_verified_runtime_scope(
    savfox_home: &std::path::Path,
    channel_id: &str,
    account: &ArkretAccountConfig,
    runtime_public_key_digest: String,
) -> anyhow::Result<VerifiedArkretRuntimeScope> {
    let authorization_ref = account
        .authorized_event_ref
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing authorizedEventRef"))?
        .to_owned();
    let state = VerifiedArkretRuntimeScope {
        schema: VERIFIED_SCOPE_SCHEMA.to_owned(),
        channel_id: channel_id.to_owned(),
        account_id: account.id.clone(),
        principal_id: account.principal_id.clone(),
        authorization_ref,
        runtime_public_key_digest,
        actions: account.requested_scope.clone(),
        verified_at: Utc::now(),
    };
    let path = verified_scope_path(savfox_home, channel_id, &account.id);
    let bytes = serde_json::to_vec_pretty(&state).context("serialize verified Arkret scope")?;
    savfox_utils::fs::write_atomically_async(&path, bytes, Some(0o600))
        .await
        .with_context(|| format!("persist verified Arkret scope {}", path.display()))?;
    Ok(state)
}

pub async fn load_verified_runtime_scope(
    savfox_home: &std::path::Path,
    channel_id: &str,
    account: &ArkretAccountConfig,
    runtime_public_key_digest: &str,
) -> anyhow::Result<Option<VerifiedArkretRuntimeScope>> {
    let path = verified_scope_path(savfox_home, channel_id, &account.id);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read verified Arkret scope {}", path.display()));
        }
    };
    let state = serde_json::from_slice::<VerifiedArkretRuntimeScope>(&bytes);
    let valid = state.as_ref().is_ok_and(|state| {
        let actions_are_canonical = duplicate_requested_scope_actions(&state.actions).is_empty()
            && unknown_requested_scope_actions(&state.actions)
                .is_ok_and(|unknown| unknown.is_empty());
        state.schema == VERIFIED_SCOPE_SCHEMA
            && state.channel_id == channel_id
            && state.account_id == account.id
            && state.principal_id == account.principal_id
            && account.authorized_event_ref.as_deref() == Some(state.authorization_ref.as_str())
            && state.runtime_public_key_digest == runtime_public_key_digest
            && actions_are_canonical
    });
    if !valid {
        tokio::fs::remove_file(&path)
            .await
            .with_context(|| format!("delete invalid verified Arkret scope {}", path.display()))?;
        return Ok(None);
    }
    Ok(state.ok())
}

pub async fn delete_verified_runtime_scope(
    savfox_home: &std::path::Path,
    channel_id: &str,
    account_id: &str,
) -> anyhow::Result<()> {
    let path = verified_scope_path(savfox_home, channel_id, account_id);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("delete verified Arkret scope {}", path.display()))
        }
    }
}

fn parse_inkson_bootstrap(value: Option<&Value>) -> Option<AgentPairingBootstrap> {
    serde_json::from_value(value?.clone()).ok()
}

/// Derive a stable protocol-valid Arkret device id for a locally managed
/// Savfox runtime identity.
#[must_use]
pub fn derive_arkret_device_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"savfox.arkret.device.v1");
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!("ak:device:{}", uuid::Uuid::from_bytes(bytes))
}

#[must_use]
fn derive_agent_runtime_account_id(
    channel_id: &str,
    principal_id: &str,
    pairing_request_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(channel_id.as_bytes());
    hasher.update([0]);
    hasher.update(principal_id.as_bytes());
    hasher.update([0]);
    hasher.update(pairing_request_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{channel_id}-{}", &digest[..24])
}

/// Read the controller DID from the saved account slot.
///
/// Both spellings are accepted because the Arkret channel config already mixes
/// camelCase UI keys with snake_case protocol keys. A value that is not a
/// well-formed DID is dropped rather than stored, so the Sidecar gate fails
/// closed instead of comparing Event actors against a malformed string.
fn parse_controller_id(raw: &serde_json::Map<String, Value>) -> Option<String> {
    let value = raw
        .get("controllerId")
        .or_else(|| raw.get("controller_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    arkret::Did::new(value.to_owned())
        .ok()
        .map(|did| did.to_string())
}

fn parse_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(_) => vec!["<invalid requestedScope representation>".to_owned()],
    }
}

/// Load all configured Arkret channels from `savfox_home/channels/*.json`.
pub async fn load_arkret_channel_configs(
    savfox_home: &PathBuf,
) -> anyhow::Result<Vec<ArkretChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for arkret")?;
    let mut parsed = Vec::new();
    for config in all_configs.iter().filter(|config| {
        config.enabled
            && config.kind.eq_ignore_ascii_case("arkret")
            && config.config.get("mode").and_then(Value::as_str) == Some("agent")
    }) {
        parsed.push(
            ArkretChannelConfig::from_strict_agent_config(config)
                .with_context(|| format!("invalid Arkret Agent config '{}'", config.id))?,
        );
    }
    Ok(parsed)
}

/// Resolve an outbound account, preserving the saved channel instance that
/// accepted the inbound event.
///
/// Arkret account channels are principal-bound, so an exact saved config ID is
/// mandatory. A type-level fallback could sign with a different Agent.
pub async fn resolve_arkret_outbound_account_for_config(
    savfox_home: &PathBuf,
    realm_id: &str,
    saved_channel_config_id: Option<&str>,
) -> anyhow::Result<Option<(ArkretChannelConfig, ArkretAccountConfig)>> {
    resolve_arkret_outbound_account_for_binding(
        savfox_home,
        realm_id,
        saved_channel_config_id,
        None,
    )
    .await
}

/// Resolve the exact account recorded by an execution binding.
///
/// Supplying `expected_account_id` disables first-sender fallback, so a
/// checkpoint can never be signed by another account in the same config.
pub async fn resolve_arkret_outbound_account_for_binding(
    savfox_home: &PathBuf,
    realm_id: &str,
    saved_channel_config_id: Option<&str>,
    expected_account_id: Option<&str>,
) -> anyhow::Result<Option<(ArkretChannelConfig, ArkretAccountConfig)>> {
    let config_id = saved_channel_config_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret outbound routing requires saved_channel_config_id; platform-level account fallback is disabled"
            )
        })?;
    let Some(raw) = savfox_core::config::channel_store::get_channel_config(savfox_home, config_id)
        .await
        .with_context(|| format!("failed to load routed Arkret channel config '{config_id}'"))?
    else {
        return Ok(None);
    };
    if !raw.enabled {
        return Ok(None);
    }
    let channel = ArkretChannelConfig::from_strict_agent_config(&raw)
        .with_context(|| format!("routed Arkret channel config '{config_id}' is invalid"))?;
    let account = if let Some(account_id) = expected_account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let account = channel
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .with_context(|| {
                format!(
                    "routed Arkret channel config '{config_id}' has no bound account '{account_id}'"
                )
            })?;
        anyhow::ensure!(
            account.send,
            "bound Arkret account '{account_id}' is not enabled for outbound delivery"
        );
        account.clone()
    } else {
        let Some(account) = channel.select_send_account(realm_id).cloned() else {
            return Ok(None);
        };
        account
    };
    Ok(Some((channel, account)))
}

fn runtime_key_proof_expires_at(
    now: DateTime<Utc>,
    pairing_expires_at: DateTime<Utc>,
) -> DateTime<Utc> {
    std::cmp::min(pairing_expires_at, now + chrono::Duration::seconds(300))
}

pub fn build_arkret_runtime_key_request_json(
    account: &ArkretAccountConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<Value> {
    let bootstrap = account.inkson_bootstrap.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret agent '{}' missing inksonBootstrap for runtime key request",
            account.id
        )
    })?;
    let key_ref = account.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret agent '{}' missing keyRef for runtime key request",
            account.id
        )
    })?;
    let verification_method = account
        .verification_method
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret agent '{}' missing verificationMethod for runtime key request",
                account.id
            )
        })?;
    let endpoint_device_id = DeviceId::new(account.device_id.trim().to_owned()).map_err(|err| {
        anyhow::anyhow!(
            "Arkret agent '{}' has invalid deviceId '{}' for runtime key request: {err}",
            account.id,
            account.device_id
        )
    })?;
    let expected_verification_method = format!("{}#{endpoint_device_id}", bootstrap.agent_id);
    anyhow::ensure!(
        verification_method == expected_verification_method,
        "Arkret agent '{}' verificationMethod must equal '{}' for runtime key request",
        account.id,
        expected_verification_method
    );
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let proof_expires_at = runtime_key_proof_expires_at(now, bootstrap.pairing_expires_at);
    let request = arkret_signatures::agent::RuntimeKeyRequestBuilder::new(
        &signing_key,
        bootstrap.clone(),
        endpoint_device_id,
    )
    .proof_created_at(now)
    .proof_expires_at(proof_expires_at)
    .build_approval_request()
    .map_err(|err| anyhow::anyhow!("agent runtime key request: {err}"))?;
    let mut request = serde_json::to_value(request.body)
        .map_err(|err| anyhow::anyhow!("serialize agent runtime key request: {err}"))?;
    request
        .as_object_mut()
        .expect("typed agent runtime key request serializes as an object")
        .remove("pairing_code");
    Ok(request)
}

/// Build the open runtime-key-request status poll body plus the digest of the
/// local runtime public key. The digest lets the caller compare the
/// `authorized_public_key_digest` returned by the Arkret server against the
/// local key: a mismatch means the pairing was completed by another runtime
/// and MUST NOT be treated as this runtime's approval.
///
/// The returned digest is the **authorization domain** digest — the hash of
/// the raw 32-byte Ed25519 key that the controller-signed
/// `ak.agent.key.authorize` payload binds, which is what the server echoes as
/// `authorized_public_key_digest`. It is deliberately not the private
/// pairing-request JWK digest returned by
/// [`agent_runtime_public_key_digest`](arkret_signatures::agent::agent_runtime_public_key_digest);
/// comparing across the two domains never matches.
pub fn build_arkret_runtime_key_status_request_json(
    account: &ArkretAccountConfig,
) -> anyhow::Result<(Value, String)> {
    let bootstrap = account.inkson_bootstrap.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret agent '{}' missing inksonBootstrap for runtime key status poll",
            account.id
        )
    })?;
    let key_ref = account.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Arkret agent '{}' missing keyRef for runtime key status poll",
            account.id
        )
    })?;
    let verification_method = account
        .verification_method
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Arkret agent '{}' missing verificationMethod for runtime key status poll",
                account.id
            )
        })?;
    let public_key = ed25519_runtime_public_key(key_ref, verification_method)?;
    let verification_method_url = DidUrl::new(verification_method.to_owned()).map_err(|err| {
        anyhow::anyhow!(
            "Arkret agent '{}' has invalid verificationMethod for runtime key status poll: {err}",
            account.id
        )
    })?;
    let local_public_key_digest = arkret_signatures::agent::validate_agent_runtime_public_key(
        &public_key,
        &verification_method_url,
    )
    .map_err(|err| anyhow::anyhow!("agent runtime public key digest: {err}"))?
    .authorization_digest;
    Ok((
        serde_json::json!({
            "pairing_request_id": bootstrap.pairing_request_id,
            "pairing_code": bootstrap.pairing_code,
            "agent_id": account.principal_id,
        }),
        local_public_key_digest.as_str().to_owned(),
    ))
}

#[cfg(test)]
mod strict_tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::{Value, json};

    use super::*;

    fn canonical_config(scope: Value) -> ChannelConfig {
        let principal_id = "did:webvh:example.org:agents:bb";
        let device_id = derive_arkret_device_id(&[principal_id, "pair-bb"]);
        ChannelConfig {
            id: "arkret-agent".to_owned(),
            kind: "arkret".to_owned(),
            slug: "agent".to_owned(),
            name: "Agent".to_owned(),
            enabled: true,
            config: json!({
                "mode": "agent",
                "inksonBootstrap": {
                    "arkret_base_url": "https://arkret.example.org",
                    "service_id": "did:webvh:arkret.example.org",
                    "agent_id": principal_id,
                    "pairing_request_id": "pair-bb",
                    "pairing_code": "123456",
                    "pairing_expires_at": "2026-07-25T09:05:35.700Z"
                },
                "keyRef": {
                    "kind": "keyring",
                    "service": "savfox-arkret",
                    "account": "runtime-bb"
                },
                "verificationMethod": format!("{principal_id}#{device_id}"),
                "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099",
                "requestedScope": scope
            }),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn default_scope() -> Value {
        json!(DEFAULT_AGENT_RUNTIME_SCOPE)
    }

    #[test]
    fn agent_endpoint_derivation_is_stable_across_pairing_components() {
        assert_eq!(
            derive_arkret_device_id(&["did:webvh:example.org:agents:support", "pair-123",]),
            "ak:device:af5d87e9-102d-765f-91ea-f649575db582"
        );
    }

    #[test]
    fn replacement_pairing_gets_fresh_durable_account_scope() {
        let first_config = canonical_config(default_scope());
        let first = ArkretChannelConfig::from_strict_agent_config(&first_config)
            .expect("first pairing parses");
        let first_again = ArkretChannelConfig::from_strict_agent_config(&first_config)
            .expect("same pairing parses deterministically");

        let mut replacement_config = canonical_config(default_scope());
        replacement_config.config["inksonBootstrap"]["pairing_request_id"] =
            json!("pair-replacement");
        let principal_id = "did:webvh:example.org:agents:bb";
        let replacement_device = derive_arkret_device_id(&[principal_id, "pair-replacement"]);
        replacement_config.config["verificationMethod"] =
            json!(format!("{principal_id}#{replacement_device}"));
        let replacement = ArkretChannelConfig::from_strict_agent_config(&replacement_config)
            .expect("replacement pairing parses");

        assert_eq!(first.accounts[0].id, first_again.accounts[0].id);
        assert_ne!(first.accounts[0].id, replacement.accounts[0].id);
        assert_ne!(
            first.accounts[0].device_id,
            replacement.accounts[0].device_id
        );
    }

    #[test]
    fn canonical_agent_config_is_accepted() {
        let parsed =
            ArkretChannelConfig::from_strict_agent_config(&canonical_config(default_scope()))
                .expect("canonical config");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(
            parsed.accounts[0].principal_id,
            "did:webvh:example.org:agents:bb"
        );
    }

    #[test]
    fn historical_action_alias_is_rejected_without_migration() {
        let mut scope = DEFAULT_AGENT_RUNTIME_SCOPE
            .iter()
            .map(|action| (*action).to_owned())
            .collect::<Vec<_>>();
        scope[1] = "ak.self.events.scan".to_owned();
        let error = ArkretChannelConfig::from_strict_agent_config(&canonical_config(json!(scope)))
            .expect_err("historical alias must fail");
        assert!(error.to_string().contains("unknown canonical actions"));
    }

    #[test]
    fn query_scope_aliases_preserve_existing_pairing_commitments() {
        let scope = DEFAULT_AGENT_RUNTIME_SCOPE
            .iter()
            .map(|action| match *action {
                "ak.self.events.read.scan" => "ak.self.events.query.scan".to_owned(),
                "ak.self.events.read.frontier" => "ak.self.events.query.frontier".to_owned(),
                action => action.to_owned(),
            })
            .collect::<Vec<_>>();

        let parsed = ArkretChannelConfig::from_strict_agent_config(&canonical_config(json!(scope)))
            .expect("query aliases should remain valid for persisted pairings");
        let account = &parsed.accounts[0];

        assert!(account.has_requested_scope("ak.self.events.read.scan"));
        assert!(account.has_requested_scope("ak.self.events.read.frontier"));
        assert!(
            account
                .requested_scope
                .iter()
                .any(|action| action == "ak.self.events.query.scan"),
            "the signed pairing spelling must not be rewritten"
        );
    }

    #[test]
    fn query_and_read_spellings_are_duplicate_scopes() {
        let actions = vec![
            "ak.self.events.query.scan".to_owned(),
            "ak.self.events.read.scan".to_owned(),
        ];

        assert_eq!(
            duplicate_requested_scope_actions(&actions),
            vec!["ak.self.events.read.scan".to_owned()]
        );
    }

    #[test]
    fn missing_required_action_is_rejected() {
        let scope = DEFAULT_AGENT_RUNTIME_SCOPE
            .iter()
            .filter(|action| **action != "ak.self.events.command.submit")
            .copied()
            .collect::<Vec<_>>();
        let error = ArkretChannelConfig::from_strict_agent_config(&canonical_config(json!(scope)))
            .expect_err("missing action must fail");
        assert!(
            error
                .to_string()
                .contains("missing required runtime actions")
        );
    }

    #[test]
    fn interactive_agent_scope_requires_reply_submission_and_presence() {
        for required_action in [
            "ak.self.events.read.frontier",
            "ak.self.events.command.submit",
            "ak.self.signal.command.send",
        ] {
            let scope = DEFAULT_AGENT_RUNTIME_SCOPE
                .iter()
                .filter(|action| **action != required_action)
                .copied()
                .collect::<Vec<_>>();
            let error =
                ArkretChannelConfig::from_strict_agent_config(&canonical_config(json!(scope)))
                    .expect_err("interactive Agent capability must fail closed");
            assert!(
                error.to_string().contains(required_action),
                "missing capability error must name {required_action}: {error:#}"
            );
        }
    }

    #[test]
    fn default_online_agent_scope_does_not_request_delayed_publication_leases() {
        assert!(
            !DEFAULT_AGENT_RUNTIME_SCOPE.contains(&"ak.self.authorization_leases.command.issue")
        );
        assert!(!REQUIRED_SEND_SCOPE.contains(&"ak.self.authorization_leases.command.issue"));
    }

    #[test]
    fn keyring_reference_rejects_empty_and_extra_fields() {
        let mut empty = canonical_config(default_scope());
        empty.config["keyRef"]["service"] = json!("");
        let error = ArkretChannelConfig::from_strict_agent_config(&empty)
            .expect_err("empty keyring service must fail");
        assert!(error.to_string().contains("must be non-empty"));

        let mut extra = canonical_config(default_scope());
        extra.config["keyRef"]["legacy"] = json!(true);
        let error = ArkretChannelConfig::from_strict_agent_config(&extra)
            .expect_err("extra keyring field must fail");
        assert!(error.to_string().contains("must contain exactly"));
    }

    #[test]
    fn noncanonical_pairing_timestamp_is_rejected() {
        let mut config = canonical_config(default_scope());
        config.config["inksonBootstrap"]["pairing_expires_at"] = json!("2026-07-25T09:05:35.7Z");
        let error = ArkretChannelConfig::from_strict_agent_config(&config)
            .expect_err("timestamp must be canonical");
        assert!(error.to_string().contains("canonical inksonBootstrap"));
    }

    #[test]
    fn runtime_key_proof_expiry_is_capped_to_five_minutes() {
        let now = "2026-08-04T09:10:00.123Z"
            .parse::<DateTime<Utc>>()
            .expect("now");
        let pairing_expires_at = "2026-08-04T09:20:00.123Z"
            .parse::<DateTime<Utc>>()
            .expect("pairing expiry");

        assert_eq!(
            runtime_key_proof_expires_at(now, pairing_expires_at),
            "2026-08-04T09:15:00.123Z"
                .parse::<DateTime<Utc>>()
                .expect("proof expiry")
        );

        let shorter_pairing_expiry = "2026-08-04T09:12:00.456Z"
            .parse::<DateTime<Utc>>()
            .expect("short pairing expiry");
        assert_eq!(
            runtime_key_proof_expires_at(now, shorter_pairing_expiry),
            shorter_pairing_expiry
        );
    }

    #[test]
    fn runtime_key_request_uses_the_capped_proof_expiry() {
        let mut config = canonical_config(default_scope());
        config.config["keyRef"] = json!({
            "kind": "inline_seed_base64",
            "value": STANDARD_NO_PAD.encode([7_u8; 32]),
        });
        let parsed = ArkretChannelConfig::from_channel_config(&config).expect("agent config");
        let now = "2026-07-25T08:55:35.700Z"
            .parse::<DateTime<Utc>>()
            .expect("now");

        let request = build_arkret_runtime_key_request_json(&parsed.accounts[0], now)
            .expect("runtime key request");

        assert_eq!(
            request["proof_of_possession"]["created_at"],
            "2026-07-25T08:55:35.700Z"
        );
        assert_eq!(
            request["proof_of_possession"]["expires_at"],
            "2026-07-25T09:00:35.700Z"
        );
    }

    /// The status poll compares its local digest against the server's
    /// `authorized_public_key_digest`, which lives in the public authorization
    /// domain (hash of the raw Ed25519 key). Emitting the private
    /// pairing-request JWK digest instead makes every legitimate approval look
    /// like "paired by a different runtime key".
    #[test]
    fn runtime_key_status_digest_uses_the_authorization_domain() {
        let mut config = canonical_config(default_scope());
        config.config["keyRef"] = json!({
            "kind": "inline_seed_base64",
            "value": STANDARD_NO_PAD.encode([7_u8; 32]),
        });
        let parsed = ArkretChannelConfig::from_channel_config(&config).expect("agent config");
        let account = &parsed.accounts[0];
        let verification_method = DidUrl::new(account.verification_method.clone().expect("vm"))
            .expect("canonical verification method");

        let (_, local_digest) =
            build_arkret_runtime_key_status_request_json(account).expect("status request");
        let submit = build_arkret_runtime_key_request_json(
            account,
            "2026-07-25T08:55:35.700Z"
                .parse::<DateTime<Utc>>()
                .expect("now"),
        )
        .expect("submit request");
        let validated = arkret_signatures::agent::validate_agent_runtime_public_key(
            &submit["public_key"],
            &verification_method,
        )
        .expect("submitted public key is canonical");

        assert_eq!(local_digest, validated.authorization_digest.as_str());
        assert_ne!(
            local_digest,
            validated.runtime_request_digest.as_str(),
            "the two digest domains must not be conflated"
        );
    }

    /// The runtime public key is the SDK DTO, not a hand-written JSON object:
    /// its field names are part of the digest preimage (`algorithm`, not
    /// `alg`), so an ad-hoc `json!` fails the SDK's closed schema at runtime.
    #[test]
    fn runtime_public_key_matches_the_sdk_dto_field_names() {
        let key_ref = super::super::signer::ArkretKeyRef::InlineSeedBase64 {
            value: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let verification_method = "did:webvh:example.org:agents:bb#ak:device:test";
        let public_key =
            super::super::signer::ed25519_runtime_public_key(&key_ref, verification_method)
                .expect("runtime public key");

        let rendered = serde_json::to_value(&public_key).expect("serialize");
        assert_eq!(rendered["kty"], json!("OKP"));
        assert_eq!(rendered["algorithm"], json!("Ed25519"));
        assert_eq!(rendered["kid"], json!(verification_method));
        assert!(rendered.get("alg").is_none());
        assert!(rendered.get("key_digest").is_none());
        arkret_signatures::agent::agent_runtime_public_key_digest(&public_key)
            .expect("the SDK accepts its own DTO");
    }

    #[tokio::test]
    async fn outbound_resolution_requires_exact_instance_id() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-strict-route-{}",
            uuid::Uuid::now_v7()
        ));
        let error = resolve_arkret_outbound_account_for_config(
            &home,
            "ak:realm:01904100-0000-8000-8000-000000000001",
            None,
        )
        .await
        .expect_err("type-level fallback must fail");
        assert!(error.to_string().contains("saved_channel_config_id"));
    }

    #[tokio::test]
    async fn outbound_resolution_preserves_exact_bound_account() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-account-route-{}",
            uuid::Uuid::now_v7()
        ));
        let config = canonical_config(default_scope());
        let expected = ArkretChannelConfig::from_strict_agent_config(&config)
            .expect("parse canonical config")
            .accounts
            .remove(0);
        savfox_core::config::channel_store::save_channel_config(&home, &config)
            .await
            .expect("save canonical config");

        let (_, account) = resolve_arkret_outbound_account_for_binding(
            &home,
            "ak:realm:01904100-0000-8000-8000-000000000001",
            Some("arkret-agent"),
            Some(&expected.id),
        )
        .await
        .expect("resolve bound account")
        .expect("bound account exists");
        assert_eq!(account.id, expected.id);

        let error = resolve_arkret_outbound_account_for_binding(
            &home,
            "ak:realm:01904100-0000-8000-8000-000000000001",
            Some("arkret-agent"),
            Some("different-account"),
        )
        .await
        .expect_err("bound account fallback must remain disabled");
        assert!(error.to_string().contains("different-account"));
        let _ = tokio::fs::remove_dir_all(home).await;
    }

    #[tokio::test]
    async fn outbound_resolution_rejects_noncanonical_routed_config() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-strict-route-{}",
            uuid::Uuid::now_v7()
        ));
        let mut config = canonical_config(default_scope());
        config.id = "arkret-agent".to_owned();
        config.config["legacyField"] = json!(true);
        savfox_core::config::channel_store::save_channel_config(&home, &config)
            .await
            .expect("save noncanonical config");

        let error = resolve_arkret_outbound_account_for_config(
            &home,
            "ak:realm:01904100-0000-8000-8000-000000000001",
            Some("arkret-agent"),
        )
        .await
        .expect_err("noncanonical routed config must fail");

        assert!(format!("{error:#}").contains("unsupported fields"));
        let _ = tokio::fs::remove_dir_all(home).await;
    }

    #[tokio::test]
    async fn verified_scope_is_invalidated_when_runtime_key_changes() {
        let home = std::env::temp_dir().join(format!(
            "savfox-arkret-authority-test-{}",
            uuid::Uuid::now_v7()
        ));
        let parsed =
            ArkretChannelConfig::from_strict_agent_config(&canonical_config(default_scope()))
                .expect("canonical config");
        let account = &parsed.accounts[0];
        save_verified_runtime_scope(
            &home,
            &parsed.id,
            account,
            "sha256:original-runtime-key".to_owned(),
        )
        .await
        .expect("save verified scope");

        let verified =
            load_verified_runtime_scope(&home, &parsed.id, account, "sha256:original-runtime-key")
                .await
                .expect("load matching verified scope");
        assert!(verified.is_some());

        let stale = load_verified_runtime_scope(
            &home,
            &parsed.id,
            account,
            "sha256:replacement-runtime-key",
        )
        .await
        .expect("invalidate stale verified scope");
        assert!(stale.is_none());

        let removed =
            load_verified_runtime_scope(&home, &parsed.id, account, "sha256:original-runtime-key")
                .await
                .expect("stale state remains deleted");
        assert!(removed.is_none());

        let _ = tokio::fs::remove_dir_all(home).await;
    }
}

// DISABLED: `any()` is always false, so this whole module is dead code that
// neither compiles nor runs. Its fixtures predate the current protocol
// (`#runtime-1` verification methods instead of derived device ids, `alg`
// instead of `algorithm` in grant proofs, removed `access_token`), and
// silencing it is what let two runtime-key bugs reach the UI. Reviving it means
// re-basing ~15 fixtures onto the conventions used by `strict_tests` above.
#[cfg(all(test, any()))]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::json;

    use super::*;

    fn make_channel_config(body: Value) -> ChannelConfig {
        ChannelConfig {
            id: "arkret-test".into(),
            kind: "arkret".into(),
            slug: "test".into(),
            name: "Test".into(),
            enabled: true,
            config: body,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn sdk_inkson_bootstrap(
        base_url: &str,
        service_id: &str,
        agent_id: &str,
        pairing_request_id: &str,
        pairing_code: &str,
    ) -> Value {
        json!({
            "arkret_base_url": base_url,
            "service_id": service_id,
            "agent_id": agent_id,
            "pairing_request_id": pairing_request_id,
            "pairing_code": pairing_code,
            "pairing_expires_at": "2026-07-06T12:00:00.000Z"
        })
    }

    #[test]
    fn parses_multi_account_array() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://arkret.example.org",
            "serviceId": "did:webvh:arkret.example.org",
            "accounts": [
                {
                    "id": "support",
                    "inksonBootstrap": sdk_inkson_bootstrap(
                        "https://arkret.example.org",
                        "did:webvh:arkret.example.org",
                        "did:webvh:example.org:agents:support-1",
                        "pair-support",
                        "123456"
                    ),
                    "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_SUPPORT_KEY" },
                    "verificationMethod": "did:webvh:example.org:agents:support-1#runtime-1",
                    "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000001",
                    "listen": true,
                    "send": true
                },
                {
                    "id": "billing",
                    "inksonBootstrap": sdk_inkson_bootstrap(
                        "https://arkret.example.org",
                        "did:webvh:arkret.example.org",
                        "did:webvh:example.org:agents:billing-1",
                        "pair-billing",
                        "654321"
                    ),
                    "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_BILLING_KEY" },
                    "verificationMethod": "did:webvh:example.org:agents:billing-1#runtime-1",
                    "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000002",
                    "listen": true,
                    "send": false
                }
            ]
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.base_url, "https://arkret.example.org");
        assert_eq!(
            parsed.service_id.as_deref(),
            Some("did:webvh:arkret.example.org")
        );
        assert_eq!(parsed.accounts.len(), 2);
        assert_eq!(parsed.accounts[0].id, "support");
        assert!(!parsed.accounts[1].send);
        parsed.validate().expect("validation");
    }

    #[test]
    fn parses_single_account_flat_form() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:bot",
                "pair-bot",
                "123456"
            ),
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000010"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].mode, ArkretAccountMode::Agent);
        assert_eq!(
            parsed.accounts[0].principal_id,
            "did:webvh:example.org:agents:bot"
        );
        assert!(arkret::DeviceId::new(parsed.accounts[0].device_id.clone()).is_ok());
        parsed.validate().expect("validate");
    }

    #[test]
    fn parses_inkson_bootstrap_agent_runtime_config() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": {
                "arkret_base_url": "https://arkret.example.org",
                "service_id": "did:webvh:arkret.example.org",
                "agent_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "pairing_expires_at": "2026-07-06T12:00:00.000Z"
            },
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099",
            "defaultRealmId": "ak:realm:01904100-0000-8000-8000-000000000001",
            "agentId": "support"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.base_url, "https://arkret.example.org");
        assert_eq!(
            parsed.service_id.as_deref(),
            Some("did:webvh:arkret.example.org")
        );
        assert_eq!(parsed.accounts.len(), 1);
        let account = &parsed.accounts[0];
        assert_eq!(account.mode, ArkretAccountMode::Agent);
        assert_eq!(account.principal_id, "did:webvh:example.org:agents:support");
        assert_eq!(
            account.authorized_event_ref.as_deref(),
            Some("ak:event:01904100-0000-8000-8000-000000000099")
        );
        assert!(account.key_ref.is_some());
        assert_eq!(
            account.requested_scope,
            DEFAULT_AGENT_RUNTIME_SCOPE
                .iter()
                .map(|scope| (*scope).to_owned())
                .collect::<Vec<_>>()
        );
        for required_scope in [
            "ak.self.keys.keypackages.upload.create",
            "ak.self.keys.keypackages.command.consume",
            "ak.self.device_messages.query.list",
            "ak.self.device_messages.command.ack",
            "ak.event.read",
        ] {
            assert!(
                account.has_requested_scope(required_scope),
                "default Arkret Agent runtime scope must include {required_scope}"
            );
        }
        assert_eq!(
            account.inkson_bootstrap.as_ref().map(|bootstrap| {
                arkret::canonical::format_timestamp_canonical(bootstrap.pairing_expires_at)
            }),
            Some("2026-07-06T12:00:00.000Z".to_owned())
        );
        parsed.validate().expect("validate");
    }

    #[test]
    fn rejects_noncanonical_pairing_expiry_precision() {
        let mut bootstrap = sdk_inkson_bootstrap(
            "https://arkret.example.org",
            "did:webvh:arkret.example.org",
            "did:webvh:example.org:agents:support",
            "pair-123",
            "123456",
        );
        bootstrap["pairing_expires_at"] = json!("2026-07-06T12:00:00.000123Z");

        assert!(parse_inkson_bootstrap(Some(&bootstrap)).is_none());
    }

    #[test]
    fn agent_runtime_rejects_static_access_token_without_bootstrap() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "baseUrl": "https://arkret.example.org",
            "principalId": "did:webvh:example.org:agents:support",
            "accessToken": "tok"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("agent mode must not accept static bearer only");
        assert!(format!("{err:#}").contains("must not store accessToken"));
    }

    #[test]
    fn derives_stable_device_id_when_user_omits_it() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:bot",
                "pair-bot",
                "123456"
            ),
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000010"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let again = ArkretChannelConfig::from_channel_config(&cfg).expect("parse again");

        assert_eq!(parsed.accounts[0].device_id, again.accounts[0].device_id);
        assert!(arkret::DeviceId::new(parsed.accounts[0].device_id.clone()).is_ok());
    }

    #[test]
    fn explicit_bad_device_id_rejects_validation() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:bot",
                "pair-bot",
                "123456"
            ),
            "deviceId": "ak:device:bot-1",
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000010"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("bad device id should fail");
        assert!(format!("{err:#}").contains("device_id"));
    }

    #[test]
    fn agent_runtime_rejects_missing_authorized_event_ref() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing authorized event ref should fail");
        assert!(format!("{err:#}").contains("authorizedEventRef"));
    }

    #[test]
    fn agent_runtime_rejects_missing_runtime_key() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing runtime key should fail");
        assert!(format!("{err:#}").contains("keyRef"));
    }

    #[test]
    fn agent_runtime_rejects_listen_without_stream_service_scope() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "principalId": "did:webvh:example.org:agents:support",
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099",
            "requestedScope": ["ak.event.read", "ak.message.create"],
            "listen": true,
            "send": false
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("missing stream service scope");

        assert!(format!("{err:#}").contains("ak.self.events.stream.subscribe"));
    }

    #[test]
    fn agent_runtime_rejects_send_without_submit_service_scope() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "principalId": "did:webvh:example.org:agents:support",
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099",
            "requestedScope": ["ak.event.read", "ak.message.create"],
            "listen": false,
            "send": true
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("missing submit service scope");

        assert!(format!("{err:#}").contains("ak.self.events.command.submit"));
    }

    #[test]
    fn disabled_returns_none() {
        let mut cfg = make_channel_config(json!({
            "baseUrl": "https://arkret.example.org",
            "principalId": "did:webvh:example.org:bot",
            "accessToken": "tok"
        }));
        cfg.enabled = false;
        assert!(ArkretChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_access_token_rejects_validation() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://arkret.example.org",
            "accounts": [{
                "id": "x",
                "principalId": "did:webvh:example.org:x",
                "deviceId": "ak:device:x",
                "accessToken": ""
            }]
        }));
        // accessToken empty → parse_account_entry returns None → accounts empty.
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.accounts.is_empty());
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn missing_base_url_rejects_parse() {
        let cfg = make_channel_config(json!({
            "accounts": [{
                "principalId": "did:webvh:example.org:x",
                "accessToken": "tok"
            }]
        }));
        assert!(ArkretChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn select_send_account_uses_first_sending_user_agent() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://x.example",
            "accounts": [
                {
                    "id":"a",
                    "inksonBootstrap": sdk_inkson_bootstrap(
                        "https://x.example",
                        "did:webvh:x.example",
                        "did:webvh:a",
                        "pair-a",
                        "111111"
                    ),
                    "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_A_KEY" },
                    "verificationMethod": "did:webvh:a#runtime-1",
                    "authorizedEventRef": "ak:event:01904100-0000-8000-8000-0000000000a1"
                },
                {
                    "id":"b",
                    "inksonBootstrap": sdk_inkson_bootstrap(
                        "https://x.example",
                        "did:webvh:x.example",
                        "did:webvh:b",
                        "pair-b",
                        "222222"
                    ),
                    "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_B_KEY" },
                    "verificationMethod": "did:webvh:b#runtime-1",
                    "authorizedEventRef": "ak:event:01904100-0000-8000-8000-0000000000b2"
                }
            ]
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let chosen = parsed.select_send_account("ak:realm:2").expect("match");
        assert_eq!(chosen.id, "a");
    }

    #[tokio::test]
    async fn outbound_resolution_preserves_routed_channel_instance() {
        let savfox_home =
            std::env::temp_dir().join(format!("savfox-arkret-route-test-{}", uuid::Uuid::now_v7()));
        let mut first = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:first",
                "pair-first",
                "111111"
            ),
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_FIRST_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:first#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-0000000000a1"
        }));
        first.id = "arkret-first".into();
        first.slug = "first".into();
        first.name = "First".into();
        let mut second = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:second",
                "pair-second",
                "222222"
            ),
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_SECOND_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:second#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-0000000000b2"
        }));
        second.id = "arkret-second".into();
        second.slug = "second".into();
        second.name = "Second".into();

        savfox_core::config::channel_store::save_channel_config(&savfox_home, &first)
            .await
            .expect("save first channel");
        savfox_core::config::channel_store::save_channel_config(&savfox_home, &second)
            .await
            .expect("save second channel");

        let (channel, account) = resolve_arkret_outbound_account_for_config(
            &savfox_home,
            "ak:realm:01904100-0000-8000-8000-000000000001",
            Some("arkret-second"),
        )
        .await
        .expect("resolve routed channel")
        .expect("routed channel exists");

        assert_eq!(channel.id, "arkret-second");
        assert_eq!(account.principal_id, "did:webvh:example.org:agents:second");

        let _ = tokio::fs::remove_dir_all(savfox_home).await;
    }

    #[test]
    fn runtime_key_request_uses_sdk_helpers_and_omits_private_key() {
        let seed = STANDARD_NO_PAD.encode([7u8; 32]);
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "keyRef": { "kind": "inline_seed_base64", "value": seed },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let request = build_arkret_runtime_key_request_json(
            &parsed.accounts[0],
            DateTime::parse_from_rfc3339("2026-07-06T11:50:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .expect("request");

        assert_eq!(request["pairing_request_id"], json!("pair-123"));
        assert_eq!(
            request["agent_id"],
            json!("did:webvh:example.org:agents:support")
        );
        assert_eq!(
            request["verification_method"],
            json!("did:webvh:example.org:agents:support#runtime-1")
        );
        assert_eq!(request["public_key"]["kty"], json!("OKP"));
        assert_eq!(
            request["public_key"]["kid"],
            json!("did:webvh:example.org:agents:support#runtime-1")
        );
        assert_eq!(request["public_key"]["alg"], json!("Ed25519"));
        assert!(
            request["public_key"]["key"]
                .as_str()
                .is_some_and(|value| !value.is_empty() && !value.contains('='))
        );
        assert_eq!(
            request["proof_of_possession"]["challenge"],
            json!("pair-123")
        );
        assert_eq!(
            request["proof_of_possession"]["audience"],
            json!("did:webvh:arkret.example.org")
        );
        assert!(
            request["proof_of_possession"]["request_canonical_digest"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert!(
            request["proof_of_possession"]["signature"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );

        let rendered = serde_json::to_string(&request).expect("render");
        assert!(!rendered.contains("inline_seed_base64"));
        assert!(!rendered.contains(&seed));
        assert!(request.get("keyRef").is_none());
        assert!(request.get("keyRef").is_none());
    }

    #[test]
    fn runtime_key_request_preserves_canonical_millisecond_expiry_and_signature() {
        use ed25519_dalek::Verifier as _;

        let seed = STANDARD_NO_PAD.encode([7u8; 32]);
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": {
                "arkret_base_url": "https://arkret.example.org",
                "service_id": "did:webvh:arkret.example.org",
                "agent_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "pairing_expires_at": "2026-07-14T14:43:48.784Z"
            },
            "keyRef": { "kind": "inline_seed_base64", "value": seed },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let account = &parsed.accounts[0];
        let request = build_arkret_runtime_key_request_json(account, Utc::now()).expect("request");
        let proof = request["proof_of_possession"].as_object().unwrap();
        let expires_at = proof["expires_at"]
            .as_str()
            .unwrap()
            .parse::<DateTime<Utc>>()
            .unwrap();
        // Verify through the typed proof DTO, so a wire-shape change in the
        // SDK breaks this test at compile time instead of silently checking a
        // field the protocol no longer has.
        let typed_proof: arkret::AgentRuntimeKeyPossessionProof =
            serde_json::from_value(request["proof_of_possession"].clone()).expect("typed proof");
        assert_eq!(typed_proof.expires_at, expires_at);
        let transcript = typed_proof
            .canonical_transcript_bytes("123456")
            .expect("canonical proof transcript");
        let signature = arkret::base64url_decode(typed_proof.signature.as_str()).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&signature).unwrap();
        let signing_key = load_ed25519_signing_key(account.key_ref.as_ref().unwrap()).unwrap();

        assert_eq!(proof["expires_at"], "2026-07-14T14:43:48.784Z");
        signing_key
            .verifying_key()
            .verify(&transcript, &signature)
            .expect("signature must bind the serialized proof expiry");
    }

    #[test]
    fn runtime_key_status_request_carries_pairing_triple_and_local_digest() {
        let seed = STANDARD_NO_PAD.encode([7u8; 32]);
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "inksonBootstrap": sdk_inkson_bootstrap(
                "https://arkret.example.org",
                "did:webvh:arkret.example.org",
                "did:webvh:example.org:agents:support",
                "pair-123",
                "123456"
            ),
            "keyRef": { "kind": "inline_seed_base64", "value": seed },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1"
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let (request, local_digest) =
            build_arkret_runtime_key_status_request_json(&parsed.accounts[0]).expect("request");

        assert_eq!(request["pairing_request_id"], json!("pair-123"));
        assert_eq!(request["pairing_code"], json!("123456"));
        assert_eq!(
            request["agent_id"],
            json!("did:webvh:example.org:agents:support")
        );
        // The status poll body must stay minimal: no key material, no PoP.
        assert!(request.get("public_key").is_none());
        assert!(request.get("proof_of_possession").is_none());
        assert!(local_digest.starts_with("sha256:"));

        // The local digest must be the authorization-domain digest of the same
        // key the submit path sends: that is the domain the server echoes as
        // `authorized_public_key_digest`, so an approval for this key compares
        // equal. The private pairing-request JWK digest is a distinct domain
        // and would never match a legitimate approval.
        let submit = build_arkret_runtime_key_request_json(
            &parsed.accounts[0],
            DateTime::parse_from_rfc3339("2026-07-06T11:50:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .expect("submit request");
        let validated = arkret_signatures::agent::validate_agent_runtime_public_key(
            &submit["public_key"],
            &DidUrl::new("did:webvh:example.org:agents:support#runtime-1").unwrap(),
        )
        .expect("validated public key");
        assert_eq!(local_digest, validated.authorization_digest.as_str());
        assert_ne!(
            local_digest,
            validated.runtime_request_digest.as_str(),
            "the status poll must not compare the private request-domain digest"
        );
    }
}
