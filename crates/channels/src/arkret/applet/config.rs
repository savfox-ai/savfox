//! Applet mode configuration.
//!
//! An Arkret channel saved with `mode = "applet"` declares an Applet Service.
//! Its HTTPS management origin supplies discovery and signed completion.
//! Independent managed Accounts require the formal authoring contract; this
//! runtime does not yet connect its authoring custody or managed Device reader.

use std::path::PathBuf;

use anyhow::Context;
use arkret::signatures::PublicKeyMaterial;
use arkret::{AccountId, DeviceId, Did, DidCoreId, SignerEvidenceRef, TrustDomainId};
use serde_json::Value;

use super::namespace::{AppletNamespaces, NamespacePattern};
use crate::arkret::signer::ArkretKeyRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretAppletTrustedVerificationMethod {
    pub verification_method: String,
    pub public_key: PublicKeyMaterial,
}

/// Local bridge identity-key custody configuration, not a protocol carrier.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedActorAuthoringSettings {
    pub principal_endpoint: String,
    pub key_encryption_key_hex: String,
}
impl std::fmt::Debug for ManagedActorAuthoringSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedActorAuthoringSettings")
            .field("principal_endpoint", &self.principal_endpoint)
            .field("key_encryption_key_hex", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretAppletConfig {
    /// Stable identifier in savfox's local channel store.
    pub id: String,
    /// `ak:applet:<uuidv7>` — stable across registrations.
    pub applet_id: String,
    /// Applet service identity core (e.g. `ak:did_core:webvh:zSlackBridgeScid`).
    pub service_id: String,
    /// Controller principal identity core that signs the registration.
    pub controller_principal_id: String,
    /// Mandatory HTTPS discovery, authoring and completion management origin
    /// (mounted under `/appservices/arkret/{id}/_arkret/edge/applet`).
    pub base_url: String,
    /// Exact Bot Account retained from accepted Applet provisioning.
    pub bot_account_id: Option<AccountId>,
    /// Resolvable Applet service DID; its projected core must equal service_id.
    pub service_did: Did,
    pub trust_domain: TrustDomainId,
    /// Explicit runtime identity-custody settings; never synthesized from the Bot.
    pub managed_actor_authoring: ManagedActorAuthoringSettings,
    /// Optional Arkret device id for the bot/applet local MLS member. Required
    /// for generating precise MLS recovery plans, but kept optional for
    /// a concrete accepted Bot Device runtime.
    pub device_id: Option<String>,
    /// Arkret server base URL where outbound events are POSTed.
    pub arkret_server_url: String,
    /// Arkret server service DID used by applet outbound authentication.
    pub arkret_server_did: Option<String>,
    /// Static server verification methods accepted for inbound management
    /// HTTP Message Signatures and completion proofs.
    pub trusted_verification_methods: Vec<ArkretAppletTrustedVerificationMethod>,
    /// Namespaces declared in the registration. They constrain managed
    /// creation and discovery but do not establish an accepted mapping.
    pub namespaces: AppletNamespaces,
    /// External protocols this Applet bridges (`["slack"]`, `["discord"]`, ...).
    pub protocols: Vec<String>,
    /// `requested_scopes[]` — informational; reducer ignores this and only
    /// honors actual `ak.capability.grant` events.
    pub requested_scopes: Vec<String>,
    /// Management delivery preference; it never permits group subscriptions.
    pub receive_events: bool,
    /// Retained management preference; never permits group Signal delivery.
    pub receive_ephemeral: bool,
    /// Whether the server is permitted to rate-limit transaction pushes.
    pub rate_limited: bool,
    /// Optional `ak.capability.grant` event id this applet currently holds.
    /// When set, outbound events include it as `authorization_ref`.
    pub authorization_grant_id: Option<String>,
    /// Retained accepted registration epoch (`sha256:<hex>`), bound to the
    /// Service identity, signing key and management endpoint. Management
    /// completion fails closed when this accepted coordinate is absent.
    pub registration_epoch: Option<String>,
    /// Ed25519 key used to sign outbound Applet events.
    pub key_ref: Option<ArkretKeyRef>,
    /// Phase 8: verification method id used by the signer. Defaults to
    /// the configured Applet service DID and key when signing.
    pub verification_method: Option<String>,
    /// Content address of the retained authenticated service signer evidence.
    pub signer_resolution_evidence_ref: Option<SignerEvidenceRef>,
    /// Phase 8: path to a pre-signed `ak.capability.grant` Event JSON.
    pub grant_event_path: Option<PathBuf>,
}

impl ArkretAppletConfig {
    /// Parse a savfox channel config as an Applet-mode Arkret channel.
    /// Returns `None` if the channel is disabled, of the wrong kind, or
    /// missing the `mode == "applet"` discriminator.
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("arkret") {
            return None;
        }
        let raw = config.config.as_object()?;
        let mode = raw
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if mode != "applet" {
            return None;
        }
        if raw.contains_key("controllerId") || raw.contains_key("controller_id") {
            return None;
        }

        let applet_id = first_non_empty(raw, &["appletId", "applet_id"])?;
        let service_id = first_non_empty(raw, &["serviceId", "service_id"])?;
        let controller_principal_id =
            first_non_empty(raw, &["controllerPrincipalId", "controller_principal_id"])?;
        let base_url = first_non_empty(raw, &["baseUrl", "base_url"])?;
        if raw.contains_key("botActorId") || raw.contains_key("bot_actor_id") {
            return None;
        }
        let bot_account_id = raw
            .get("bot_account_id")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .ok()?;
        let service_did = serde_json::from_value(raw.get("service_did")?.clone()).ok()?;
        let trust_domain = serde_json::from_value(raw.get("trust_domain")?.clone()).ok()?;
        let managed_actor_authoring =
            serde_json::from_value(raw.get("managed_actor_authoring")?.clone()).ok()?;
        let device_id = first_non_empty(raw, &["deviceId", "device_id", "botDeviceId"]);
        let arkret_server_url =
            first_non_empty(raw, &["arkretServerUrl", "arkret_server_url", "homeserver"])
                .unwrap_or_else(|| base_url.clone());
        let arkret_server_did = first_non_empty(
            raw,
            &[
                "arkretServerDid",
                "arkret_server_did",
                "trustedServerDid",
                "trusted_server_did",
            ],
        );
        let trusted_verification_methods = parse_trusted_verification_methods(
            raw.get("trustedVerificationMethods")
                .or_else(|| raw.get("trusted_verification_methods")),
        )?;

        let namespaces = parse_namespaces(raw.get("namespaces"));
        let protocols = parse_string_list(raw.get("protocols"));
        let requested_scopes =
            parse_string_list(raw.get("requestedScopes").or(raw.get("requested_scopes")));

        let receive_events = raw
            .get("receiveEvents")
            .or(raw.get("receive_events"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let receive_ephemeral = raw
            .get("receiveEphemeral")
            .or(raw.get("receive_ephemeral"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rate_limited = raw
            .get("rateLimited")
            .or(raw.get("rate_limited"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let authorization_grant_id = first_non_empty(
            raw,
            &["authorizationGrantId", "authorization_grant_id", "grantId"],
        );
        let registration_epoch = first_non_empty(raw, &["registrationEpoch", "registration_epoch"]);
        let key_ref = raw
            .get("keyRef")
            .or_else(|| raw.get("key_ref"))
            .and_then(ArkretKeyRef::from_value);
        let verification_method = first_non_empty(
            raw,
            &[
                "verificationMethod",
                "verification_method",
                "verificationMethodId",
            ],
        );
        let signer_resolution_evidence_ref = raw
            .get("signerResolutionEvidenceRef")
            .or_else(|| raw.get("signer_resolution_evidence_ref"))
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let grant_event_path =
            first_non_empty(raw, &["grantEventPath", "grant_event_path"]).map(PathBuf::from);

        Some(Self {
            id: config.id.clone(),
            applet_id,
            service_id,
            controller_principal_id,
            base_url,
            bot_account_id,
            service_did,
            trust_domain,
            managed_actor_authoring,
            device_id,
            arkret_server_url,
            arkret_server_did,
            trusted_verification_methods,
            namespaces,
            protocols,
            requested_scopes,
            receive_events,
            receive_ephemeral,
            rate_limited,
            authorization_grant_id,
            registration_epoch,
            key_ref,
            verification_method,
            signer_resolution_evidence_ref,
            grant_event_path,
        })
    }

    /// Validate required fields are non-empty.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (label, value) in [
            ("applet_id", &self.applet_id),
            ("service_id", &self.service_id),
            ("controller_principal_id", &self.controller_principal_id),
            ("base_url", &self.base_url),
            ("arkret_server_url", &self.arkret_server_url),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("Arkret applet channel '{}' missing {label}", self.id);
            }
        }
        // Strictly parse the DID-typed fields with the SDK parser (not a loose
        // `starts_with("did:")`). This guarantees the invariant relied on by
        // downstream applet registration and edge construction.
        for (label, value) in [
            ("service_id", &self.service_id),
            ("controller_principal_id", &self.controller_principal_id),
        ] {
            DidCoreId::new(value.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' {label} must be a valid DID URI, got '{}': {err}",
                    self.id,
                    value
                )
            })?;
        }
        let management_url = url::Url::parse(&self.base_url)?;
        anyhow::ensure!(
            management_url.scheme() == "https"
                && management_url.host_str().is_some()
                && management_url.username().is_empty()
                && management_url.password().is_none()
                && management_url.fragment().is_none(),
            "Applet base_url requires HTTPS and cannot contain credentials or a fragment"
        );
        if let Some(account) = &self.bot_account_id {
            account.validate()?;
        }
        anyhow::ensure!(
            self.device_id.is_none() || self.bot_account_id.is_some(),
            "Device runtime requires its exact accepted Account"
        );
        let custody_endpoint = url::Url::parse(&self.managed_actor_authoring.principal_endpoint)?;
        anyhow::ensure!(
            custody_endpoint.scheme() == "https"
                || matches!(
                    custody_endpoint.host_str(),
                    Some("127.0.0.1" | "localhost" | "::1")
                ),
            "managed_actor_authoring requires HTTPS outside loopback"
        );
        anyhow::ensure!(
            hex::decode(&self.managed_actor_authoring.key_encryption_key_hex)?.len() == 32,
            "managed_actor_authoring key must encode exactly 32 bytes"
        );
        anyhow::ensure!(
            arkret::project_did_to_core_id(&self.service_did)?.as_str() == self.service_id,
            "Applet service_did does not match its registered service_id"
        );
        let method = self
            .verification_method
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Applet service verification_method is required"))?;
        anyhow::ensure!(
            verification_method_did(method) == Some(self.service_did.as_str()),
            "Applet verification method belongs to a different service DID"
        );
        if !self.service_id.starts_with("ak:did_core:webvh:") {
            anyhow::bail!(
                "Arkret applet channel '{}' service_id must use did:webvh",
                self.id
            );
        }
        anyhow::ensure!(
            self.controller_principal_id != self.service_id,
            "Arkret applet channel '{}' controller_principal_id must name a principal and cannot reuse service_id",
            self.id
        );
        if self.key_ref.is_none() {
            anyhow::bail!(
                "Arkret applet channel '{}' requires key_ref for signed outbound events",
                self.id
            );
        }
        if self.signer_resolution_evidence_ref.is_none() {
            anyhow::bail!(
                "Arkret applet channel '{}' requires signer_resolution_evidence_ref for offline Event authoring",
                self.id
            );
        }
        if let Some(device_id) = self.device_id.as_deref() {
            DeviceId::new(device_id.to_owned()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' device_id must be a valid Arkret device id, got '{}': {err}",
                    self.id,
                    device_id
                )
            })?;
        }
        if let Some(value) = self.arkret_server_did.as_deref() {
            Did::new(value.to_owned()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' arkret_server_did must be a valid DID URI, got '{}': {err}",
                    self.id,
                    value
                )
            })?;
        } else if self.key_ref.is_some() {
            anyhow::bail!(
                "Arkret applet channel '{}' has key_ref but no arkret_server_did / arkretServerDid for server trust",
                self.id
            );
        }
        for method in &self.trusted_verification_methods {
            if method.verification_method.trim().is_empty() {
                anyhow::bail!(
                    "Arkret applet channel '{}' has an empty trusted verification method id",
                    self.id
                );
            }
            let owner_did = verification_method_did(&method.verification_method).ok_or_else(|| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' trusted verification method '{}' must include a DID fragment",
                    self.id,
                    method.verification_method
                )
            })?;
            if let Some(server_did) = self.arkret_server_did.as_deref()
                && owner_did != server_did
            {
                anyhow::bail!(
                    "Arkret applet channel '{}' trusted verification method '{}' is owned by '{}', not trusted server DID '{}'",
                    self.id,
                    method.verification_method,
                    owner_did,
                    server_did
                );
            }
            method.public_key.ed25519_bytes().map_err(|err| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' trusted verification method '{}' public key is not valid Ed25519 material: {err}",
                    self.id,
                    method.verification_method
                )
            })?;
        }
        if self.protocols.is_empty() {
            anyhow::bail!(
                "Arkret applet channel '{}' declares no protocols (e.g. [\"slack\"])",
                self.id
            );
        }
        Ok(())
    }
}

fn parse_namespaces(value: Option<&Value>) -> AppletNamespaces {
    let Some(Value::Object(obj)) = value else {
        return AppletNamespaces::default();
    };
    AppletNamespaces {
        actors: parse_pattern_list(obj.get("actors")),
        realms: parse_pattern_list(obj.get("realms")),
        handles: parse_pattern_list(obj.get("handles")),
    }
}

fn parse_pattern_list(value: Option<&Value>) -> Vec<NamespacePattern> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let pattern = obj.get("pattern").and_then(Value::as_str)?.trim();
            if pattern.is_empty() {
                return None;
            }
            let exclusive = obj
                .get("exclusive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(if exclusive {
                NamespacePattern::exclusive(pattern)
            } else {
                NamespacePattern::shared(pattern)
            })
        })
        .collect()
}

fn parse_trusted_verification_methods(
    value: Option<&Value>,
) -> Option<Vec<ArkretAppletTrustedVerificationMethod>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let items = value.as_array()?;
    items
        .iter()
        .map(parse_trusted_verification_method)
        .collect()
}

fn parse_trusted_verification_method(
    value: &Value,
) -> Option<ArkretAppletTrustedVerificationMethod> {
    let obj = value.as_object()?;
    let verification_method = first_non_empty(
        obj,
        &[
            "verificationMethod",
            "verification_method",
            "verificationMethodId",
        ],
    )?;
    let public_key = if let Some(value) = obj.get("publicKey").or_else(|| obj.get("public_key")) {
        serde_json::from_value(value.clone()).ok()?
    } else if let Some(value) = obj.get("publicKeyJwk") {
        PublicKeyMaterial::Jwk {
            value: value.clone(),
        }
    } else {
        PublicKeyMaterial::Ed25519Multibase {
            value: obj.get("publicKeyMultibase")?.as_str()?.to_owned(),
        }
    };
    Some(ArkretAppletTrustedVerificationMethod {
        verification_method,
        public_key,
    })
}

fn verification_method_did(verification_method: &str) -> Option<&str> {
    verification_method
        .rsplit_once('#')
        .map(|(did, _)| did)
        .filter(|did| !did.is_empty())
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
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::String(text) => text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Load all configured Arkret applet channels.
pub async fn load_arkret_applet_configs(
    savfox_home: &std::path::PathBuf,
) -> anyhow::Result<Vec<ArkretAppletConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for arkret applet")?;
    let mut applets = Vec::new();
    for config in &all_configs {
        if !config.enabled
            || !config.kind.eq_ignore_ascii_case("arkret")
            || !config
                .config
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode.eq_ignore_ascii_case("applet"))
        {
            continue;
        }
        let parsed = ArkretAppletConfig::from_channel_config(config).ok_or_else(||
            anyhow::anyhow!("Arkret Applet '{}' requires complete service_did, trust_domain and managed_actor_authoring configuration", config.id))?;
        parsed.validate()?;
        applets.push(parsed);
    }
    Ok(applets)
}

#[cfg(test)]
mod tests {
    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::json;

    use super::*;

    fn make_channel_config(body: Value) -> ChannelConfig {
        ChannelConfig {
            id: "arkret-applet-test".into(),
            kind: "arkret".into(),
            slug: "applet".into(),
            name: "Applet".into(),
            enabled: true,
            config: body,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn valid_body() -> Value {
        json!({
            "mode": "applet",
            "appletId": "ak:applet:21532600-0000-7000-8000-000000000000",
            "serviceId": "ak:did_core:webvh:z6mkfixture",
            "controllerPrincipalId": "ak:did_core:webvh:z6mkcontroller",
            "baseUrl": "https://savfox.example/appservices/arkret/arkret-applet-test",
            "bot_account_id": {"principal_id":"ak:did_core:webvh:z6mkbot", "station_id":"ak:did_core:webvh:z6mkstation"},
            "service_did":"did:webvh:z6mkfixture:slack.example",
            "trust_domain":"ak:trust_domain:example.net",
            "verification_method":"did:webvh:z6mkfixture:slack.example#key-1",
            "managed_actor_authoring":{"principal_endpoint":"https://actors.example", "key_encryption_key_hex":"22".repeat(32)},
            "arkretServerUrl": "https://arkret.example.org",
            "arkretServerDid": "did:webvh:arkret.example.org",
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_APPLET_KEY" },
            "signerResolutionEvidenceRef": "ak:signer_evidence:sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "protocols": ["slack"],
            "namespaces": {
                "actors": [
                    { "pattern": "did:webvh:slack-bridge.example:ghost:*", "exclusive": true }
                ],
                "realms": [
                    { "pattern": "slack:team:*:channel:*", "exclusive": true }
                ],
                "handles": [
                    { "pattern": "slack.acme.example/*", "exclusive": false }
                ]
            },
            "requestedScopes": ["ak.strand.create", "ak.message.create"]
        })
    }

    #[test]
    fn applet_bot_requires_the_exact_account_and_service_binding() {
        let mut body = valid_body();
        body.as_object_mut().unwrap().remove("bot_account_id");
        body["bot_actor_id"] = json!("did:webvh:z6mkbot:bot.example");
        assert!(ArkretAppletConfig::from_channel_config(&make_channel_config(body)).is_none());
        let mut body = valid_body();
        body["service_did"] = json!("did:webvh:z6mkother:other.example");
        assert!(
            ArkretAppletConfig::from_channel_config(&make_channel_config(body))
                .unwrap()
                .validate()
                .is_err()
        );
        let parsed =
            ArkretAppletConfig::from_channel_config(&make_channel_config(valid_body())).unwrap();
        assert_eq!(
            parsed.bot_account_id.as_ref().unwrap().station_id.as_str(),
            "ak:did_core:webvh:z6mkstation"
        );
        assert_ne!(
            parsed.bot_account_id.as_ref().unwrap().station_id.as_str(),
            parsed.service_id
        );
    }

    #[test]
    fn service_installation_does_not_require_a_default_bot() {
        let mut body = valid_body();
        body["bot_account_id"] = Value::Null;
        body.as_object_mut().unwrap().remove("deviceId");
        body.as_object_mut().unwrap().remove("device_id");
        let parsed = ArkretAppletConfig::from_channel_config(&make_channel_config(body)).unwrap();
        assert!(parsed.bot_account_id.is_none());
        parsed.validate().unwrap();
    }

    #[test]
    fn management_base_url_requires_https() {
        let mut body = valid_body();
        body["baseUrl"] = json!("http://127.0.0.1:18881");
        assert!(
            ArkretAppletConfig::from_channel_config(&make_channel_config(body))
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn parses_full_applet_config() {
        let cfg = make_channel_config(valid_body());
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(
            parsed.applet_id,
            "ak:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(parsed.protocols, vec!["slack"]);
        assert_eq!(
            parsed.arkret_server_did.as_deref(),
            Some("did:webvh:arkret.example.org")
        );
        assert_eq!(parsed.namespaces.actors.len(), 1);
        assert!(parsed.namespaces.actors[0].exclusive);
        parsed.validate().expect("validate");
    }

    #[test]
    fn parses_snake_case_controller_principal_id() {
        let mut body = valid_body();
        let object = body
            .as_object_mut()
            .expect("valid body should be an object");
        object.remove("controllerPrincipalId");
        object.insert(
            "controller_principal_id".to_owned(),
            json!("ak:did_core:webvh:z6mksnakeadmin"),
        );

        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");

        assert_eq!(
            parsed.controller_principal_id,
            "ak:did_core:webvh:z6mksnakeadmin"
        );
        parsed.validate().expect("validate");
    }

    #[test]
    fn retired_principal_only_field_name_is_not_accepted() {
        let mut body = valid_body();
        let object = body.as_object_mut().expect("valid body object");
        object.remove("controllerPrincipalId");
        object.insert(
            "controllerId".to_owned(),
            json!("did:webvh:example.com:admin"),
        );

        let cfg = make_channel_config(body);
        assert!(ArkretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn validate_rejects_service_id_impersonating_controller_principal() {
        let mut body = valid_body();
        body["controllerPrincipalId"] = body["serviceId"].clone();
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        let error = parsed
            .validate()
            .expect_err("service identity is not a controller principal");
        assert!(error.to_string().contains("cannot reuse service_id"));
    }

    #[test]
    fn parses_trusted_verification_methods() {
        let mut body = valid_body();
        body["trustedVerificationMethods"] = json!([
            {
                "verificationMethod": "did:webvh:arkret.example.org#key-1",
                "publicKey": {
                    "encoding": "ed25519_raw",
                    "bytes": "CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg"
                }
            }
        ]);
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        parsed.validate().expect("validate");
        assert_eq!(parsed.trusted_verification_methods.len(), 1);
        assert_eq!(
            parsed.trusted_verification_methods[0].verification_method,
            "did:webvh:arkret.example.org#key-1"
        );
        assert_eq!(
            parsed.trusted_verification_methods[0]
                .public_key
                .ed25519_bytes()
                .expect("test public key should decode"),
            [8u8; 32]
        );
    }

    #[test]
    fn validate_rejects_trusted_verification_method_from_untrusted_did() {
        let mut body = valid_body();
        body["trustedVerificationMethods"] = json!([
            {
                "verificationMethod": "did:webvh:evil.example.org#key-1",
                "publicKey": {
                    "encoding": "ed25519_raw",
                    "bytes": "CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg"
                }
            }
        ]);
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("wrong trusted server DID should fail");
        assert!(err.to_string().contains("trusted server DID"));
    }

    #[test]
    fn account_mode_returns_none() {
        let mut body = valid_body();
        body["mode"] = json!("account");
        let cfg = make_channel_config(body);
        assert!(ArkretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_mode_returns_none() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("mode");
        let cfg = make_channel_config(body);
        assert!(ArkretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn disabled_returns_none() {
        let mut cfg = make_channel_config(valid_body());
        cfg.enabled = false;
        assert!(ArkretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_applet_id_returns_none() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("appletId");
        let cfg = make_channel_config(body);
        assert!(ArkretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn service_only_installation_does_not_require_a_ghost_namespace() {
        let mut body = valid_body();
        body["namespaces"] = json!({"actors": [], "realms": [], "handles": []});
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        parsed
            .validate()
            .expect("independent Bots do not need an external identity namespace");
    }

    #[test]
    fn validate_rejects_no_protocols() {
        let mut body = valid_body();
        body["protocols"] = json!([]);
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("empty protocols should fail");
        assert!(err.to_string().contains("protocols"));
    }

    #[test]
    fn validate_rejects_bad_service_id_scheme() {
        let mut body = valid_body();
        body["serviceId"] = json!("not-a-did");
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn service_transport_does_not_require_a_bearer_token() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("accessToken");
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        parsed
            .validate()
            .expect("Service HTTP signatures do not need a bearer token");
    }

    #[test]
    fn defaults_receive_flags() {
        let cfg = make_channel_config(valid_body());
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.receive_events);
        assert!(!parsed.receive_ephemeral);
        assert!(parsed.rate_limited);
    }
}
