//! Bridge between savfox's [`CokretAppletConfig`] and the reusable
//! `cokret-bridge-runtime` crate.
//!
//! The runtime crate factors out the outbound mint bookkeeping (monotonic
//! `actor_seq`, HLC generation, signing, and applet metadata transport tagging)
//! and the inbound actor/realm resolution probes. savfox owns the
//! *configuration* and the
//! *signer* (loaded from a [`CokretKeyRef`] via [`load_ed25519_signer`]), so the
//! glue here is purely:
//!
//! * [`applet_runtime_config`] — translate a savfox applet config into the runtime's
//!   [`Config`](cokret_bridge_runtime::Config). The pure mapping leaves the signing seed empty;
//!   [`build_outbound_edge`] fills it from the configured
//!   [`CokretKeyRef`](crate::cokret::CokretKeyRef) before handing the config to the runtime.
//! * [`SavfoxAppletResolver`] — answer `resolve_actor` / `resolve_realm` probes from the applet's
//!   declared namespaces.
//! * [`build_outbound_edge`] — assemble a ready-to-use
//!   [`CokretEdge`](cokret_bridge_runtime::CokretEdge).

use std::sync::Arc;

use cokret_bridge_runtime::{
    AppletResolver, CokretEdge, Config, ResolvedActor, ResolvedRealm, SeqStore,
};
use serde_json::json;

use crate::cokret::applet::config::CokretAppletConfig;
use crate::cokret::applet::namespace::{AppletNamespaces, AppletNamespacesExt};

/// Default verification method fragment when the applet config leaves it unset.
const DEFAULT_VERIFICATION_METHOD: &str = "#key-1";

/// Translate a savfox [`CokretAppletConfig`] into a runtime
/// [`Config`](cokret_bridge_runtime::Config).
///
/// Field mapping (savfox → runtime):
///
/// * `cfg.cokret_server_url`              → `cokret.server_url`
/// * `cfg.cokret_server_did`              → `cokret.trusted_server_did`
/// * `cfg.service_did`                    → `cokret.service_did`
/// * `cfg.applet_id`                      → `cokret.applet_id`
/// * `cfg.cokret_bearer_token` (or "")    → `cokret.access_token`
/// * `cfg.verification_method` (or "#key-1") → `cokret.verification_method_id`
/// * `""` (placeholder)                   → `cokret.signing_key_seed_hex`
/// * `cfg.id`                             → `bridge.bridge_id`
///
/// `bridge` gets sensible defaults for the transport fields (the runtime
/// server is not actually bound by savfox — inbound flows in through savfox's
/// own appservice mount), and `database` / `logging` use their `Default`. The
/// `app` adapter section is `Null`.
///
/// The runtime keeps its `BridgeConfig` / `CokretConfig` field structs private
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
pub fn applet_runtime_config(cfg: &CokretAppletConfig) -> Config {
    let verification_method_id = cfg
        .verification_method
        .clone()
        .unwrap_or_else(|| DEFAULT_VERIFICATION_METHOD.to_owned());
    let trusted_server_did = cfg
        .cokret_server_did
        .clone()
        .unwrap_or_else(|| cfg.service_did.clone());
    let doc = json!({
        "bridge": {
            "bridge_id": cfg.id,
            "port": 9100,
            "bind_address": "0.0.0.0",
        },
        "cokret": {
            "server_url": cfg.cokret_server_url,
            "service_did": cfg.service_did,
            "applet_id": cfg.applet_id,
            "access_token": cfg.cokret_bearer_token.clone().unwrap_or_default(),
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
    pub fn new(cfg: &CokretAppletConfig) -> Self {
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

/// Build a ready-to-use outbound [`CokretEdge`](cokret_bridge_runtime::CokretEdge)
/// for `cfg`, injecting a `seq` store.
///
/// The runtime config is derived from `cfg` via [`applet_runtime_config`], then
/// its `signing_key_seed_hex` placeholder is filled from `cfg.key_ref` because
/// current `cokret-bridge-runtime` constructs its own signer inside
/// [`CokretEdge::new`]. The runtime [`BridgeError`] is mapped into `anyhow` for
/// savfox callers.
pub fn build_outbound_edge(
    cfg: &CokretAppletConfig,
    seq: Arc<dyn SeqStore>,
) -> anyhow::Result<CokretEdge> {
    let mut config = applet_runtime_config(cfg);
    let key_ref = cfg.key_ref.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "cokret applet '{}' cannot build runtime edge without key_ref",
            cfg.id
        )
    })?;
    config.cokret.signing_key_seed_hex = crate::cokret::load_ed25519_seed_hex(key_ref)?;
    let (inbound_tx, _inbound_rx) = tokio::sync::mpsc::channel(1);
    CokretEdge::new(Arc::new(config), seq, inbound_tx)
        .map_err(|e| anyhow::anyhow!("cokret runtime edge: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cokret::applet::namespace::NamespacePattern;

    fn make_config() -> CokretAppletConfig {
        CokretAppletConfig {
            id: "cokret-applet-test".to_owned(),
            applet_id: "ck:applet:21532600-0000-7000-8000-000000000000".to_owned(),
            service_did: "did:web:slack-bridge.example".to_owned(),
            controller_did: "did:webvh:example.com:admin".to_owned(),
            base_url: "https://savfox.example/appservices/cokret/cokret-applet-test".to_owned(),
            bot_actor_id: "did:web:slack-bridge.example:bot".to_owned(),
            device_id: None,
            cokret_server_url: "https://cokret.example.org".to_owned(),
            cokret_server_did: Some("did:webvh:cokret.example.org".to_owned()),
            trusted_verification_methods: Vec::new(),
            login_challenge: None,
            cokret_bearer_token: Some("applet-bearer-1".to_owned()),
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

        assert_eq!(rc.bridge.bridge_id, "cokret-applet-test");
        assert_eq!(rc.cokret.server_url, "https://cokret.example.org");
        assert_eq!(rc.cokret.trusted_server_did, "did:webvh:cokret.example.org");
        assert_eq!(rc.cokret.service_did, "did:web:slack-bridge.example");
        assert_eq!(
            rc.cokret.applet_id,
            "ck:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(rc.cokret.access_token, "applet-bearer-1");
        // Defaulted verification method when config leaves it unset.
        assert_eq!(rc.cokret.verification_method_id, "#key-1");
        // Placeholder seed in the pure config mapping; build_outbound_edge
        // fills it from key_ref before constructing a runtime edge.
        assert!(rc.cokret.signing_key_seed_hex.is_empty());
        assert!(rc.app.is_null());
    }

    #[test]
    fn runtime_config_uses_explicit_verification_method() {
        let mut cfg = make_config();
        cfg.verification_method = Some("did:web:slack-bridge.example#bridge-key".to_owned());
        let rc = applet_runtime_config(&cfg);
        assert_eq!(
            rc.cokret.verification_method_id,
            "did:web:slack-bridge.example#bridge-key"
        );
    }

    #[test]
    fn runtime_config_empty_access_token_when_missing() {
        let mut cfg = make_config();
        cfg.cokret_bearer_token = None;
        let rc = applet_runtime_config(&cfg);
        assert!(rc.cokret.access_token.is_empty());
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
