use std::path::PathBuf;

use anyhow::Context;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use cokret_identifiers::{DeviceId, Did};
use ed25519_dalek::Signer as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::signer::{CokretKeyRef, load_ed25519_signing_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CokretAccountMode {
    /// CKP-0008 personal agent runtime: Yougen bootstrap, local runtime key,
    /// agent_key_proof session grant, and DPoP-bound self endpoints.
    Agent,
}

impl CokretAccountMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretYougenBootstrap {
    pub base_url: String,
    pub service_did: Option<String>,
    pub agent_principal_id: String,
    pub pairing_request_id: String,
    pub pairing_code: String,
    pub expires_at: Option<String>,
    pub requested_scope: Vec<String>,
    pub content_grant_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretAccountConfig {
    pub mode: CokretAccountMode,
    pub id: String,
    pub principal_id: String,
    pub device_id: String,
    /// Reject-only stale field. Personal-agent mode must not store bearer
    /// tokens; applet bearer config lives in the applet config.
    pub access_token: String,
    /// Local Ed25519 runtime key reference. In personal-agent mode this is the
    /// savfox-owned runtime key authorized by Yougen/controller.
    pub key_ref: Option<CokretKeyRef>,
    /// Authorized runtime verification method id for agent_key_proof and event
    /// signing.
    pub verification_method: Option<String>,
    /// Cokret service DID / agent_key_proof audience.
    pub cokret_server_did: Option<String>,
    /// Reject-only stale field. Personal-agent runtime uses agent_key_proof,
    /// not DID-proof login.
    pub login_challenge: Option<String>,
    /// Path to a pre-signed `ck.capability.grant` Event JSON.
    /// When set, the event_id is attached as `authorization_ref` on every
    /// outbound write.
    pub grant_event_path: Option<PathBuf>,
    /// Yougen pairing/bootstrap metadata consumed by savfox. This never
    /// contains a private key.
    pub yougen_bootstrap: Option<CokretYougenBootstrap>,
    /// Durable `ck.agent.key.authorize` reference proving the runtime key has
    /// been approved by the controller.
    pub authorized_event_ref: Option<String>,
    /// Requested service/content runtime scope saved as typed list.
    pub requested_scope: Vec<String>,
    pub default_realm_id: Option<String>,
    pub default_flow_id: Option<String>,
    pub agent_id: Option<String>,
    pub listen: bool,
    pub send: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretChannelConfig {
    pub id: String,
    pub base_url: String,
    pub service_did: Option<String>,
    pub accounts: Vec<CokretAccountConfig>,
}

impl CokretChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("cokret") {
            return None;
        }

        let raw = config.config.as_object()?;
        let bootstrap =
            parse_yougen_bootstrap(raw.get("yougenBootstrap").or(raw.get("yougen_bootstrap")));
        let base_url = first_non_empty(raw, &["baseUrl", "base_url", "homeserver", "url"])
            .or_else(|| bootstrap.as_ref().map(|value| value.base_url.clone()))?;
        let service_did = first_non_empty(raw, &["serviceDid", "service_did"]).or_else(|| {
            bootstrap
                .as_ref()
                .and_then(|value| value.service_did.clone())
        });

        let accounts = match raw.get("accounts") {
            Some(value) => parse_accounts(value, raw, &config.id, bootstrap.as_ref()),
            None => parse_accounts(&Value::Null, raw, &config.id, bootstrap.as_ref()),
        };

        Some(Self {
            id: config.id.clone(),
            base_url,
            service_did,
            accounts,
        })
    }

    /// Validate that the channel has at least one usable account.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.base_url.trim().is_empty() {
            anyhow::bail!("Cokret channel '{}' missing base_url", self.id);
        }
        if let Some(service_did) = self.service_did.as_deref() {
            Did::new(service_did.to_owned()).map_err(|err| {
                anyhow::anyhow!(
                    "Cokret channel '{}' service_did must be a valid DID URI, got '{}': {err}",
                    self.id,
                    service_did
                )
            })?;
        }
        if self.accounts.is_empty() {
            anyhow::bail!(
                "Cokret channel '{}' has no accounts; configure at least one controlled account",
                self.id
            );
        }
        for account in &self.accounts {
            account.validate().with_context(|| {
                format!(
                    "Cokret channel '{}' account '{}' is invalid",
                    self.id, account.id
                )
            })?;
        }
        Ok(())
    }

    /// Find an account by id.
    #[must_use]
    pub fn account(&self, account_id: &str) -> Option<&CokretAccountConfig> {
        self.accounts.iter().find(|a| a.id == account_id)
    }

    /// Pick the account that should send to the given realm. Preference:
    /// 1. An account whose `default_realm_id == realm_id`.
    /// 2. The first account with `send == true`.
    #[must_use]
    pub fn select_send_account(&self, realm_id: &str) -> Option<&CokretAccountConfig> {
        if let Some(account) = self.accounts.iter().find(|a| {
            a.send
                && a.default_realm_id
                    .as_deref()
                    .is_some_and(|r| r.eq_ignore_ascii_case(realm_id))
        }) {
            return Some(account);
        }
        self.accounts.iter().find(|a| a.send)
    }
}

impl CokretAccountConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("Cokret account missing id");
        }
        if self.principal_id.trim().is_empty() {
            anyhow::bail!("Cokret account '{}' missing principal_id (DID)", self.id);
        }
        Did::new(self.principal_id.clone()).map_err(|err| {
            anyhow::anyhow!(
                "Cokret account '{}' principal_id must be a valid DID URI, got '{}': {err}",
                self.id,
                self.principal_id
            )
        })?;
        if !self.device_id.trim().is_empty() {
            DeviceId::new(self.device_id.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Cokret account '{}' device_id must be a valid Cokret device id, got '{}': {err}",
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
                "Cokret agent '{}' must not store accessToken; use Yougen bootstrap, runtimeKeyRef, and agent_key_proof + DPoP",
                self.id
            );
        }
        if self.yougen_bootstrap.is_none() {
            anyhow::bail!(
                "Cokret agent '{}' missing yougenBootstrap; paste the Yougen Savfox bootstrap instead of a static session grant",
                self.id
            );
        }
        if self.key_ref.is_none() {
            anyhow::bail!(
                "Cokret agent '{}' missing runtimeKeyRef/keyRef for the local agent runtime key",
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
                "Cokret agent '{}' missing authorized verificationMethod from the completed agent-key pairing",
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
                "Cokret agent '{}' missing authorizedEventRef for ck.agent.key.authorize; pairing must complete before the channel can run",
                self.id
            );
        }
        if self.requested_scope.is_empty() {
            anyhow::bail!(
                "Cokret agent '{}' missing requestedScope; include service scopes such as ck.self.events.stream.subscribe",
                self.id
            );
        }
        if self.listen
            && self
                .default_realm_id
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "Cokret agent '{}' listen=true requires defaultRealmId for ck.self.events.stream.subscribe",
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
    bootstrap: Option<&CokretYougenBootstrap>,
) -> Vec<CokretAccountConfig> {
    match accounts_value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let item_bootstrap = parse_yougen_bootstrap(
                    object
                        .get("yougenBootstrap")
                        .or(object.get("yougen_bootstrap")),
                );
                parse_account_entry(object, channel_id, item_bootstrap.as_ref().or(bootstrap))
            })
            .collect(),
        Value::Object(_) | Value::Null => {
            // Allow single-account flat form, but the personal-agent runtime
            // must still carry a Yougen bootstrap.
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
    bootstrap: Option<&CokretYougenBootstrap>,
) -> Option<CokretAccountConfig> {
    let principal_id = first_non_empty(map, &["principalId", "principal_id", "did", "user_id"])
        .or_else(|| bootstrap.map(|value| value.agent_principal_id.clone()))?;
    let access_token = first_non_empty(map, &["accessToken", "access_token"]).unwrap_or_default();
    let key_ref = map
        .get("runtimeKeyRef")
        .or_else(|| map.get("runtime_key_ref"))
        .or_else(|| map.get("runtimeKey"))
        .or_else(|| map.get("runtime_key"))
        .or_else(|| map.get("keyRef"))
        .or_else(|| map.get("key_ref"))
        .and_then(CokretKeyRef::from_value);
    let mode = CokretAccountMode::Agent;
    // Caller must have at least one usable path or bootstrap. Reject parse for
    // entries that are not actionable; `validate()` reports the precise error
    // for incomplete bootstrap/pairing entries.
    if bootstrap.is_none() && key_ref.is_none() && access_token.is_empty() {
        return None;
    }
    let id = first_non_empty(map, &["id", "accountId", "account_id"])
        .unwrap_or_else(|| principal_id.clone());
    let device_id = first_non_empty(map, &["deviceId", "device_id"])
        .unwrap_or_else(|| derive_cokret_device_id(&[channel_id, &id, &principal_id]));
    let default_realm_id = first_non_empty(map, &["defaultRealmId", "default_realm_id", "realmId"]);
    let default_flow_id = first_non_empty(map, &["defaultFlowId", "default_flow_id", "flowId"]);
    let agent_id = first_non_empty(map, &["agentId", "agent_id", "agent"]);
    let verification_method = first_non_empty(
        map,
        &[
            "verificationMethod",
            "verification_method",
            "verificationMethodId",
            "authorizedVerificationMethod",
            "authorized_verification_method",
        ],
    );
    let cokret_server_did = first_non_empty(map, &["cokretServerDid", "cokret_server_did"]);
    let login_challenge = first_non_empty(map, &["loginChallenge", "login_challenge"]);
    let grant_event_path =
        first_non_empty(map, &["grantEventPath", "grant_event_path"]).map(PathBuf::from);
    let authorized_event_ref = first_non_empty(
        map,
        &[
            "authorizedEventRef",
            "authorized_event_ref",
            "agentKeyAuthorizationRef",
            "agent_key_authorization_ref",
        ],
    );

    let listen = map.get("listen").and_then(Value::as_bool).unwrap_or(true);
    let send = map.get("send").and_then(Value::as_bool).unwrap_or(true);
    let requested_scope = parse_string_list(
        map.get("requestedScope")
            .or_else(|| map.get("requested_scope")),
    )
    .into_iter()
    .chain(
        bootstrap
            .map(|value| value.requested_scope.clone())
            .unwrap_or_default(),
    )
    .fold(Vec::<String>::new(), |mut acc, item| {
        if !acc.iter().any(|seen| seen == &item) {
            acc.push(item);
        }
        acc
    });

    Some(CokretAccountConfig {
        mode,
        id,
        principal_id,
        device_id,
        access_token,
        key_ref,
        verification_method,
        cokret_server_did,
        login_challenge,
        grant_event_path,
        yougen_bootstrap: bootstrap.cloned(),
        authorized_event_ref,
        requested_scope,
        default_realm_id,
        default_flow_id,
        agent_id,
        listen,
        send,
    })
}

fn parse_yougen_bootstrap(value: Option<&Value>) -> Option<CokretYougenBootstrap> {
    let obj = value?.as_object()?;
    let base_url = first_non_empty(
        obj,
        &[
            "baseUrl",
            "base_url",
            "cokretBaseUrl",
            "cokret_base_url",
            "yougenBaseUrl",
            "yougen_base_url",
        ],
    )?;
    let service_did = first_non_empty(obj, &["serviceDid", "service_did", "cokretServiceDid"]);
    let agent_principal_id = first_non_empty(
        obj,
        &[
            "agentPrincipalId",
            "agent_principal_id",
            "principalId",
            "principal_id",
            "agentDid",
            "agent_did",
        ],
    )?;
    let pairing_request_id = first_non_empty(
        obj,
        &[
            "pairingRequestId",
            "pairing_request_id",
            "requestId",
            "request_id",
        ],
    )?;
    let pairing_code = first_non_empty(obj, &["pairingCode", "pairing_code", "code"])?;
    let expires_at = first_non_empty(obj, &["expiresAt", "expires_at"]);
    let requested_scope = parse_string_list(
        obj.get("requestedScope")
            .or_else(|| obj.get("requested_scope"))
            .or_else(|| obj.get("requestedServiceScope"))
            .or_else(|| obj.get("requested_service_scope"))
            .or_else(|| obj.get("serviceScope"))
            .or_else(|| obj.get("service_scope")),
    );
    let content_grant_summary = bootstrap_summary_value(
        obj.get("contentGrantSummary")
            .or_else(|| obj.get("content_grant_summary"))
            .or_else(|| obj.get("contentGrant"))
            .or_else(|| obj.get("content_grant")),
    );

    Some(CokretYougenBootstrap {
        base_url,
        service_did,
        agent_principal_id,
        pairing_request_id,
        pairing_code,
        expires_at,
        requested_scope,
        content_grant_summary,
    })
}

fn bootstrap_summary_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        Value::Null => None,
        other => serde_json::to_string(other).ok(),
    }
}

/// Derive a stable protocol-valid Cokret device id for a locally managed
/// Savfox runtime identity.
#[must_use]
pub fn derive_cokret_device_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"savfox.cokret.device.v1");
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!("ck:device:{}", uuid::Uuid::from_bytes(bytes))
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
        _ => Vec::new(),
    }
}

/// Load all configured Cokret channels from `savfox_home/channels/*.json`.
pub async fn load_cokret_channel_configs(
    savfox_home: &PathBuf,
) -> anyhow::Result<Vec<CokretChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for cokret")?;
    Ok(all_configs
        .iter()
        .filter_map(CokretChannelConfig::from_channel_config)
        .collect())
}

/// Resolve an outbound (send) account for a realm.
///
/// Iterates configured Cokret channels and returns the first
/// `(channel, account)` pair whose [`CokretChannelConfig::select_send_account`]
/// matches the realm.
pub async fn resolve_cokret_outbound_account(
    savfox_home: &PathBuf,
    realm_id: &str,
) -> anyhow::Result<Option<(CokretChannelConfig, CokretAccountConfig)>> {
    let channels = load_cokret_channel_configs(savfox_home).await?;
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

pub fn build_cokret_runtime_key_request_json(
    account: &CokretAccountConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<Value> {
    let bootstrap = account.yougen_bootstrap.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret agent '{}' missing yougenBootstrap for runtime key request",
            account.id
        )
    })?;
    let key_ref = account.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Cokret agent '{}' missing runtimeKeyRef/keyRef for runtime key request",
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
                "Cokret agent '{}' missing verificationMethod for runtime key request",
                account.id
            )
        })?;
    let audience = account
        .cokret_server_did
        .as_deref()
        .or(bootstrap.service_did.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cokret agent '{}' missing serviceDid/cokretServerDid for runtime key request audience",
                account.id
            )
        })?;
    let agent_principal_id = Did::new(account.principal_id.clone())?;
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let public_key = serde_json::json!({
        "kty": "OKP",
        "kid": verification_method,
        "alg": "Ed25519",
        "key": cokret_core::base64url_encode(signing_key.verifying_key().to_bytes()),
    });
    let request_canonical_digest = cokret::agent::agent_key_pair_proof_request_binding_digest(
        &bootstrap.pairing_request_id,
        &agent_principal_id,
        verification_method,
        &public_key,
        None,
    )
    .map_err(|err| anyhow::anyhow!("agent key pair proof request digest: {err}"))?;
    let expires_at = bootstrap
        .expires_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|| now + Duration::minutes(15));
    let signing_input = cokret::agent::agent_key_pair_proof_signing_input(
        verification_method.to_owned(),
        bootstrap.pairing_request_id.clone(),
        audience.to_owned(),
        expires_at,
        request_canonical_digest.clone(),
    );
    let signature = signing_key.sign(
        &signing_input
            .canonical_bytes()
            .map_err(|err| anyhow::anyhow!("agent key pair proof canonical bytes: {err}"))?,
    );
    let signature = cokret_core::base64url_encode(signature.to_bytes());

    Ok(serde_json::json!({
        "pairing_request_id": bootstrap.pairing_request_id,
        "agent_principal_id": account.principal_id,
        "verification_method": verification_method,
        "public_key": public_key,
        "proof_of_possession": {
            "challenge": bootstrap.pairing_request_id,
            "audience": audience,
            "request_canonical_digest": request_canonical_digest.as_str(),
            "expires_at": expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "signature": signature,
        },
    }))
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
            id: "cokret-test".into(),
            kind: "cokret".into(),
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

    #[test]
    fn parses_multi_account_array() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://cokret.example.org",
            "serviceDid": "did:webvh:cokret.example.org",
            "accounts": [
                {
                    "id": "support",
                    "yougenBootstrap": {
                        "base_url": "https://cokret.example.org",
                        "service_did": "did:webvh:cokret.example.org",
                        "agent_principal_id": "did:webvh:example.org:agents:support-1",
                        "pairing_request_id": "pair-support",
                        "pairing_code": "123456",
                        "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
                    },
                    "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_SUPPORT_KEY" },
                    "verificationMethod": "did:webvh:example.org:agents:support-1#runtime-1",
                    "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000001",
                    "defaultRealmId": "ck:realm:abc",
                    "defaultFlowId": "ck:flow:def",
                    "agentId": "support",
                    "listen": true,
                    "send": true
                },
                {
                    "id": "billing",
                    "yougenBootstrap": {
                        "base_url": "https://cokret.example.org",
                        "service_did": "did:webvh:cokret.example.org",
                        "agent_principal_id": "did:webvh:example.org:agents:billing-1",
                        "pairing_request_id": "pair-billing",
                        "pairing_code": "654321",
                        "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
                    },
                    "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_BILLING_KEY" },
                    "verificationMethod": "did:webvh:example.org:agents:billing-1#runtime-1",
                    "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000002",
                    "defaultRealmId": "ck:realm:billing",
                    "listen": true,
                    "send": false
                }
            ]
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.base_url, "https://cokret.example.org");
        assert_eq!(
            parsed.service_did.as_deref(),
            Some("did:webvh:cokret.example.org")
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
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:bot",
                "pairing_request_id": "pair-bot",
                "pairing_code": "123456",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000010",
            "defaultRealmId": "ck:realm:01904100-0000-7000-8000-000000000001"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].mode, CokretAccountMode::Agent);
        assert_eq!(
            parsed.accounts[0].principal_id,
            "did:webvh:example.org:agents:bot"
        );
        assert!(cokret_identifiers::DeviceId::new(parsed.accounts[0].device_id.clone()).is_ok());
        parsed.validate().expect("validate");
    }

    #[test]
    fn parses_yougen_bootstrap_agent_runtime_config() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "expires_at": "2026-07-06T12:00:00Z",
                "requested_service_scope": [
                    "ck.self.events.stream.subscribe",
                    "ck.self.events.query.scan",
                    "ck.self.events.command.submit",
                    "ck.event.read",
                    "ck.message.create"
                ],
                "content_grant_summary": "Read + Reply"
            },
            "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000099",
            "defaultRealmId": "ck:realm:01904100-0000-7000-8000-000000000001",
            "agentId": "support"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.base_url, "https://cokret.example.org");
        assert_eq!(
            parsed.service_did.as_deref(),
            Some("did:webvh:cokret.example.org")
        );
        assert_eq!(parsed.accounts.len(), 1);
        let account = &parsed.accounts[0];
        assert_eq!(account.mode, CokretAccountMode::Agent);
        assert_eq!(account.principal_id, "did:webvh:example.org:agents:support");
        assert_eq!(
            account.authorized_event_ref.as_deref(),
            Some("ck:event:01904100-0000-7000-8000-000000000099")
        );
        assert!(account.access_token.is_empty());
        assert!(account.key_ref.is_some());
        assert!(
            account
                .requested_scope
                .contains(&"ck.self.events.stream.subscribe".to_owned())
        );
        parsed.validate().expect("validate");
    }

    #[test]
    fn agent_runtime_rejects_static_access_token_without_bootstrap() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "baseUrl": "https://cokret.example.org",
            "principalId": "did:webvh:example.org:agents:support",
            "accessToken": "tok"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("agent mode must not accept static bearer only");
        assert!(format!("{err:#}").contains("must not store accessToken"));
    }

    #[test]
    fn derives_stable_device_id_when_user_omits_it() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:bot",
                "pairing_request_id": "pair-bot",
                "pairing_code": "123456",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000010",
            "defaultRealmId": "ck:realm:01904100-0000-7000-8000-000000000001"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let again = CokretChannelConfig::from_channel_config(&cfg).expect("parse again");

        assert_eq!(parsed.accounts[0].device_id, again.accounts[0].device_id);
        assert!(cokret_identifiers::DeviceId::new(parsed.accounts[0].device_id.clone()).is_ok());
    }

    #[test]
    fn explicit_bad_device_id_rejects_validation() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:bot",
                "pairing_request_id": "pair-bot",
                "pairing_code": "123456",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "deviceId": "ck:device:bot-1",
            "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:bot#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000010"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("bad device id should fail");
        assert!(format!("{err:#}").contains("device_id"));
    }

    #[test]
    fn agent_runtime_rejects_missing_authorized_event_ref() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_AGENT_KEY" },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing authorized event ref should fail");
        assert!(format!("{err:#}").contains("authorizedEventRef"));
    }

    #[test]
    fn agent_runtime_rejects_missing_runtime_key() {
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000099"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing runtime key should fail");
        assert!(format!("{err:#}").contains("runtimeKeyRef"));
    }

    #[test]
    fn disabled_returns_none() {
        let mut cfg = make_channel_config(json!({
            "baseUrl": "https://cokret.example.org",
            "principalId": "did:webvh:example.org:bot",
            "accessToken": "tok"
        }));
        cfg.enabled = false;
        assert!(CokretChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_access_token_rejects_validation() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://cokret.example.org",
            "accounts": [{
                "id": "x",
                "principalId": "did:webvh:example.org:x",
                "deviceId": "ck:device:x",
                "accessToken": ""
            }]
        }));
        // accessToken empty → parse_account_entry returns None → accounts empty.
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
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
        assert!(CokretChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn select_send_account_prefers_default_realm_match() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://x.example",
            "accounts": [
                {
                    "id":"a",
                    "yougenBootstrap": {
                        "base_url": "https://x.example",
                        "service_did": "did:webvh:x.example",
                        "agent_principal_id": "did:webvh:a",
                        "pairing_request_id": "pair-a",
                        "pairing_code": "111111",
                        "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
                    },
                    "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_A_KEY" },
                    "verificationMethod": "did:webvh:a#runtime-1",
                    "authorizedEventRef": "ck:event:01904100-0000-7000-8000-0000000000a1",
                    "defaultRealmId":"ck:realm:1"
                },
                {
                    "id":"b",
                    "yougenBootstrap": {
                        "base_url": "https://x.example",
                        "service_did": "did:webvh:x.example",
                        "agent_principal_id": "did:webvh:b",
                        "pairing_request_id": "pair-b",
                        "pairing_code": "222222",
                        "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
                    },
                    "runtimeKeyRef": { "kind": "env", "var": "SAVFOX_COKRET_B_KEY" },
                    "verificationMethod": "did:webvh:b#runtime-1",
                    "authorizedEventRef": "ck:event:01904100-0000-7000-8000-0000000000b2",
                    "defaultRealmId":"ck:realm:2"
                }
            ]
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let chosen = parsed.select_send_account("ck:realm:2").expect("match");
        assert_eq!(chosen.id, "b");
    }

    #[test]
    fn runtime_key_request_uses_sdk_helpers_and_omits_private_key() {
        let seed = STANDARD_NO_PAD.encode([7u8; 32]);
        let cfg = make_channel_config(json!({
            "mode": "agent",
            "yougenBootstrap": {
                "base_url": "https://cokret.example.org",
                "service_did": "did:webvh:cokret.example.org",
                "agent_principal_id": "did:webvh:example.org:agents:support",
                "pairing_request_id": "pair-123",
                "pairing_code": "123456",
                "expires_at": "2026-07-06T12:00:00Z",
                "requested_scope": ["ck.self.events.stream.subscribe", "ck.event.read"]
            },
            "runtimeKeyRef": { "kind": "inline_seed_base64", "value": seed },
            "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
            "authorizedEventRef": "ck:event:01904100-0000-7000-8000-000000000099",
            "defaultRealmId": "ck:realm:01904100-0000-7000-8000-000000000001"
        }));
        let parsed = CokretChannelConfig::from_channel_config(&cfg).expect("parse");
        let request = build_cokret_runtime_key_request_json(
            &parsed.accounts[0],
            DateTime::parse_from_rfc3339("2026-07-06T11:50:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .expect("request");

        assert_eq!(request["pairing_request_id"], json!("pair-123"));
        assert_eq!(
            request["agent_principal_id"],
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
            json!("did:webvh:cokret.example.org")
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
        assert!(request.get("runtimeKeyRef").is_none());
        assert!(request.get("keyRef").is_none());
    }
}
