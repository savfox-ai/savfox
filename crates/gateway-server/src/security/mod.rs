//! Security and policy-facing gateway domain.
//!
//! This groups authentication, rate limiting, redaction, SSRF protection, and
//! audit helpers under one namespace while keeping root-level compatibility
//! re-exports for existing call sites.

#[path = "../auth.rs"]
pub mod auth;
#[path = "../rate_limit.rs"]
pub mod rate_limit;
#[path = "../redaction.rs"]
pub mod redaction;
#[path = "../security_audit.rs"]
pub mod security_audit;
#[path = "../ssrf.rs"]
pub mod ssrf;
