use std::path::PathBuf;

use anyhow::Context;
use arkret::{AgentPairingBootstrap, DeviceId, Did};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const DEFAULT_AGENT_RUNTIME_SCOPE: &[&str] = &[
    "ak.self.events.stream.subscribe",
    "ak.self.events.query.scan",
    "ak.self.events.command.submit",
    "ak.self.keys.keypackages.upload.create",
    "ak.self.keys.keypackages.command.consume",
    "ak.self.device_messages.query.list",
    "ak.self.device_messages.command.ack",
    "ak.event.read",
    "ak.message.create",
];

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
    /// Reject-only stale field. Personal-agent mode must not store bearer
    /// tokens; applet bearer config lives in the applet config.
    pub access_token: String,
    /// Local Ed25519 runtime key reference. In personal-agent mode this is the
    /// savfox-owned runtime key authorized by Inkson/controller.
    pub key_ref: Option<ArkretKeyRef>,
    /// Authorized runtime verification method id for agent_key_proof and event
    /// signing.
    pub verification_method: Option<String>,
    /// Arkret service DID / agent_key_proof audience.
    pub arkret_server_did: Option<String>,
    /// Reject-only stale field. Personal-agent runtime uses agent_key_proof,
    /// not DID-proof login.
    pub login_challenge: Option<String>,
    /// Path to a pre-signed `ak.capability.grant` Event JSON.
    /// When set, the event_id is attached as `authorization_ref` on every
    /// outbound write.
    pub grant_event_path: Option<PathBuf>,
    /// Inkson pairing/bootstrap metadata consumed by savfox. This never
    /// contains a private key.
    pub inkson_bootstrap: Option<AgentPairingBootstrap>,
    /// Durable `ak.agent.key.authorize` reference proving the runtime key has
    /// been approved by the controller.
    pub authorized_event_ref: Option<String>,
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
    pub accounts: Vec<ArkretAccountConfig>,
}

impl ArkretChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("arkret") {
            return None;
        }

        let raw = config.config.as_object()?;
        let bootstrap = parse_inkson_bootstrap(raw.get("inksonBootstrap"));
        let base_url = first_non_empty(raw, &["baseUrl"]).or_else(|| {
            bootstrap
                .as_ref()
                .map(|value| value.arkret_base_url.clone())
        })?;
        let service_id = first_non_empty(raw, &["serviceId"])
            .or_else(|| bootstrap.as_ref().map(|value| value.service_id.to_string()));

        let accounts = match raw.get("accounts") {
            Some(value) => parse_accounts(value, raw, &config.id, bootstrap.as_ref()),
            None => parse_accounts(&Value::Null, raw, &config.id, bootstrap.as_ref()),
        };

        Some(Self {
            id: config.id.clone(),
            base_url,
            service_id,
            accounts,
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
        self.requested_scope
            .iter()
            .any(|scope| scope.trim() == action)
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
        if !self.access_token.trim().is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' must not store accessToken; use Inkson bootstrap, keyRef, and agent_key_proof + DPoP",
                self.id
            );
        }
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
        if self
            .verification_method
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "Arkret agent '{}' missing authorized verificationMethod from the completed agent-key pairing",
                self.id
            );
        }
        if self
            .authorized_event_ref
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "Arkret agent '{}' missing authorizedEventRef for ak.agent.key.authorize; pairing must complete before the channel can run",
                self.id
            );
        }
        if self.requested_scope.is_empty() {
            anyhow::bail!(
                "Arkret agent '{}' missing requestedScope; include service scopes such as ak.self.events.stream.subscribe",
                self.id
            );
        }
        if self.listen && !self.has_requested_scope("ak.self.events.stream.subscribe") {
            anyhow::bail!(
                "Arkret agent '{}' listen=true requires service scope ak.self.events.stream.subscribe; content grants alone must not open the subscribe endpoint",
                self.id
            );
        }
        if self.send && !self.has_requested_scope("ak.self.events.command.submit") {
            anyhow::bail!(
                "Arkret agent '{}' send=true requires service scope ak.self.events.command.submit; content grants alone must not call the submit endpoint",
                self.id
            );
        }
        Ok(())
    }
}

fn parse_accounts(
    accounts_value: &Value,
    parent_raw: &serde_json::Map<String, Value>,
    channel_id: &str,
    bootstrap: Option<&AgentPairingBootstrap>,
) -> Vec<ArkretAccountConfig> {
    match accounts_value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let item_bootstrap = parse_inkson_bootstrap(object.get("inksonBootstrap"));
                parse_account_entry(object, channel_id, item_bootstrap.as_ref().or(bootstrap))
            })
            .collect(),
        Value::Object(_) | Value::Null => {
            // Allow single-account flat form, but the personal-agent runtime
            // must still carry a Inkson bootstrap.
            if let Some(account) = parse_account_entry(parent_raw, channel_id, bootstrap) {
                vec![account]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn parse_account_entry(
    map: &serde_json::Map<String, Value>,
    channel_id: &str,
    bootstrap: Option<&AgentPairingBootstrap>,
) -> Option<ArkretAccountConfig> {
    let principal_id = first_non_empty(map, &["principalId"])
        .or_else(|| bootstrap.map(|value| value.agent_id.to_string()))?;
    let access_token = first_non_empty(map, &["accessToken"]).unwrap_or_default();
    let key_ref = map.get("keyRef").and_then(ArkretKeyRef::from_value);
    let mode = ArkretAccountMode::Agent;
    // Caller must have at least one usable path or bootstrap. Reject parse for
    // entries that are not actionable; `validate()` reports the precise error
    // for incomplete bootstrap/pairing entries.
    if bootstrap.is_none() && key_ref.is_none() && access_token.is_empty() {
        return None;
    }
    let id = first_non_empty(map, &["id"]).unwrap_or_else(|| principal_id.clone());
    let device_id = first_non_empty(map, &["deviceId"])
        .unwrap_or_else(|| derive_arkret_device_id(&[channel_id, &id, &principal_id]));
    let verification_method = first_non_empty(map, &["verificationMethod"]);
    let arkret_server_did = first_non_empty(map, &["arkretServerDid"]);
    let login_challenge = first_non_empty(map, &["loginChallenge"]);
    let grant_event_path = first_non_empty(map, &["grantEventPath"]).map(PathBuf::from);
    let authorized_event_ref = first_non_empty(map, &["authorizedEventRef"]);

    let listen = map.get("listen").and_then(Value::as_bool).unwrap_or(true);
    let send = map.get("send").and_then(Value::as_bool).unwrap_or(true);
    let mut requested_scope = parse_string_list(map.get("requestedScope"));
    if requested_scope.is_empty() {
        requested_scope = DEFAULT_AGENT_RUNTIME_SCOPE
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect();
    }

    Some(ArkretAccountConfig {
        mode,
        id,
        principal_id,
        device_id,
        access_token,
        key_ref,
        verification_method,
        arkret_server_did,
        login_challenge,
        grant_event_path,
        inkson_bootstrap: bootstrap.cloned(),
        authorized_event_ref,
        requested_scope,
        listen,
        send,
    })
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

fn first_non_empty(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            }
        })
    })
}

fn parse_string_list(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::String(text) => text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Object(map) => parse_string_list(map.get("actions")),
        _ => Vec::new(),
    }
}

/// Load all configured Arkret channels from `savfox_home/channels/*.json`.
pub async fn load_arkret_channel_configs(
    savfox_home: &PathBuf,
) -> anyhow::Result<Vec<ArkretChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for arkret")?;
    Ok(all_configs
        .iter()
        .filter_map(ArkretChannelConfig::from_channel_config)
        .collect())
}

/// Resolve an outbound (send) account for a realm.
///
/// Iterates configured Arkret channels and returns the first
/// `(channel, account)` pair whose [`ArkretChannelConfig::select_send_account`]
/// matches the realm.
pub async fn resolve_arkret_outbound_account(
    savfox_home: &PathBuf,
    realm_id: &str,
) -> anyhow::Result<Option<(ArkretChannelConfig, ArkretAccountConfig)>> {
    let channels = load_arkret_channel_configs(savfox_home).await?;
    for channel in channels {
        if channel.validate().is_err() {
            continue;
        }
        if let Some(account) = channel.select_send_account(realm_id) {
            let account = account.clone();
            return Ok(Some((channel, account)));
        }
    }
    Ok(None)
}

pub fn build_arkret_runtime_key_request_json(
    account: &ArkretAccountConfig,
    _now: DateTime<Utc>,
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
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let request =
        arkret_signatures::agent::RuntimeKeyRequestBuilder::new(&signing_key, bootstrap.clone())
            .verification_method(verification_method)
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
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let public_key = serde_json::json!({
        "kty": "OKP",
        "kid": verification_method,
        "alg": "Ed25519",
        "key": arkret::base64url_encode(signing_key.verifying_key().to_bytes()),
    });
    let local_public_key_digest =
        arkret_signatures::agent::agent_runtime_public_key_digest(&public_key)
            .map_err(|err| anyhow::anyhow!("agent runtime public key digest: {err}"))?;
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
                    "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000001",
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
                    "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000002",
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000010"
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
            "defaultRealmId": "ak:realm:01904100-0000-7000-8000-000000000001",
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
            Some("ak:event:01904100-0000-7000-8000-000000000099")
        );
        assert!(account.access_token.is_empty());
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000010"
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000010"
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099"
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
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
                    "authorizedEventRef": "ak:event:01904100-0000-7000-8000-0000000000a1"
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
                    "authorizedEventRef": "ak:event:01904100-0000-7000-8000-0000000000b2"
                }
            ]
        }));
        let parsed = ArkretChannelConfig::from_channel_config(&cfg).expect("parse");
        let chosen = parsed.select_send_account("ak:realm:2").expect("match");
        assert_eq!(chosen.id, "a");
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
            "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099"
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
        let request_digest = arkret::Hash::new(
            proof["request_canonical_digest"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
        .unwrap();
        let signing_input = arkret_signatures::agent::agent_key_pair_proof_signing_input(
            request["verification_method"].as_str().unwrap(),
            proof["challenge"].as_str().unwrap(),
            proof["audience"].as_str().unwrap(),
            expires_at,
            request_digest,
        );
        let signature = arkret::base64url_decode(proof["signature"].as_str().unwrap()).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&signature).unwrap();
        let signing_key = load_ed25519_signing_key(account.key_ref.as_ref().unwrap()).unwrap();

        assert_eq!(proof["expires_at"], "2026-07-14T14:43:48.784Z");
        signing_key
            .verifying_key()
            .verify(&signing_input.canonical_bytes().unwrap(), &signature)
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

        // The local digest must match the digest of the public key the
        // submit path sends, so a server echo compares equal for this key.
        let submit = build_arkret_runtime_key_request_json(
            &parsed.accounts[0],
            DateTime::parse_from_rfc3339("2026-07-06T11:50:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .expect("submit request");
        let expected_digest =
            arkret_signatures::agent::agent_runtime_public_key_digest(&submit["public_key"])
                .expect("digest");
        assert_eq!(local_digest, expected_digest.as_str());
    }
}
