//! Applet mode configuration.
//!
//! A Arkret channel saved with `mode = "applet"` declares this savfox node
//! as a registered Applet (the Matrix-AppService equivalent in the Arkret
//! universe). The config carries: applet identity, service URL where this
//! node receives `POST /_arkret/edge/applet/transactions`, the Arkret server we
//! write events back to, namespace declarations, and a ghost-DID
//! generation rule.

use std::path::PathBuf;

use anyhow::Context;
use arkret::signatures::PublicKeyMaterial;
use arkret::{DeviceId, Did};
use serde_json::Value;

use super::namespace::{AppletNamespaces, NamespacePattern};
use crate::arkret::signer::ArkretKeyRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretAppletTrustedVerificationMethod {
    pub verification_method: String,
    pub public_key: PublicKeyMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArkretAppletConfig {
    /// Stable identifier in savfox's local channel store.
    pub id: String,
    /// `ak:applet:<uuidv7>` — stable across registrations.
    pub applet_id: String,
    /// Applet service DID (e.g. `did:web:slack-bridge.example`).
    pub service_id: String,
    /// Controller DID that signs the registration (typically `did:webvh:...`).
    pub controller_id: String,
    /// Public URL where this savfox node accepts inbound transactions
    /// (mounted under `/appservices/arkret/{id}/_arkret/edge/applet`).
    pub base_url: String,
    /// Bot actor DID — the visible identity of the applet in Realms it joins
    /// (usually `<service_id>:bot`).
    pub bot_actor_id: String,
    /// Optional Arkret device id for the bot/applet local MLS member. Required
    /// for generating precise MLS recovery plans, but kept optional for
    /// existing bearer-only applet deployments.
    pub device_id: Option<String>,
    /// Arkret server base URL where outbound events are POSTed.
    pub arkret_server_url: String,
    /// Arkret server service DID used by applet outbound authentication.
    pub arkret_server_did: Option<String>,
    /// Static server verification methods accepted for inbound applet HTTP
    /// Message Signatures and event pushes.
    pub trusted_verification_methods: Vec<ArkretAppletTrustedVerificationMethod>,
    /// Server-issued one-time challenge for DID-proof session grant issuance.
    pub login_challenge: Option<String>,
    /// Optional bearer for outbound `events_submit` calls. In a fully
    /// signed flow this is replaced by the ghost actor's detached JWS.
    pub arkret_bearer_token: Option<String>,
    /// Namespaces declared in the registration. Used for inbound transaction
    /// filtering and for actor / realm lookup endpoints.
    pub namespaces: AppletNamespaces,
    /// External protocols this Applet bridges (`["slack"]`, `["discord"]`, ...).
    pub protocols: Vec<String>,
    /// Prefix to prepend when minting ghost DIDs (colon path-segment form):
    /// `{service_id}:{ghost_did_prefix}{external_id_slug}`.
    /// Default `"ghost:"` → `did:web:host:ghost:<slug>`.
    pub ghost_did_prefix: String,
    /// `requested_scopes[]` — informational; reducer ignores this and only
    /// honors actual `ak.capability.grant` events.
    pub requested_scopes: Vec<String>,
    /// Whether the Arkret server is expected to push event transactions
    /// (`receive_events: true`). Default `true`.
    pub receive_events: bool,
    /// Whether to receive ephemeral (typing/presence) events. Default `false`.
    pub receive_ephemeral: bool,
    /// Whether the server is permitted to rate-limit transaction pushes.
    pub rate_limited: bool,
    /// Optional `ak.capability.grant` event id this applet currently holds.
    /// When set, outbound events include it as `authorization_ref`.
    pub authorization_grant_id: Option<String>,
    /// Operator-supplied security epoch hash (`sha256:<hex>`) over the
    /// registration evidence (DID Document + signing key + endpoint + auth),
    /// per `applet-schema.md` §1. savfox cannot synthesize it locally; when
    /// absent, applet registration validation
    /// emits a zero placeholder suitable only for an unsigned draft destined
    /// for offline controller signing.
    pub registration_epoch: Option<String>,
    /// Phase 8 (T8.A): ed25519 key for DID-proof login + event signing.
    /// When set, `start_arkret_applet_channel` runs login_did_proof to
    /// obtain the bearer rather than relying on a static `accessToken`.
    pub key_ref: Option<ArkretKeyRef>,
    /// Phase 8: verification method id used by the signer. Defaults to
    /// `{bot_actor_id}#key-1` when missing.
    pub verification_method: Option<String>,
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

        let applet_id = first_non_empty(raw, &["appletId", "applet_id"])?;
        let service_id = first_non_empty(raw, &["serviceId", "service_id"])?;
        let controller_id = first_non_empty(raw, &["controllerId", "controller_id"])?;
        let base_url = first_non_empty(raw, &["baseUrl", "base_url"])?;
        let bot_actor_id = first_non_empty(raw, &["botActorId", "bot_actor_id"])
            .unwrap_or_else(|| format!("{service_id}:bot"));
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
        let login_challenge = first_non_empty(raw, &["loginChallenge", "login_challenge"]);
        let arkret_bearer_token =
            first_non_empty(raw, &["accessToken", "access_token", "arkretBearerToken"]);

        let namespaces = parse_namespaces(raw.get("namespaces"));
        let protocols = parse_string_list(raw.get("protocols"));
        let requested_scopes =
            parse_string_list(raw.get("requestedScopes").or(raw.get("requested_scopes")));
        let ghost_did_prefix = first_non_empty(raw, &["ghostDidPrefix", "ghost_did_prefix"])
            .unwrap_or_else(|| "ghost:".to_owned());

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
        let grant_event_path =
            first_non_empty(raw, &["grantEventPath", "grant_event_path"]).map(PathBuf::from);

        Some(Self {
            id: config.id.clone(),
            applet_id,
            service_id,
            controller_id,
            base_url,
            bot_actor_id,
            device_id,
            arkret_server_url,
            arkret_server_did,
            trusted_verification_methods,
            login_challenge,
            arkret_bearer_token,
            namespaces,
            protocols,
            ghost_did_prefix,
            requested_scopes,
            receive_events,
            receive_ephemeral,
            rate_limited,
            authorization_grant_id,
            registration_epoch,
            key_ref,
            verification_method,
            grant_event_path,
        })
    }

    /// Validate required fields are non-empty.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (label, value) in [
            ("applet_id", &self.applet_id),
            ("service_id", &self.service_id),
            ("controller_id", &self.controller_id),
            ("base_url", &self.base_url),
            ("bot_actor_id", &self.bot_actor_id),
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
            ("controller_id", &self.controller_id),
            ("bot_actor_id", &self.bot_actor_id),
        ] {
            Did::new(value.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Arkret applet channel '{}' {label} must be a valid DID URI, got '{}': {err}",
                    self.id,
                    value
                )
            })?;
        }
        let service_did = Did::new(self.service_id.clone())?;
        if service_did.method() != "webvh" {
            anyhow::bail!(
                "Arkret applet channel '{}' service_id must use did:webvh",
                self.id
            );
        }
        if self.key_ref.is_none() {
            anyhow::bail!(
                "Arkret applet channel '{}' requires key_ref for signed outbound events",
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
                "Arkret applet channel '{}' has key_ref but no arkret_server_did / arkretServerDid for DID-proof audience",
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
        if self.key_ref.is_some() {
            let Some(challenge) = self.login_challenge.as_deref().map(str::trim) else {
                anyhow::bail!(
                    "Arkret applet channel '{}' has key_ref but no login_challenge / loginChallenge",
                    self.id
                );
            };
            if challenge.is_empty() {
                anyhow::bail!(
                    "Arkret applet channel '{}' has key_ref but no login_challenge / loginChallenge",
                    self.id
                );
            }
            if challenge.len() < 16 {
                anyhow::bail!(
                    "Arkret applet channel '{}' login_challenge must be at least 16 characters",
                    self.id
                );
            }
        }
        if self.namespaces.actors.is_empty()
            && self.namespaces.realms.is_empty()
            && self.namespaces.handles.is_empty()
        {
            anyhow::bail!(
                "Arkret applet channel '{}' declares no namespaces; at least one of \
                 actors/realms/handles is required",
                self.id
            );
        }
        if self.protocols.is_empty() {
            anyhow::bail!(
                "Arkret applet channel '{}' declares no protocols (e.g. [\"slack\"])",
                self.id
            );
        }
        if self
            .arkret_bearer_token
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "Arkret applet channel '{}' missing access_token / arkretBearerToken for inbound applet authentication",
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
    Ok(all_configs
        .iter()
        .filter_map(ArkretAppletConfig::from_channel_config)
        .collect())
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
            "serviceId": "did:webvh:slack-bridge.example",
            "controllerId": "did:webvh:example.com:admin",
            "baseUrl": "https://savfox.example/appservices/arkret/arkret-applet-test",
            "botActorId": "did:webvh:slack-bridge.example:bot",
            "arkretServerUrl": "https://arkret.example.org",
            "arkretServerDid": "did:webvh:arkret.example.org",
            "accessToken": "applet-bearer-1",
            "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_APPLET_KEY" },
            "loginChallenge": "arkret-applet-login-challenge",
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
        assert_eq!(parsed.ghost_did_prefix, "ghost:");
        parsed.validate().expect("validate");
    }

    #[test]
    fn parses_snake_case_controller_id() {
        let mut body = valid_body();
        let object = body
            .as_object_mut()
            .expect("valid body should be an object");
        object.remove("controllerId");
        object.insert(
            "controller_id".to_owned(),
            json!("did:webvh:example.com:snake-case-admin"),
        );

        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");

        assert_eq!(
            parsed.controller_id,
            "did:webvh:example.com:snake-case-admin"
        );
        parsed.validate().expect("validate");
    }

    #[test]
    fn parses_keyed_applet_login_challenge() {
        let mut body = valid_body();
        body["keyRef"] = json!({ "kind": "env", "var": "SAVFOX_ARKRET_APPLET_KEY" });
        body["loginChallenge"] = json!("challenge-from-arkret");
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(
            parsed.login_challenge.as_deref(),
            Some("challenge-from-arkret")
        );
        parsed.validate().expect("validate");
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
    fn validate_rejects_no_namespaces() {
        let mut body = valid_body();
        body["namespaces"] = json!({"actors": [], "realms": [], "handles": []});
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("empty namespaces should fail");
        assert!(err.to_string().contains("namespaces"));
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
    fn validate_rejects_missing_inbound_bearer_token() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("accessToken");
        let cfg = make_channel_config(body);
        let parsed = ArkretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing inbound bearer token should fail");
        assert!(err.to_string().contains("inbound applet authentication"));
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
