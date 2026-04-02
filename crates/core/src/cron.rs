//! Shared cron scheduling and persistence types.

use serde::{Deserialize, Serialize};

/// How a cron job is scheduled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronSchedule {
    /// Execute at a specific absolute timestamp (one-shot).
    At {
        /// Epoch milliseconds.
        at_ms: u64,
    },
    /// Execute every N seconds, anchored to a start time.
    Every {
        /// Interval in seconds.
        interval_secs: u64,
        /// Anchor point (epoch ms). Defaults to job creation time.
        #[serde(default)]
        anchor_ms: u64,
    },
    /// Standard cron expression (5 or 6 fields).
    Cron {
        /// Cron expression string.
        expression: String,
        /// IANA timezone (e.g. "America/New_York"). Defaults to UTC.
        #[serde(default)]
        timezone: Option<String>,
    },
}

impl CronSchedule {
    /// Compute the next run time in epoch milliseconds, or None if expired.
    #[must_use] 
    pub fn next_run_ms(&self, after_ms: u64) -> Option<u64> {
        match self {
            Self::At { at_ms } => {
                if *at_ms > after_ms {
                    Some(*at_ms)
                } else {
                    None
                }
            }
            Self::Every {
                interval_secs,
                anchor_ms,
            } => {
                let interval_ms = interval_secs * 1000;
                if interval_ms == 0 {
                    return None;
                }
                let elapsed = after_ms.saturating_sub(*anchor_ms);
                let periods = elapsed / interval_ms + 1;
                Some(anchor_ms + periods * interval_ms)
            }
            Self::Cron { expression, .. } => {
                use std::str::FromStr;

                let schedule = cron::Schedule::from_str(expression).ok()?;
                let after_dt = chrono::DateTime::from_timestamp_millis(after_ms as i64)?;
                schedule
                    .after(&after_dt)
                    .next()
                    .map(|dt| dt.timestamp_millis() as u64)
            }
        }
    }
}

/// What to execute when the job fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CronPayload {
    /// Inject a text message into the target agent session.
    SystemEvent {
        /// Text to inject.
        text: String,
    },
    /// Run an agent turn.
    AgentTurn {
        /// Prompt for the agent.
        message: String,
        /// Model override (optional).
        #[serde(default)]
        model: Option<String>,
        /// Timeout in seconds (default: 600).
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
}

/// Where to deliver job results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronDelivery {
    /// Delivery mode: "none" or "announce".
    #[serde(default = "default_delivery_mode")]
    pub mode: String,
    /// Channel to announce to (e.g. "discord:123456").
    #[serde(default)]
    pub channel: Option<String>,
    /// Recipient for DM delivery.
    #[serde(default)]
    pub recipient: Option<String>,
}

fn default_delivery_mode() -> String {
    "none".to_owned()
}

impl Default for CronDelivery {
    fn default() -> Self {
        Self {
            mode: default_delivery_mode(),
            channel: None,
            recipient: None,
        }
    }
}

/// Session target for cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CronSessionTarget {
    /// Use the agent's main session.
    #[default]
    Main,
    /// Create a temporary isolated session that is cleaned up after execution.
    Isolated,
}

/// Runtime state for a cron job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronJobState {
    /// Whether the job is enabled.
    pub enabled: bool,
    /// Next scheduled run (epoch ms), or None.
    #[serde(default)]
    pub next_run_at_ms: Option<u64>,
    /// Last run timestamp (epoch ms).
    #[serde(default)]
    pub last_run_at_ms: Option<u64>,
    /// Last run status: "ok", "error", "timeout".
    #[serde(default)]
    pub last_status: Option<String>,
    /// Consecutive error count.
    #[serde(default)]
    pub consecutive_errors: u32,
    /// Stable session id used when a job targets its main session.
    #[serde(default)]
    pub main_session_id: Option<String>,
}

impl Default for CronJobState {
    fn default() -> Self {
        Self {
            enabled: true,
            next_run_at_ms: None,
            last_run_at_ms: None,
            last_status: None,
            consecutive_errors: 0,
            main_session_id: None,
        }
    }
}

/// A persistent cron job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    /// Unique job identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Target agent identifier. Defaults to the built-in `default` agent.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Schedule definition.
    pub schedule: CronSchedule,
    /// What to execute.
    pub payload: CronPayload,
    /// Where to deliver results.
    #[serde(default)]
    pub delivery: CronDelivery,
    /// Session target: "main" uses the agent's main session, "isolated" creates
    /// a temporary session that is cleaned up after cron execution completes.
    #[serde(default)]
    pub session_target: CronSessionTarget,
    /// Runtime state.
    #[serde(default)]
    pub state: CronJobState,
    /// Creation timestamp (epoch ms).
    pub created_at_ms: u64,
}

/// A record of a single cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronRunEntry {
    pub job_id: String,
    pub job_name: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub result_preview: Option<String>,
}

/// Status summary for the cron service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronServiceStatus {
    pub enabled: bool,
    pub total_jobs: usize,
    pub enabled_jobs: usize,
    pub running_jobs: usize,
}
