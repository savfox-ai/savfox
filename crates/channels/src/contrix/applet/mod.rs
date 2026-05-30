//! Contrix Applet + Ghost Actor mode (≈ Matrix AppService).
//!
//! When a savfox channel is saved with `kind = "contrix"` and
//! `config.mode = "applet"`, this module's types take over.
//!
//! Layered like Phase 1 (`mode = "account"`) but oriented around the
//! *bridge* direction:
//!
//! * [`config`] — `ContrixAppletConfig` + parser + namespace block.
//! * [`namespace`] — applet namespace pattern matcher (spec
//!   `applet-schema.md` §2 grammar). Mirrors `contrix::namespace_pattern_matches`
//!   but stays self-contained for downstream feature-gating.
//! * [`registration`] — wire-format `cx.applet.registration` JSON builder.
//!   Output is unsigned; controller signs offline.
//! * [`ghost`] — Ghost actor DID minting + profile event content +
//!   external_ref builder.
//! * [`outbound`] — Build `cx.message.create` Event attributed to a
//!   Ghost Actor.
//! * [`transaction`] — Parse inbound `POST /api/v1/applet/transactions`
//!   request bodies into savfox-side dispatch commands.
//!
//! Phase 6 limitations (see `_contrix_todos.md`):
//!
//! * Events are unsigned (`proofs: []`).
//! * Real `cx.capability.grant` flow is not modeled; the
//!   `authorization_ref` is whatever the operator pre-configured.
//! * No `cx.applet.bridge_error` emission.
//! * No MLS / E2EE.

pub mod config;
pub mod ghost;
pub mod namespace;
pub mod outbound;
pub mod registration;
pub mod transaction;

pub use config::{ContrixAppletConfig, load_contrix_applet_configs};
#[allow(deprecated)]
pub use ghost::build_ghost_profile;
pub use ghost::{build_external_ref, build_ghost_profile_event, mint_ghost_did};
pub use namespace::{AppletNamespaces, NamespacePattern, namespace_pattern_matches};
pub use outbound::{AppletMessageRequest, build_applet_message_event, sign_outbound_event};
pub use registration::{build_registration_json, build_registration_payload};
pub use transaction::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, classify_inbound_event,
};
