//! Contrix v1 channel adapter.
//!
//! Phase 1 scope: **login as already-existing controlled accounts** and
//! exchange messages with the savfox agent. Out of scope: provisioning new
//! Contrix principals, Applet / Ghost Actor patterns, A2A/ACP agent protocol
//! session upgrade, MLS E2EE. See `_contrix.md`, `_contrix_codex.md`, and
//! `_contrix_todos.md` at repo root for design context.
//!
//! Layered like the other channel modules:
//!
//! * [`config`] — typed wrapper for the saved `ChannelConfig` JSON, plus
//!   `load_contrix_channel_configs` + `resolve_contrix_outbound_account`.
//! * [`parse`] — convert one `cx.message.create` Event Envelope (typically
//!   produced by `AccountSubscribeFrame::Delta` traversal) into a savfox
//!   `ContrixInboundEvent` ready for the agent pipeline. Frame parsing itself
//!   lives in the Contrix SDK (`AccountSubscribeFrame::from_ndjson_line`).
//! * [`client`] — thin wrapper around [`contrix_http_client::Client`] with
//!   the bearer token attached.
//! * [`outbound`] — build an unsigned `cx.message.create` Event from
//!   `(realm_id, flow_id, body, actor)`. Signing is left as a TODO until the
//!   SDK exposes a high-level signer; in the meantime the bearer
//!   `cx.session.grant` is the auth credential.

pub mod applet;
mod client;
mod config;
mod grant;
mod outbound;
mod parse;
mod session;
mod signer;

pub use grant::{ContrixGrant, load_and_verify_grant};
pub use session::{ContrixSession, login_with_signer};
pub use signer::{ContrixKeyRef, load_ed25519_signer};

#[allow(deprecated)]
pub use applet::build_ghost_profile;
pub use applet::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, AppletMessageRequest,
    AppletNamespaces, ContrixAppletConfig, NamespacePattern, build_applet_message_event,
    build_external_ref, build_ghost_profile_event, build_registration_json,
    build_registration_payload, classify_inbound_event, load_contrix_applet_configs,
    mint_ghost_did, namespace_pattern_matches,
};
pub use client::{ContrixFrameStream, ContrixHttpClient};
pub use config::{
    ContrixAccountConfig, ContrixChannelConfig, load_contrix_channel_configs,
    resolve_contrix_outbound_account,
};
pub use outbound::{MessageCreateRequest, build_message_create_event, sign_outbound_event};
pub use parse::{
    ContrixInboundEvent, ContrixInboundParseResult, extract_message_event,
    parse_delta_frame_for_account, should_dispatch_event,
};
