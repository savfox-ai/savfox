//! Applet mode configuration.
//!
//! A Cokret channel saved with `mode = "applet"` declares this savfox node
//! as a registered Applet (the Matrix-AppService equivalent in the Cokret
//! universe). The config carries: applet identity, service URL where this
//! node receives `POST /_cokret/edge/applet/transactions`, the Cokret server we
//! write events back to, namespace declarations, and a ghost-DID
//! generation rule.

use std::path::PathBuf;

use anyhow::Context;
use cokret_identifiers::Did;
use serde_json::Value;

use super::namespace::{AppletNamespaces, NamespacePattern};
use crate::cokret::signer::CokretKeyRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretAppletConfig {
    /// Stable identifier in savfox's local channel store.
    pub id: String,
    /// `ck:applet:<uuidv7>` — stable across registrations.
    pub applet_id: String,
    /// Applet service DID (e.g. `did:web:slack-bridge.example`).
    pub service_did: String,
    /// Controller DID that signs the registration (typically `did:webvh:...`).
    pub controller_did: String,
    /// Public URL where this savfox node accepts inbound transactions
    /// (mounted under `/appservices/cokret/{id}/_cokret/edge/applet`).
    pub base_url: String,
    /// Bot actor DID — the visible identity of the applet in Realms it joins
    /// (usually `<service_did>:bot`).
    pub bot_actor_id: String,
    /// Cokret server base URL where outbound events are POSTed.
    pub cokret_server_url: String,
    /// Cokret server service DID used as DID-proof login audience.
    pub cokret_server_did: Option<String>,
    /// Optional bearer for outbound `events_submit` calls. In a fully
    /// signed flow this is replaced by the ghost actor's detached JWS.
    pub cokret_bearer_token: Option<String>,
    /// Namespaces declared in the registration. Used for inbound transaction
    /// filtering and for actor / realm lookup endpoints.
    pub namespaces: AppletNamespaces,
    /// External protocols this Applet bridges (`["slack"]`, `["discord"]`, ...).
    pub protocols: Vec<String>,
    /// Prefix to prepend when minting ghost DIDs (colon path-segment form):
    /// `{service_did}:{ghost_did_prefix}{external_id_slug}`.
    /// Default `"ghost:"` → `did:web:host:ghost:<slug>`.
    pub ghost_did_prefix: String,
    /// `requested_scopes[]` — informational; reducer ignores this and only
    /// honors actual `ck.capability.grant` events.
    pub requested_scopes: Vec<String>,
    /// Whether the Cokret server is expected to push event transactions
    /// (`receive_events: true`). Default `true`.
    pub receive_events: bool,
    /// Whether to receive ephemeral (typing/presence) events. Default `false`.
    pub receive_ephemeral: bool,
    /// Whether the server is permitted to rate-limit transaction pushes.
    pub rate_limited: bool,
    /// Optional `ck.capability.grant` event id this applet currently holds.
    /// When set, outbound events include it as `authorization_ref`.
    pub authorization_grant_id: Option<String>,
    /// Operator-supplied security epoch hash (`sha256:<hex>`) over the
    /// registration evidence (DID Document + signing key + endpoint + auth),
    /// per `applet-schema.md` §1. savfox cannot synthesize it locally; when
    /// absent, [`build_registration_payload`](super::build_registration_payload)
    /// emits a zero placeholder suitable only for an unsigned draft destined
    /// for offline controller signing.
    pub registration_epoch: Option<String>,
    /// Phase 8 (T8.A): ed25519 key for DID-proof login + event signing.
    /// When set, `start_cokret_applet_channel` runs login_did_proof to
    /// obtain the bearer rather than relying on a static `accessToken`.
    pub key_ref: Option<CokretKeyRef>,
    /// Phase 8: verification method id used by the signer. Defaults to
    /// `{bot_actor_id}#key-1` when missing.
    pub verification_method: Option<String>,
    /// Phase 8: path to a pre-signed `ck.capability.grant` Event JSON.
    pub grant_event_path: Option<PathBuf>,
}

impl CokretAppletConfig {
    /// Parse a savfox channel config as an Applet-mode Cokret channel.
    /// Returns `None` if the channel is disabled, of the wrong kind, or
    /// missing the `mode == "applet"` discriminator.
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("cokret") {
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
        let service_did = first_non_empty(raw, &["serviceDid", "service_did"])?;
        let controller_did = first_non_empty(raw, &["controllerDid", "controller_did"])?;
        let base_url = first_non_empty(raw, &["baseUrl", "base_url"])?;
        let bot_actor_id = first_non_empty(raw, &["botActorId", "bot_actor_id"])
            .unwrap_or_else(|| format!("{service_did}:bot"));
        let cokret_server_url =
            first_non_empty(raw, &["cokretServerUrl", "cokret_server_url", "homeserver"])
                .unwrap_or_else(|| base_url.clone());
        let cokret_server_did = first_non_empty(
            raw,
            &[
                "cokretServerDid",
                "cokret_server_did",
                "trustedServerDid",
                "trusted_server_did",
            ],
        );
        let cokret_bearer_token =
            first_non_empty(raw, &["accessToken", "access_token", "cokretBearerToken"]);

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
            .and_then(CokretKeyRef::from_value);
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
            service_did,
            controller_did,
            base_url,
            bot_actor_id,
            cokret_server_url,
            cokret_server_did,
            cokret_bearer_token,
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
            ("service_did", &self.service_did),
            ("controller_did", &self.controller_did),
            ("base_url", &self.base_url),
            ("bot_actor_id", &self.bot_actor_id),
            ("cokret_server_url", &self.cokret_server_url),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("Cokret applet channel '{}' missing {label}", self.id);
            }
        }
        // Strictly parse the DID-typed fields with the SDK parser (not a loose
        // `starts_with("did:")`). This guarantees the invariant relied on by
        // `build_registration_payload`, whose `Did::new(...).expect(...)` would
        // otherwise panic on an input that passed the lenient prefix check.
        for (label, value) in [
            ("service_did", &self.service_did),
            ("controller_did", &self.controller_did),
            ("bot_actor_id", &self.bot_actor_id),
        ] {
            Did::new(value.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Cokret applet channel '{}' {label} must be a valid DID URI, got '{}': {err}",
                    self.id,
                    value
                )
            })?;
        }
        if let Some(value) = self.cokret_server_did.as_deref() {
            Did::new(value.to_owned()).map_err(|err| {
                anyhow::anyhow!(
                    "Cokret applet channel '{}' cokret_server_did must be a valid DID URI, got '{}': {err}",
                    self.id,
                    value
                )
            })?;
        } else if self.key_ref.is_some() {
            anyhow::bail!(
                "Cokret applet channel '{}' has key_ref but no cokret_server_did / cokretServerDid for DID-proof audience",
                self.id
            );
        }
        if self.namespaces.actors.is_empty()
            && self.namespaces.realms.is_empty()
            && self.namespaces.handles.is_empty()
        {
            anyhow::bail!(
                "Cokret applet channel '{}' declares no namespaces; at least one of \
                 actors/realms/handles is required",
                self.id
            );
        }
        if self.protocols.is_empty() {
            anyhow::bail!(
                "Cokret applet channel '{}' declares no protocols (e.g. [\"slack\"])",
                self.id
            );
        }
        if self
            .cokret_bearer_token
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "Cokret applet channel '{}' missing access_token / cokretBearerToken for inbound applet authentication",
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
            Some(NamespacePattern::new(pattern.to_owned(), exclusive))
        })
        .collect()
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

/// Load all configured Cokret applet channels.
pub async fn load_cokret_applet_configs(
    savfox_home: &std::path::PathBuf,
) -> anyhow::Result<Vec<CokretAppletConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for cokret applet")?;
    Ok(all_configs
        .iter()
        .filter_map(CokretAppletConfig::from_channel_config)
        .collect())
}

#[cfg(test)]
mod tests {
    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::json;

    use super::*;

    fn make_channel_config(body: Value) -> ChannelConfig {
        ChannelConfig {
            id: "cokret-applet-test".into(),
            kind: "cokret".into(),
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
            "appletId": "ck:applet:21532600-0000-7000-8000-000000000000",
            "serviceDid": "did:web:slack-bridge.example",
            "controllerDid": "did:webvh:example.com:admin",
            "baseUrl": "https://savfox.example/appservices/cokret/cokret-applet-test",
            "botActorId": "did:web:slack-bridge.example:bot",
            "cokretServerUrl": "https://cokret.example.org",
            "cokretServerDid": "did:webvh:cokret.example.org",
            "accessToken": "applet-bearer-1",
            "protocols": ["slack"],
            "namespaces": {
                "actors": [
                    { "pattern": "did:web:slack-bridge.example:ghost:*", "exclusive": true }
                ],
                "realms": [
                    { "pattern": "slack:team:*:channel:*", "exclusive": true }
                ],
                "handles": [
                    { "pattern": "slack.acme.example/*", "exclusive": false }
                ]
            },
            "requestedScopes": ["ck.flow.create", "ck.message.create"]
        })
    }

    #[test]
    fn parses_full_applet_config() {
        let cfg = make_channel_config(valid_body());
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(
            parsed.applet_id,
            "ck:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(parsed.protocols, vec!["slack"]);
        assert_eq!(
            parsed.cokret_server_did.as_deref(),
            Some("did:webvh:cokret.example.org")
        );
        assert_eq!(parsed.namespaces.actors.len(), 1);
        assert!(parsed.namespaces.actors[0].exclusive);
        assert_eq!(parsed.ghost_did_prefix, "ghost:");
        parsed.validate().expect("validate");
    }

    #[test]
    fn account_mode_returns_none() {
        let mut body = valid_body();
        body["mode"] = json!("account");
        let cfg = make_channel_config(body);
        assert!(CokretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_mode_returns_none() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("mode");
        let cfg = make_channel_config(body);
        assert!(CokretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn disabled_returns_none() {
        let mut cfg = make_channel_config(valid_body());
        cfg.enabled = false;
        assert!(CokretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_applet_id_returns_none() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("appletId");
        let cfg = make_channel_config(body);
        assert!(CokretAppletConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn validate_rejects_no_namespaces() {
        let mut body = valid_body();
        body["namespaces"] = json!({"actors": [], "realms": [], "handles": []});
        let cfg = make_channel_config(body);
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("empty namespaces should fail");
        assert!(err.to_string().contains("namespaces"));
    }

    #[test]
    fn validate_rejects_no_protocols() {
        let mut body = valid_body();
        body["protocols"] = json!([]);
        let cfg = make_channel_config(body);
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed.validate().expect_err("empty protocols should fail");
        assert!(err.to_string().contains("protocols"));
    }

    #[test]
    fn validate_rejects_bad_service_did_scheme() {
        let mut body = valid_body();
        body["serviceDid"] = json!("not-a-did");
        let cfg = make_channel_config(body);
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_inbound_bearer_token() {
        let mut body = valid_body();
        body.as_object_mut()
            .expect("valid body should be an object")
            .remove("accessToken");
        let cfg = make_channel_config(body);
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        let err = parsed
            .validate()
            .expect_err("missing inbound bearer token should fail");
        assert!(err.to_string().contains("inbound applet authentication"));
    }

    #[test]
    fn defaults_receive_flags() {
        let cfg = make_channel_config(valid_body());
        let parsed = CokretAppletConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.receive_events);
        assert!(!parsed.receive_ephemeral);
        assert!(parsed.rate_limited);
    }
}
