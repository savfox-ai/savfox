//! Arkret v1 channel adapter.
//!
//! Supports two modes:
//! * **agent** — personal agent runtime configuration seeded from an Inkson bootstrap. Runtime keys
//!   use `agent_key_proof` to mint short-lived DPoP-bound `ak.session.grant` credentials for
//!   protected self endpoints.
//! * **applet** — register this node as an Arkret Applet (the Matrix-AppService equivalent), with
//!   Ghost Actor minting and Ed25519 event signing (see the [`applet`] and [`signer`] submodules).
//!
//! Principal provisioning is performed by Inkson/Arkret; this adapter consumes the resulting
//! bootstrap and runtime-key authorization. Native Agent MLS identities reuse that authorized
//! runtime key so KeyPackage claims bind to the same accepted `ak.agent.key.authorize` chain.
//!
//! Layered like the other channel modules:
//!
//! * [`config`] — typed wrapper for the saved `ChannelConfig` JSON, plus
//!   `load_arkret_channel_configs` + `resolve_arkret_outbound_account_for_config`.
//! * [`inbound_adapter`] — adapt Arkret SDK-decoded inbound Events and account notification deltas
//!   into a savfox `ArkretInboundEvent` ready for the agent pipeline. Frame parsing itself lives in
//!   the Arkret SDK (`AccountSubscribeFrame::from_ndjson_line`).
//! * [`client`] — thin wrapper around [`arkret::http_client::Client`]. Agent mode must use a
//!   DPoP-bound client.
//! * [`outbound`] — build a `ak.message.create` Event from `(realm_id, strand_id, body, actor)`.
//!   When the account/applet has an `ed25519` `key_ref`, the event is signed via [`signer`].
//! * [`signer`] — load an Ed25519 signing key and attach detached-JWS proofs to outbound events.

mod account_store;
pub mod applet;
mod client;
mod config;
mod crypto_state;
mod grant;
mod inbound_adapter;
mod outbound;
mod session;
mod sidecar;
mod signer;

pub use account_store::{delete_account_store, device_messages_scope, open_account_store};
pub use applet::{
    AppletDispatchSkip, AppletEventOutcome, AppletInboundCommand, AppletNamespaces,
    AppletNamespacesExt, ArkretAppletConfig, NamespacePattern, NamespacePatternExt,
    classify_inbound_event, load_arkret_applet_configs, mint_ghost_did, namespace_pattern_matches,
};
pub use arkret_crypto::UnableToDecryptReason;
pub use arkret_wire::EventInitialSubmission;
pub use client::{
    ArkretAccountFrameStream, ArkretAgentSessionProvider, ArkretFrameStream, ArkretHttpClient,
    ArkretInitialSubmissionProvider, PrincipalServerInitialSubmissionProvider,
    SavfoxArkretClientCore, SavfoxDurableArkretClientCore, agent_session_exchange_reason,
    agent_session_reason_is_irreversibly_terminal, build_mls_key_packages_claim_request,
    sign_mls_welcome_claim_envelope,
};
pub use config::{
    ArkretAccountConfig, ArkretAccountMode, ArkretChannelConfig, DEFAULT_AGENT_RUNTIME_SCOPE,
    VerifiedArkretRuntimeScope, build_arkret_runtime_key_request_json,
    build_arkret_runtime_key_status_request_json, delete_verified_runtime_scope,
    derive_arkret_device_id, duplicate_requested_scope_actions, load_arkret_channel_configs,
    load_verified_runtime_scope, missing_required_scope_actions,
    resolve_arkret_outbound_account_for_config, save_verified_runtime_scope,
    unknown_requested_scope_actions,
};
pub use crypto_state::{
    ArkretBootstrapRecord, ArkretContentEncryptionFloor, ArkretCryptoStateFile,
    ArkretDecryptDetailedOutcome, ArkretDecryptOutcome, ArkretEncryptOutcome, ArkretKeyBackupState,
    ArkretMlsIdentityStateRecord, ArkretMlsWelcomeConsumeBinding, ArkretRealmCryptoPolicy,
    FileArkretCryptoStore, account_scope_id, applet_scope_id,
    extract_encrypted_metadata_payload_from_message_content,
    extract_encrypted_payload_from_message_content, extract_mls_welcome_consume_binding,
    extract_mls_welcome_envelope, message_content_has_encrypted_carrier,
    mls_key_package_record_from_claim,
};
pub use grant::{ArkretGrant, load_and_verify_grant};
pub use inbound_adapter::{
    ArkretInboundEvent, ArkretInboundEventOutcome, ArkretInboundParseResult,
    ArkretInboundSkipReason, ArkretInboundSkippedEvent, account_allows_event_read,
    classify_message_event, extract_message_event, parse_delta_frame_for_account,
    parse_notification_delta_for_account, should_dispatch_event,
};
pub use outbound::{
    MessageCreateRequest, apply_data_event_basis, build_message_create_event, sign_outbound_event,
};
pub use session::{ArkretSession, login_with_signer};
pub use sidecar::{
    EVENT_REF_ROLE_AFTER, SidecarExchangeAdmission, SidecarExchangeContext, SidecarExchangeStore,
    SidecarRequestGate, build_user_facing_response_metadata, encode_sidecar_reply_target,
    gate_inbound_request_binding, sidecar_binding_from_metadata_plaintext,
    split_sidecar_reply_target,
};
pub use signer::{
    ArkretKeyRef, ed25519_runtime_public_key_digest, generate_ed25519_key_ref_in_keyring,
    get_or_generate_ed25519_key_ref_in_keyring, load_ed25519_seed_hex, load_ed25519_signer,
    sign_keypackages_consume_request, sign_keypackages_revoke_request,
    sign_keypackages_upload_request,
};
