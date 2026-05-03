//! Provider and model runtime domain.
//!
//! This groups provider-selection, provider metadata, model-manager, and
//! remote-model plumbing behind one namespace without breaking existing
//! root-level re-exports.

#[path = "../model_fallback.rs"]
pub mod fallback;
#[path = "../model_identifiers.rs"]
pub(crate) mod identifiers;
#[path = "../model_provider_info.rs"]
pub(crate) mod info;
#[path = "../models_manager.rs"]
pub mod manager;
#[path = "../remote_models.rs"]
pub mod remote;
