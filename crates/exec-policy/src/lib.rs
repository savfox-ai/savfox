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

pub use amend::{AmendError, blocking_append_allow_prefix_rule};
pub use decision::Decision;
pub use error::{Error, ErrorLocation, Result, TextPosition, TextRange};
pub use execpolicycheck::ExecPolicyCheckCommand;
pub use parser::PolicyParser;
pub use policy::{Evaluation, Policy};
pub use rule::{Rule, RuleMatch, RuleRef};
