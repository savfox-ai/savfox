use tracing::{info, warn};

/// Context pressure level indicating how full the context window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPressure {
    /// Below 50% — plenty of room.
    Low,
    /// 50-75% — normal operating range.
    Normal,
    /// 75-90% — approaching limit, consider compaction.
    High,
    /// 90-95% — compact soon or risk truncation.
    Critical,
    /// 95%+ — must compact immediately.
    Emergency,
}

impl ContextPressure {
    /// Whether compaction should be triggered at this pressure level.
    #[must_use]
    pub fn should_compact(self) -> bool {
        matches!(self, Self::Critical | Self::Emergency)
    }

    /// Whether a warning should be emitted.
    #[must_use]
    pub fn should_warn(self) -> bool {
        matches!(self, Self::High | Self::Critical | Self::Emergency)
    }
}

impl std::fmt::Display for ContextPressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Normal => write!(f, "normal"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
            Self::Emergency => write!(f, "emergency"),
        }
    }
}

/// Configuration for the context window guard.
#[derive(Debug, Clone)]
pub struct ContextGuardConfig {
    /// Maximum tokens in the context window.
    pub max_context_tokens: usize,
    /// Fraction at which to start warning (0.75 = 75%).
    pub warn_threshold: f64,
    /// Fraction at which to trigger auto-compact (0.90 = 90%).
    pub compact_threshold: f64,
    /// Fraction at which to force immediate compaction (0.95 = 95%).
    pub emergency_threshold: f64,
    /// Target token count after compaction (fraction of max).
    pub compact_target: f64,
}

impl Default for ContextGuardConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            warn_threshold: 0.75,
            compact_threshold: 0.90,
            emergency_threshold: 0.95,
            compact_target: 0.50,
        }
    }
}

/// Monitors context window utilization and triggers compaction when needed.
///
/// The guard is stateless — call `evaluate()` with the current token count
/// and it returns the appropriate action.
#[derive(Debug)]
pub struct ContextGuard {
    config: ContextGuardConfig,
}

impl ContextGuard {
    #[must_use]
    pub fn new(config: ContextGuardConfig) -> Self {
        Self { config }
    }

    /// Evaluate the current context usage and return pressure level.
    pub fn evaluate(&self, current_tokens: usize) -> ContextPressure {
        let ratio = current_tokens as f64 / self.config.max_context_tokens as f64;

        let pressure = if ratio >= self.config.emergency_threshold {
            ContextPressure::Emergency
        } else if ratio >= self.config.compact_threshold {
            ContextPressure::Critical
        } else if ratio >= self.config.warn_threshold {
            ContextPressure::High
        } else if ratio >= 0.50 {
            ContextPressure::Normal
        } else {
            ContextPressure::Low
        };

        if pressure.should_warn() {
            warn!(
                tokens = current_tokens,
                max = self.config.max_context_tokens,
                ratio = format!("{:.1}%", ratio * 100.0),
                pressure = %pressure,
                "context window pressure elevated"
            );
        }

        pressure
    }

    /// Calculate the target token count after compaction.
    #[must_use]
    pub fn compact_target_tokens(&self) -> usize {
        (self.config.max_context_tokens as f64 * self.config.compact_target) as usize
    }

    /// How many tokens should be freed by compaction.
    #[must_use]
    pub fn tokens_to_free(&self, current_tokens: usize) -> usize {
        let target = self.compact_target_tokens();
        current_tokens.saturating_sub(target)
    }

    /// Returns a recommendation for the caller.
    #[must_use]
    pub fn recommend(&self, current_tokens: usize) -> ContextRecommendation {
        let pressure = self.evaluate(current_tokens);
        match pressure {
            ContextPressure::Low | ContextPressure::Normal => ContextRecommendation::Continue,
            ContextPressure::High => ContextRecommendation::WarnUser {
                tokens_used: current_tokens,
                max_tokens: self.config.max_context_tokens,
            },
            ContextPressure::Critical => ContextRecommendation::AutoCompact {
                tokens_to_free: self.tokens_to_free(current_tokens),
            },
            ContextPressure::Emergency => ContextRecommendation::ForceCompact {
                tokens_to_free: self.tokens_to_free(current_tokens),
            },
        }
    }

    /// Update the max context tokens (e.g., when switching models).
    pub fn set_max_tokens(&mut self, max_tokens: usize) {
        info!(
            old = self.config.max_context_tokens,
            new = max_tokens,
            "context guard: max tokens updated"
        );
        self.config.max_context_tokens = max_tokens;
    }

    /// Current configuration.
    #[must_use]
    pub fn config(&self) -> &ContextGuardConfig {
        &self.config
    }
}

impl Default for ContextGuard {
    fn default() -> Self {
        Self::new(ContextGuardConfig::default())
    }
}

/// What the context guard recommends.
#[derive(Debug, Clone)]
pub enum ContextRecommendation {
    /// Normal operation, no action needed.
    Continue,
    /// Context is getting full, warn the user.
    WarnUser {
        tokens_used: usize,
        max_tokens: usize,
    },
    /// Should auto-compact in background.
    AutoCompact { tokens_to_free: usize },
    /// Must compact immediately before next turn.
    ForceCompact { tokens_to_free: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_levels() {
        let guard = ContextGuard::new(ContextGuardConfig {
            max_context_tokens: 100_000,
            ..Default::default()
        });

        assert_eq!(guard.evaluate(10_000), ContextPressure::Low);
        assert_eq!(guard.evaluate(60_000), ContextPressure::Normal);
        assert_eq!(guard.evaluate(80_000), ContextPressure::High);
        assert_eq!(guard.evaluate(92_000), ContextPressure::Critical);
        assert_eq!(guard.evaluate(96_000), ContextPressure::Emergency);
    }

    #[test]
    fn compact_target() {
        let guard = ContextGuard::new(ContextGuardConfig {
            max_context_tokens: 128_000,
            compact_target: 0.50,
            ..Default::default()
        });

        assert_eq!(guard.compact_target_tokens(), 64_000);
        assert_eq!(guard.tokens_to_free(120_000), 56_000);
    }

    #[test]
    fn recommendations() {
        let guard = ContextGuard::default();

        assert!(matches!(
            guard.recommend(50_000),
            ContextRecommendation::Continue
        ));
        assert!(matches!(
            guard.recommend(100_000),
            ContextRecommendation::WarnUser { .. }
        ));
        assert!(matches!(
            guard.recommend(118_000),
            ContextRecommendation::AutoCompact { .. }
        ));
        assert!(matches!(
            guard.recommend(125_000),
            ContextRecommendation::ForceCompact { .. }
        ));
    }
}
