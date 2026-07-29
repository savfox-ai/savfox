//! Security and policy-facing gateway domain.
//!
//! This groups authentication, rate limiting, redaction, SSRF protection, and
//! audit helpers under one namespace while keeping root-level compatibility
//! re-exports for existing call sites.

pub(crate) mod approval_coordinator;
pub mod auth;
pub(crate) mod execution_policy;
pub mod path_safety;
pub mod rate_limit;
pub mod redaction;
pub mod security_audit;
pub mod ssrf;
