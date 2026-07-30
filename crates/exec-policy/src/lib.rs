//! Allow / forbid / ask policy DSL for shell command execution.
//!
//! `exec-policy` is the security boundary that decides, per command line,
//! whether to run the command, refuse it, or escalate to operator approval.
//! Each rule is compiled from a small declarative DSL (see [`parser`])
//! into a [`policy::ExecPolicy`] that can be evaluated against a parsed
//! command via [`execpolicycheck`].
//!
//! Three outcomes are possible per command (see [`decision::Decision`]):
//!
//! * `Allow` — run unattended.
//! * `Forbid` — refuse and surface the matching rule's reason to the model so it can adjust.
//! * `Ask` — require the operator's interactive approval.
//!
//! Rule sets are layered: project-level rules under `.savfox/rules/`,
//! user-level rules under `~/.savfox/rules/`, and built-in safety rules.
//! The [`amend`] module composes the layers and resolves precedence.

#![allow(unsafe_code)]
#![allow(missing_debug_implementations)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod amend;
pub mod decision;
pub mod error;
pub mod execpolicycheck;
pub mod parser;
pub mod policy;
pub mod rule;

pub use amend::{
    AmendError, blocking_append_allow_prefix_rule, blocking_append_prefix_rule,
    blocking_remove_prefix_rule,
};
pub use decision::Decision;
pub use error::{Error, ErrorLocation, Result, TextPosition, TextRange};
pub use execpolicycheck::ExecPolicyCheckCommand;
pub use parser::PolicyParser;
pub use policy::{Evaluation, Policy};
pub use rule::{Rule, RuleMatch, RuleRef};
