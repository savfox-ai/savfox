//! Cokret v1 channel adapter.
//!
//! Supports two modes:
//! * **agent** — personal agent runtime configuration seeded from a Yougen bootstrap. Runtime keys
//!   use `agent_key_proof` to mint short-lived DPoP-bound `ck.session.grant` credentials for
//!   protected self endpoints.
//! * **applet** — register this node as a Cokret Applet (the Matrix-AppService equivalent), with
//!   Ghost Actor minting and Ed25519 event signing (see the [`applet`] and [`signer`] submodules).
//!
//! Principal provisioning is performed by Yougen/Cokret; this adapter consumes the resulting
//! bootstrap and runtime-key authorization. MLS/E2EE membership for agent runtimes remains a
//! separate integration path.
//!
//! Layered like the other channel modules:
//!
//! * [`config`] — typed wrapper for the saved `ChannelConfig` JSON, plus
//!   `load_cokret_channel_configs` + `resolve_cokret_outbound_account`.
//! * [`parse`] — convert one `ck.message.create` Event Envelope or account notification delta
//!   (typically produced by `AccountSubscribeFrame::Delta` traversal) into a savfox
//!   `CokretInboundEvent` ready for the agent pipeline. Frame parsing itself lives in the Cokret
//!   SDK (`AccountSubscribeFrame::from_ndjson_line`).
//! * [`client`] — thin wrapper around [`cokret::http_client::Client`]. Agent mode must use a
//!   DPoP-bound client.
//! * [`outbound`] — build a `ck.message.create` Event from `(realm_id, flow_id, body, actor)`. When
//!   the account/applet has an `ed25519` `key_ref`, the event is signed via [`signer`].
//! * [`signer`] — load an Ed25519 signing key and attach detached-JWS proofs to outbound events.

pub mod applet;
mod client;
mod config;
mod crypto_state;
mod grant;
mod outbound;
mod parse;
mod seq_store;
mod session;
mod signer;

#[allow(deprecated)]
pub use applet::build_ghost_profile;
pub use applet::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, AppletMessageRequest,
    AppletNamespaces, CokretAppletConfig, NamespacePattern, SavfoxAppletResolver,
    applet_runtime_config, build_applet_message_event, build_external_ref,
    build_ghost_profile_event, build_outbound_edge, build_registration_json,
    build_registration_payload, classify_inbound_event, load_cokret_applet_configs, mint_ghost_did,
    namespace_pattern_matches,
};
pub use client::{CokretAccountFrameStream, CokretFrameStream, CokretHttpClient};
pub use config::{
    CokretAccountConfig, CokretAccountMode, CokretChannelConfig,
    build_cokret_runtime_key_request_json, derive_cokret_device_id, load_cokret_channel_configs,
    resolve_cokret_outbound_account,
};
pub use crypto_state::{
    CokretBootstrapRecord, CokretContentEncryptionFloor, CokretCryptoStateFile,
    CokretDecryptOutcome, CokretEncryptOutcome, CokretKeyBackupState, CokretRealmCryptoPolicy,
    FileCokretCryptoStore, account_scope_id, applet_scope_id,
    extract_encrypted_payload_from_message_content, message_content_has_encrypted_carrier,
};
pub use grant::{CokretGrant, load_and_verify_grant};
pub use outbound::{MessageCreateRequest, build_message_create_event, sign_outbound_event};
pub use parse::{
    CokretInboundEvent, CokretInboundEventOutcome, CokretInboundParseResult,
    CokretInboundSkipReason, CokretInboundSkippedEvent, account_allows_event_read,
    classify_message_event, extract_message_event, parse_delta_frame_for_account,
    parse_event_frame_for_account, parse_notification_delta_for_account, should_dispatch_event,
};
pub use seq_store::FileSeqStore;
pub use session::{CokretSession, login_with_signer};
pub use signer::{CokretKeyRef, load_ed25519_seed_hex, load_ed25519_signer};
