//! Security and policy-facing gateway domain.
//!
//! This groups authentication, rate limiting, redaction, SSRF protection, and
//! audit helpers under one namespace while keeping root-level compatibility
//! re-exports for existing call sites.

pub mod auth;
pub mod path_safety;
pub mod rate_limit;
pub mod redaction;
pub mod security_audit;
pub mod ssrf;
