//! Bridge between savfox's [`ArkretAppletConfig`] and the reusable
//! `arkret-bridge-runtime` crate.
//!
//! The runtime crate factors out the outbound mint bookkeeping (monotonic
//! `actor_seq`, HLC generation, signing, and applet metadata transport tagging)
//! and the inbound actor/realm resolution probes. savfox owns the
//! *configuration* and the
//! *signer* (loaded from a [`ArkretKeyRef`] via [`load_ed25519_signer`]), so the
//! glue here is purely:
//!
//! * [`applet_runtime_config`] — translate a savfox applet config into the runtime's
//!   [`Config`](arkret_bridge_runtime::Config). The pure mapping leaves the signing seed empty;
//!   [`build_outbound_edge`] fills it from the configured
//!   [`ArkretKeyRef`](crate::arkret::ArkretKeyRef) before handing the config to the runtime.
//! * [`SavfoxAppletResolver`] — answer `resolve_actor` / `resolve_realm` probes from the applet's
//!   declared namespaces.
//! * [`build_outbound_edge`] — assemble a ready-to-use
//!   [`ArkretEdge`](arkret_bridge_runtime::ArkretEdge).

use std::sync::Arc;

use arkret_bridge_runtime::{
    AppletResolver, CokretEdge as ArkretEdge, Config, ResolvedActor, ResolvedRealm, SeqStore,
};
use serde_json::json;

use crate::arkret::applet::config::ArkretAppletConfig;
use crate::arkret::applet::namespace::{AppletNamespaces, AppletNamespacesExt};

/// Default verification method fragment when the applet config leaves it unset.
const DEFAULT_VERIFICATION_METHOD: &str = "#key-1";

/// Translate a savfox [`ArkretAppletConfig`] into a runtime
/// [`Config`](arkret_bridge_runtime::Config).
///
/// Field mapping (savfox → runtime):
///
/// * `cfg.arkret_server_url`              → `arkret.server_url`
/// * `cfg.arkret_server_did`              → `arkret.trusted_server_did`
/// * `cfg.service_did`                    → `arkret.service_did`
/// * `cfg.applet_id`                      → `arkret.applet_id`
/// * `cfg.arkret_bearer_token` (or "")    → `arkret.access_token`
/// * `cfg.verification_method` (or "#key-1") → `arkret.verification_method_id`
/// * `""` (placeholder)                   → `arkret.signing_key_seed_hex`
/// * `cfg.id`                             → `bridge.bridge_id`
///
/// `bridge` gets sensible defaults for the transport fields (the runtime
/// server is not actually bound by savfox — inbound flows in through savfox's
/// own appservice mount), and `database` / `logging` use their `Default`. The
/// `app` adapter section is `Null`.
///
/// The runtime keeps its `BridgeConfig` / `ArkretConfig` field structs private
/// (only the top-level [`Config`] is re-exported), so we assemble the config
/// through its `Deserialize` impl from a JSON document rather than naming the
/// inner types. `database` / `logging` / `app` are omitted and fall back to the
/// runtime's `#[serde(default)]` values; the `bridge` transport fields likewise
/// take their defaults (the runtime server is not bound by savfox — inbound
/// flows through savfox's own appservice mount).
///
/// # Panics
///
/// Never in practice: the document is built from owned, always-valid strings and
/// matches the runtime `Config` schema exactly. The `expect` guards against a
/// future schema drift, which would surface immediately in tests/CI.
#[must_use]
pub fn applet_runtime_config(cfg: &ArkretAppletConfig) -> Config {
    let verification_method_id = cfg
        .verification_method
        .clone()
        .unwrap_or_else(|| DEFAULT_VERIFICATION_METHOD.to_owned());
    let trusted_server_did = cfg
        .arkret_server_did
        .clone()
        .unwrap_or_else(|| cfg.service_did.clone());
    let doc = json!({
        "bridge": {
            "bridge_id": cfg.id,
            "port": 9100,
            "bind_address": "0.0.0.0",
        },
        "arkret": {
            "server_url": cfg.arkret_server_url,
            "service_did": cfg.service_did,
            "applet_id": cfg.applet_id,
            "access_token": cfg.arkret_bearer_token.clone().unwrap_or_default(),
            "trusted_server_did": trusted_server_did,
            // Placeholder: build_outbound_edge fills this from key_ref before
            // constructing a runtime edge.
            "signing_key_seed_hex": "",
            "verification_method_id": verification_method_id,
        },
        "app": serde_json::Value::Null,
    });
    serde_json::from_value(doc).expect("applet_runtime_config: runtime Config schema drift")
}

/// Answers the applet `resolve_actor` / `resolve_realm` probes using the
/// applet's declared namespace patterns.
///
/// Holds a clone of the relevant config fields (rather than borrowing the
/// config) so it can be installed as an `Arc<dyn AppletResolver>` with a
/// `'static` lifetime.
#[derive(Debug)]
pub struct SavfoxAppletResolver {
    namespaces: AppletNamespaces,
    service_did: String,
    ghost_did_prefix: String,
}

impl SavfoxAppletResolver {
    /// Build a resolver from the applet config.
    #[must_use]
    pub fn new(cfg: &ArkretAppletConfig) -> Self {
        Self {
            namespaces: cfg.namespaces.clone(),
            service_did: cfg.service_did.clone(),
            ghost_did_prefix: cfg.ghost_did_prefix.clone(),
        }
    }
}

impl AppletResolver for SavfoxAppletResolver {
    fn resolve_actor(&self, actor_id: &str) -> Option<ResolvedActor> {
        if !self.namespaces.actor_matches(actor_id) {
            return None;
        }
        Some(ResolvedActor {
            actor_id: actor_id.to_owned(),
            display_name: None,
            external_ref: json!({
                "managed_by": self.service_did,
                "ghost_did_prefix": self.ghost_did_prefix,
            }),
        })
    }

    fn resolve_realm(&self, realm_id_or_alias: &str) -> Option<ResolvedRealm> {
        if !self.namespaces.realm_matches(realm_id_or_alias) {
            return None;
        }
        Some(ResolvedRealm {
            realm_id: realm_id_or_alias.to_owned(),
            title: None,
            external_ref: json!({
                "managed_by": self.service_did,
            }),
        })
    }
}

/// Build a ready-to-use outbound [`ArkretEdge`](arkret_bridge_runtime::ArkretEdge)
/// for `cfg`, injecting a `seq` store.
///
/// The runtime config is derived from `cfg` via [`applet_runtime_config`], then
/// its `signing_key_seed_hex` placeholder is filled from `cfg.key_ref` because
/// current `arkret-bridge-runtime` constructs its own signer inside
/// [`ArkretEdge::new`]. The runtime [`BridgeError`] is mapped into `anyhow` for
/// savfox callers.
pub fn build_outbound_edge(
    cfg: &ArkretAppletConfig,
    seq: Arc<dyn SeqStore>,
) -> anyhow::Result<ArkretEdge> {
    let mut config = applet_runtime_config(cfg);
    let key_ref = cfg.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "arkret applet '{}' cannot build runtime edge without key_ref",
            cfg.id
        )
    })?;
    config.arkret.signing_key_seed_hex = crate::arkret::load_ed25519_seed_hex(key_ref)?;
    let (inbound_tx, _inbound_rx) = tokio::sync::mpsc::channel(1);
    ArkretEdge::new(Arc::new(config), seq, inbound_tx)
        .map_err(|e| anyhow::anyhow!("arkret runtime edge: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arkret::applet::namespace::NamespacePattern;

    fn make_config() -> ArkretAppletConfig {
        ArkretAppletConfig {
            id: "arkret-applet-test".to_owned(),
            applet_id: "ak:applet:21532600-0000-7000-8000-000000000000".to_owned(),
            service_did: "did:web:slack-bridge.example".to_owned(),
            controller_did: "did:webvh:example.com:admin".to_owned(),
            base_url: "https://savfox.example/appservices/arkret/arkret-applet-test".to_owned(),
            bot_actor_id: "did:web:slack-bridge.example:bot".to_owned(),
            device_id: None,
            arkret_server_url: "https://arkret.example.org".to_owned(),
            arkret_server_did: Some("did:webvh:arkret.example.org".to_owned()),
            trusted_verification_methods: Vec::new(),
            login_challenge: None,
            arkret_bearer_token: Some("applet-bearer-1".to_owned()),
            namespaces: AppletNamespaces {
                actors: vec![NamespacePattern::exclusive(
                    "did:web:slack-bridge.example:ghost:*",
                )],
                realms: vec![NamespacePattern::exclusive("slack:team:*:channel:*")],
                handles: vec![],
            },
            protocols: vec!["slack".to_owned()],
            ghost_did_prefix: "ghost:".to_owned(),
            requested_scopes: vec![],
            receive_events: true,
            receive_ephemeral: false,
            rate_limited: true,
            authorization_grant_id: None,
            registration_epoch: None,
            key_ref: None,
            verification_method: None,
            grant_event_path: None,
        }
    }

    #[test]
    fn runtime_config_maps_fields() {
        let cfg = make_config();
        let rc = applet_runtime_config(&cfg);

        assert_eq!(rc.bridge.bridge_id, "arkret-applet-test");
        assert_eq!(rc.arkret.server_url, "https://arkret.example.org");
        assert_eq!(rc.arkret.trusted_server_did, "did:webvh:arkret.example.org");
        assert_eq!(rc.arkret.service_did, "did:web:slack-bridge.example");
        assert_eq!(
            rc.arkret.applet_id,
            "ak:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(rc.arkret.access_token, "applet-bearer-1");
        // Defaulted verification method when config leaves it unset.
        assert_eq!(rc.arkret.verification_method_id, "#key-1");
        // Placeholder seed in the pure config mapping; build_outbound_edge
        // fills it from key_ref before constructing a runtime edge.
        assert!(rc.arkret.signing_key_seed_hex.is_empty());
        assert!(rc.app.is_null());
    }

    #[test]
    fn runtime_config_uses_explicit_verification_method() {
        let mut cfg = make_config();
        cfg.verification_method = Some("did:web:slack-bridge.example#bridge-key".to_owned());
        let rc = applet_runtime_config(&cfg);
        assert_eq!(
            rc.arkret.verification_method_id,
            "did:web:slack-bridge.example#bridge-key"
        );
    }

    #[test]
    fn runtime_config_empty_access_token_when_missing() {
        let mut cfg = make_config();
        cfg.arkret_bearer_token = None;
        let rc = applet_runtime_config(&cfg);
        assert!(rc.arkret.access_token.is_empty());
    }

    #[test]
    fn resolver_matches_in_namespace_actor() {
        let resolver = SavfoxAppletResolver::new(&make_config());
        let resolved = resolver
            .resolve_actor("did:web:slack-bridge.example:ghost:u123")
            .expect("in-namespace actor should resolve");
        assert_eq!(resolved.actor_id, "did:web:slack-bridge.example:ghost:u123");
        assert_eq!(
            resolved.external_ref["managed_by"],
            "did:web:slack-bridge.example"
        );
    }

    #[test]
    fn resolver_rejects_out_of_namespace_actor() {
        let resolver = SavfoxAppletResolver::new(&make_config());
        assert!(
            resolver
                .resolve_actor("did:web:other.example:ghost:u123")
                .is_none()
        );
        // The bot actor is not in the ghost:* actor namespace.
        assert!(
            resolver
                .resolve_actor("did:web:slack-bridge.example:bot")
                .is_none()
        );
    }

    #[test]
    fn resolver_matches_in_namespace_realm() {
        let resolver = SavfoxAppletResolver::new(&make_config());
        let resolved = resolver
            .resolve_realm("slack:team:T123:channel:C456")
            .expect("in-namespace realm should resolve");
        assert_eq!(resolved.realm_id, "slack:team:T123:channel:C456");
    }

    #[test]
    fn resolver_rejects_out_of_namespace_realm() {
        let resolver = SavfoxAppletResolver::new(&make_config());
        assert!(
            resolver
                .resolve_realm("discord:guild:G1:channel:C1")
                .is_none()
        );
    }
}
