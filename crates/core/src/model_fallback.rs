//! Model fallback strategy: automatically try alternative models when the primary fails.
//!
//! When a request to the primary model fails with a retryable error (rate limit, timeout,
//! server unavailability), the [`FallbackResolver`] walks an ordered chain of
//! [`FallbackEntry`] items to pick the next model to try. Non-retryable errors such as
//! authentication failures or bad requests short-circuit the chain immediately.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Trigger & entry types
// ---------------------------------------------------------------------------

/// Condition under which a fallback entry should be considered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTrigger {
    /// Model returned a rate-limit error (HTTP 429).
    RateLimited,
    /// Request timed out before a response was received.
    Timeout,
    /// Model is unavailable (HTTP 5xx or connection refused).
    Unavailable,
    /// Matches *any* retryable error.
    AnyError,
}

/// A single entry in the fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEntry {
    /// Model identifier to fall back to (e.g. `"gpt-4o-mini"`).
    pub model: String,
    /// Trigger conditions for this entry.
    pub triggers: Vec<FallbackTrigger>,
    /// Maximum retries on *this* fallback model before moving to the next.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Delay in milliseconds before attempting this fallback.
    #[serde(default)]
    pub delay_ms: u64,
}

fn default_max_retries() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// Top-level fallback strategy configuration, typically stored in config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackStrategy {
    /// Master switch. When `false` the resolver always returns `None`.
    #[serde(default)]
    pub enabled: bool,
    /// Ordered list of fallback models, tried in sequence.
    #[serde(default)]
    pub chain: Vec<FallbackEntry>,
    /// Hard cap on the total number of attempts (primary + fallbacks).
    #[serde(default = "default_max_attempts")]
    pub max_total_attempts: u32,
}

fn default_max_attempts() -> u32 {
    3
}

impl Default for FallbackStrategy {
    fn default() -> Self {
        Self {
            enabled: false,
            chain: Vec::new(),
            max_total_attempts: default_max_attempts(),
        }
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Classification of an upstream error for fallback-routing purposes.
#[derive(Debug, Clone)]
pub enum ErrorClass {
    /// HTTP 429 — optionally includes `Retry-After`.
    RateLimited { retry_after: Option<Duration> },
    /// The request timed out.
    Timeout,
    /// Server error / unreachable.
    Unavailable,
    /// Authentication failure (HTTP 401/403). Never retryable.
    AuthError,
    /// Client-side bad request (HTTP 400). Never retryable.
    BadRequest,
    /// Anything else.
    Other(String),
}

impl ErrorClass {
    /// Returns `true` when this error matches the given trigger.
    #[must_use] 
    pub fn matches(&self, trigger: &FallbackTrigger) -> bool {
        matches!(
            (self, trigger),
            (Self::RateLimited { .. }, FallbackTrigger::RateLimited)
                | (Self::Timeout, FallbackTrigger::Timeout)
                | (Self::Unavailable, FallbackTrigger::Unavailable)
                | (_, FallbackTrigger::AnyError)
        )
    }

    /// Whether this error class is retryable at all. Auth and bad-request
    /// errors are terminal — switching models would not help.
    #[must_use] 
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::AuthError | Self::BadRequest)
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolves which model (if any) to try next after a failure.
#[derive(Debug)]
pub struct FallbackResolver {
    strategy: FallbackStrategy,
}

impl FallbackResolver {
    /// Create a new resolver from the given strategy.
    #[must_use] 
    pub fn new(strategy: FallbackStrategy) -> Self {
        Self { strategy }
    }

    /// Given the current zero-based attempt index and the error that occurred,
    /// return the next [`FallbackDecision`] or `None` if no more fallbacks are
    /// available.
    ///
    /// `attempt` counts how many attempts have already been made (0 = just the
    /// primary failed). `primary_model` is used only for logging.
    pub fn next_model(
        &self,
        attempt: u32,
        error: &ErrorClass,
        primary_model: &str,
    ) -> Option<FallbackDecision> {
        if !self.strategy.enabled {
            return None;
        }

        // `attempt` is zero-based: attempt 0 means the primary just failed, so
        // the total number of attempts *so far* is `attempt + 1`.
        if attempt + 1 >= self.strategy.max_total_attempts {
            warn!(
                attempt,
                max = self.strategy.max_total_attempts,
                "fallback: max total attempts reached"
            );
            return None;
        }

        if !error.is_retryable() {
            return None;
        }

        // Walk the chain starting from `attempt` (each attempt consumes one
        // chain entry).
        let chain_index = attempt as usize;

        for entry in self.strategy.chain.iter().skip(chain_index) {
            let triggers_match = entry.triggers.iter().any(|t| error.matches(t));
            if triggers_match {
                info!(
                    from = primary_model,
                    to = %entry.model,
                    attempt,
                    "fallback: switching model"
                );
                return Some(FallbackDecision {
                    model: entry.model.clone(),
                    delay: Duration::from_millis(entry.delay_ms),
                });
            }
        }

        None
    }
}

/// The resolver's recommendation for the next attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackDecision {
    /// Model to try next.
    pub model: String,
    /// How long to wait before sending the request.
    pub delay: Duration,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ErrorClass, FallbackDecision, FallbackEntry, FallbackResolver, FallbackStrategy,
        FallbackTrigger,
    };

    fn strategy_with_chain(enabled: bool, chain: Vec<FallbackEntry>) -> FallbackStrategy {
        FallbackStrategy {
            enabled,
            chain,
            max_total_attempts: 3,
        }
    }

    fn entry(model: &str, triggers: Vec<FallbackTrigger>) -> FallbackEntry {
        FallbackEntry {
            model: model.to_owned(),
            triggers,
            max_retries: 1,
            delay_ms: 0,
        }
    }

    // -- disabled strategy --------------------------------------------------

    #[test]
    fn disabled_strategy_returns_none() {
        let strategy = strategy_with_chain(
            false,
            vec![entry("fallback-model", vec![FallbackTrigger::AnyError])],
        );
        let resolver = FallbackResolver::new(strategy);
        let error = ErrorClass::Timeout;
        assert!(resolver.next_model(0, &error, "primary").is_none());
    }

    // -- matching triggers --------------------------------------------------

    #[test]
    fn matching_trigger_returns_correct_fallback_model() {
        let strategy = strategy_with_chain(
            true,
            vec![
                entry("rate-limit-fallback", vec![FallbackTrigger::RateLimited]),
                entry("timeout-fallback", vec![FallbackTrigger::Timeout]),
            ],
        );
        let resolver = FallbackResolver::new(strategy);

        // A rate-limit error on the first attempt should match the first entry.
        let decision = resolver
            .next_model(
                0,
                &ErrorClass::RateLimited {
                    retry_after: Some(Duration::from_secs(5)),
                },
                "primary",
            )
            .expect("expected a fallback decision");

        assert_eq!(
            decision,
            FallbackDecision {
                model: "rate-limit-fallback".to_owned(),
                delay: Duration::ZERO,
            }
        );
    }

    #[test]
    fn timeout_trigger_matches_timeout_error() {
        let strategy = strategy_with_chain(
            true,
            vec![entry("timeout-fallback", vec![FallbackTrigger::Timeout])],
        );
        let resolver = FallbackResolver::new(strategy);

        let decision = resolver
            .next_model(0, &ErrorClass::Timeout, "primary")
            .expect("expected a fallback decision");

        assert_eq!(decision.model, "timeout-fallback");
    }

    #[test]
    fn unavailable_trigger_matches_unavailable_error() {
        let strategy = strategy_with_chain(
            true,
            vec![entry(
                "unavailable-fallback",
                vec![FallbackTrigger::Unavailable],
            )],
        );
        let resolver = FallbackResolver::new(strategy);

        let decision = resolver
            .next_model(0, &ErrorClass::Unavailable, "primary")
            .expect("expected a fallback decision");

        assert_eq!(decision.model, "unavailable-fallback");
    }

    #[test]
    fn any_error_trigger_matches_all_retryable_errors() {
        let strategy = strategy_with_chain(
            true,
            vec![entry("catch-all", vec![FallbackTrigger::AnyError])],
        );
        let resolver = FallbackResolver::new(strategy);

        for error in [
            ErrorClass::RateLimited { retry_after: None },
            ErrorClass::Timeout,
            ErrorClass::Unavailable,
            ErrorClass::Other("something went wrong".into()),
        ] {
            assert!(
                resolver.next_model(0, &error, "primary").is_some(),
                "AnyError should match {error:?}"
            );
        }
    }

    // -- max attempts -------------------------------------------------------

    #[test]
    fn max_attempts_is_respected() {
        let strategy = FallbackStrategy {
            enabled: true,
            chain: vec![
                entry("fb1", vec![FallbackTrigger::AnyError]),
                entry("fb2", vec![FallbackTrigger::AnyError]),
                entry("fb3", vec![FallbackTrigger::AnyError]),
            ],
            max_total_attempts: 2,
        };
        let resolver = FallbackResolver::new(strategy);
        let error = ErrorClass::Timeout;

        // attempt 0 (primary failed): total attempts so far = 1, limit = 2 => allowed.
        assert!(resolver.next_model(0, &error, "primary").is_some());

        // attempt 1 (first fallback failed): total attempts so far = 2, limit = 2 => blocked.
        assert!(resolver.next_model(1, &error, "primary").is_none());
    }

    // -- non-retryable errors -----------------------------------------------

    #[test]
    fn auth_error_does_not_trigger_fallback() {
        let strategy = strategy_with_chain(
            true,
            vec![entry("fallback", vec![FallbackTrigger::AnyError])],
        );
        let resolver = FallbackResolver::new(strategy);

        assert!(
            resolver
                .next_model(0, &ErrorClass::AuthError, "primary")
                .is_none()
        );
    }

    #[test]
    fn bad_request_does_not_trigger_fallback() {
        let strategy = strategy_with_chain(
            true,
            vec![entry("fallback", vec![FallbackTrigger::AnyError])],
        );
        let resolver = FallbackResolver::new(strategy);

        assert!(
            resolver
                .next_model(0, &ErrorClass::BadRequest, "primary")
                .is_none()
        );
    }

    // -- no matching trigger ------------------------------------------------

    #[test]
    fn no_matching_trigger_returns_none() {
        let strategy = strategy_with_chain(
            true,
            vec![entry("rate-limit-only", vec![FallbackTrigger::RateLimited])],
        );
        let resolver = FallbackResolver::new(strategy);

        // A timeout error should *not* match a rate-limit-only entry.
        assert!(
            resolver
                .next_model(0, &ErrorClass::Timeout, "primary")
                .is_none()
        );
    }

    // -- delay propagation --------------------------------------------------

    #[test]
    fn delay_is_propagated_in_decision() {
        let strategy = strategy_with_chain(
            true,
            vec![FallbackEntry {
                model: "delayed".to_owned(),
                triggers: vec![FallbackTrigger::AnyError],
                max_retries: 1,
                delay_ms: 500,
            }],
        );
        let resolver = FallbackResolver::new(strategy);

        let decision = resolver
            .next_model(0, &ErrorClass::Timeout, "primary")
            .expect("expected decision");

        assert_eq!(decision.delay, Duration::from_millis(500));
    }

    // -- empty chain --------------------------------------------------------

    #[test]
    fn empty_chain_returns_none() {
        let strategy = strategy_with_chain(true, vec![]);
        let resolver = FallbackResolver::new(strategy);

        assert!(
            resolver
                .next_model(0, &ErrorClass::Timeout, "primary")
                .is_none()
        );
    }

    // -- chain progression --------------------------------------------------

    #[test]
    fn chain_progresses_with_attempts() {
        let strategy = FallbackStrategy {
            enabled: true,
            chain: vec![
                entry("fb1", vec![FallbackTrigger::AnyError]),
                entry("fb2", vec![FallbackTrigger::AnyError]),
            ],
            max_total_attempts: 4,
        };
        let resolver = FallbackResolver::new(strategy);
        let error = ErrorClass::Unavailable;

        let d0 = resolver
            .next_model(0, &error, "primary")
            .expect("attempt 0");
        assert_eq!(d0.model, "fb1");

        let d1 = resolver
            .next_model(1, &error, "primary")
            .expect("attempt 1");
        assert_eq!(d1.model, "fb2");
    }

    // -- serde round-trip ---------------------------------------------------

    #[test]
    fn strategy_serde_round_trip() {
        let strategy = FallbackStrategy {
            enabled: true,
            chain: vec![FallbackEntry {
                model: "gpt-4o-mini".to_owned(),
                triggers: vec![FallbackTrigger::RateLimited, FallbackTrigger::Timeout],
                max_retries: 2,
                delay_ms: 1000,
            }],
            max_total_attempts: 5,
        };

        let json = serde_json::to_string(&strategy).expect("serialize");
        let deserialized: FallbackStrategy = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.enabled, strategy.enabled);
        assert_eq!(deserialized.max_total_attempts, strategy.max_total_attempts);
        assert_eq!(deserialized.chain.len(), 1);
        assert_eq!(deserialized.chain[0].model, "gpt-4o-mini");
        assert_eq!(deserialized.chain[0].max_retries, 2);
        assert_eq!(deserialized.chain[0].delay_ms, 1000);
        assert_eq!(
            deserialized.chain[0].triggers,
            vec![FallbackTrigger::RateLimited, FallbackTrigger::Timeout]
        );
    }

    // -- default strategy ---------------------------------------------------

    #[test]
    fn default_strategy_is_disabled_with_empty_chain() {
        let strategy = FallbackStrategy::default();
        assert!(!strategy.enabled);
        assert!(strategy.chain.is_empty());
        assert_eq!(strategy.max_total_attempts, 3);
    }
}
