//! Arkret Applet Service management mode.
//!
//! When a savfox channel is saved with `kind = "arkret"` and
//! `config.mode = "applet"`, this module's types take over.
//!
//! * [`config`] — `ArkretAppletConfig` + parser + namespace block.
//! * [`namespace`] — aliases for SDK namespace DTOs and their canonical matcher.
//! Group Events and Signals are consumed by authenticated managed Devices;
//! the Applet management endpoint only accepts signed authoring completions.
//!
//! A Service installation may have no Bot. Each configured execution Account
//! must be provisioned separately. The management URL never supplies group
//! Events or Signals; the restricted Device reader is not wired for Applet mode.

pub mod config;
pub mod namespace;

pub use config::{
    ArkretAppletConfig, ArkretAppletTrustedVerificationMethod, load_arkret_applet_configs,
};
pub use namespace::{
    AppletNamespaces, AppletNamespacesExt, NamespacePattern, NamespacePatternExt,
    namespace_pattern_matches,
};
