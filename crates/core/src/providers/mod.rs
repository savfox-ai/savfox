//! Provider and model runtime domain.
//!
//! This groups provider-selection, provider metadata, model-manager, and
//! remote-model plumbing behind one namespace without breaking existing
//! root-level re-exports.

pub mod fallback;
pub(crate) mod identifiers;
pub(crate) mod info;
pub mod manager;
pub mod remote;
