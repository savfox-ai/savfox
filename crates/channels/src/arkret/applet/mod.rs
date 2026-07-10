//! Arkret Applet + Ghost Actor mode (≈ Matrix AppService).
//!
//! When a savfox channel is saved with `kind = "arkret"` and
//! `config.mode = "applet"`, this module's types take over.
//!
//! Layered like Phase 1 (`mode = "account"`) but oriented around the
//! *bridge* direction:
//!
//! * [`config`] — `ArkretAppletConfig` + parser + namespace block.
//! * [`namespace`] — applet namespace pattern matcher (spec `applet-schema.md` §2 grammar). Mirrors
//!   `arkret::namespace_pattern_matches` but stays self-contained for downstream feature-gating.
//! * [`registration`] — wire-format `ak.applet.registration` JSON builder. Output is unsigned;
//!   controller signs offline.
//! * [`ghost`] — Ghost actor DID minting + profile event content + external_ref builder.
//! * [`outbound`] — Build `ak.message.create` Event attributed to a Ghost Actor.
//! * [`transaction`] — Parse inbound `POST /_arkret/edge/applet/transactions` request bodies into
//!   savfox-side dispatch commands.
//!
//! Phase 6 limitations (see `_arkret_todos.md`):
//!
//! * Events are unsigned (`proofs: []`).
//! * Real `ak.capability.grant` flow is not modeled; the `authorization_ref` is whatever the
//!   operator pre-configured.
//! * No `ak.applet.bridge_error` emission.
//! * No MLS / E2EE.

pub mod config;
pub mod ghost;
pub mod namespace;
pub mod outbound;
pub mod registration;
pub mod runtime_bridge;
pub mod transaction;

pub use config::{
    ArkretAppletConfig, ArkretAppletTrustedVerificationMethod, load_arkret_applet_configs,
};
#[allow(deprecated)]
pub use ghost::build_ghost_profile;
pub use ghost::{build_external_ref, build_ghost_profile_event, mint_ghost_did};
pub use namespace::{
    AppletNamespaces, AppletNamespacesExt, NamespacePattern, NamespacePatternExt,
    namespace_pattern_matches,
};
pub use outbound::{AppletMessageRequest, build_applet_message_event, sign_outbound_event};
pub use registration::{build_registration_json, build_registration_payload};
pub use runtime_bridge::{SavfoxAppletResolver, applet_runtime_config, build_outbound_edge};
pub use transaction::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, classify_inbound_event,
};
