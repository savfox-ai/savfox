//! Background cron/scheduler service.
//!
//! Provides timer-based job scheduling with persistent JSON storage,
//! multiple schedule types (cron expressions, intervals, one-shot timestamps),
//! job execution via agent turns or system events, exponential backoff on errors,
//! and run history logging.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn};

use crate::bridge::GatewayChannel;

// ─── Schedule Types ────────────────────────────────────────────────────────

/// How a cron job is scheduled.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn next_run_ms(&self, after_ms: u64) -> Option<u64> {
        match self {
            Self::At { at_ms } => {
                if *at_ms > after_ms {
                    Some(*at_ms)
                } else {
                    None // Already past.
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
                // Convert after_ms to DateTime.
                let after_dt = chrono::DateTime::from_timestamp_millis(after_ms as i64)?;
                schedule
                    .after(&after_dt)
                    .next()
                    .map(|dt| dt.timestamp_millis() as u64)
            }
        }
    }
}

// ─── Payload Types ─────────────────────────────────────────────────────────

/// What to execute when the job fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CronPayload {
    /// Inject a text message into the main agent session.
    SystemEvent {
        /// Text to inject.
        text: String,
    },
    /// Run an isolated agent turn.
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

// ─── Delivery Config ───────────────────────────────────────────────────────

/// Where to deliver job results.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    "none".to_string()
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CronSessionTarget {
    /// Use the agent's main session.
    #[default]
    Main,
    /// Create a temporary isolated session that is cleaned up after execution.
    Isolated,
}

// ─── Job State ─────────────────────────────────────────────────────────────

/// Runtime state for a cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for CronJobState {
    fn default() -> Self {
        Self {
            enabled: true,
            next_run_at_ms: None,
            last_run_at_ms: None,
            last_status: None,
            consecutive_errors: 0,
        }
    }
}

// ─── Cron Job ──────────────────────────────────────────────────────────────

/// A persistent cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    /// Unique job identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
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

// ─── Run Log Entry ─────────────────────────────────────────────────────────

/// A record of a single cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ─── Backoff Schedule ──────────────────────────────────────────────────────

/// Exponential backoff delays for consecutive errors.
const BACKOFF_SCHEDULE: &[Duration] = &[
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
    Duration::from_secs(3600),
];

fn backoff_delay(consecutive_errors: u32) -> Duration {
    let idx = (consecutive_errors as usize).min(BACKOFF_SCHEDULE.len() - 1);
    BACKOFF_SCHEDULE[idx]
}

// ─── Cron Service ──────────────────────────────────────────────────────────

/// Background cron service that manages and executes scheduled jobs.
#[derive(Debug)]
pub struct CronService {
    /// Directory for cron data files.
    data_dir: PathBuf,
    /// All jobs keyed by ID.
    jobs: Arc<RwLock<HashMap<String, CronJob>>>,
    /// Shutdown signal.
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl CronService {
    /// Create a new cron service rooted at the given directory.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: None,
        }
    }

    /// Create from SAVFOX_HOME.
    pub fn from_home(home: &Path) -> Self {
        Self::new(home.join("cron"))
    }

    fn jobs_path(&self) -> PathBuf {
        self.data_dir.join("jobs.json")
    }

    fn runs_dir(&self) -> PathBuf {
        self.data_dir.join("runs")
    }

    /// Ensure data directories exist.
    async fn ensure_dirs(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.data_dir).await?;
        tokio::fs::create_dir_all(self.runs_dir()).await?;
        Ok(())
    }

    /// Load jobs from disk.
    async fn load_jobs(&self) -> HashMap<String, CronJob> {
        let path = self.jobs_path();
        let data = match tokio::fs::read_to_string(&path).await {
            Ok(d) => d,
            Err(_) => return HashMap::new(),
        };
        match serde_json::from_str::<HashMap<String, CronJob>>(&data) {
            Ok(jobs) => jobs,
            Err(err) => {
                warn!("failed to parse cron jobs: {err}");
                HashMap::new()
            }
        }
    }

    /// Save jobs to disk.
    async fn save_jobs(&self) -> std::io::Result<()> {
        let _ = self.ensure_dirs().await;
        let jobs = self.jobs.read().await;
        let json = serde_json::to_string_pretty(&*jobs)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(self.jobs_path(), json).await
    }

    /// Initialize the service  - load jobs from disk and recompute next runs.
    pub async fn init(&self) {
        let loaded = self.load_jobs().await;
        let now = now_epoch_ms();

        let mut jobs = self.jobs.write().await;
        *jobs = loaded;

        // Recompute next_run for all enabled jobs.
        for job in jobs.values_mut() {
            if job.state.enabled {
                job.state.next_run_at_ms = job.schedule.next_run_ms(now);
            }
        }
    }

    /// Start the background timer loop. Returns a handle for shutdown.
    pub(crate) fn start(self: &Arc<Self>, bridge: Arc<GatewayChannel>) -> mpsc::Sender<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        let service = Arc::clone(self);

        tokio::spawn(async move {
            info!("cron service started");
            loop {
                let sleep_ms = service.compute_sleep_ms().await;
                let sleep_dur = Duration::from_millis(sleep_ms.min(60_000)); // Cap at 60s

                tokio::select! {
                    _ = tokio::time::sleep(sleep_dur) => {
                        service.on_timer(&bridge).await;
                    }
                    _ = shutdown_rx.recv() => {
                        info!("cron service shutting down");
                        break;
                    }
                }
            }
        });

        shutdown_tx
    }

    /// Compute how long to sleep before the next due job (in ms).
    async fn compute_sleep_ms(&self) -> u64 {
        let jobs = self.jobs.read().await;
        let now = now_epoch_ms();

        let mut min_ms = 60_000u64; // Default: check every 60s

        for job in jobs.values() {
            if !job.state.enabled {
                continue;
            }
            if let Some(next) = job.state.next_run_at_ms {
                let delta = next.saturating_sub(now);
                if delta < min_ms {
                    min_ms = delta;
                }
            }
        }

        // At least 1 second.
        min_ms.max(1000)
    }

    /// Timer callback  - find and execute due jobs.
    async fn on_timer(&self, bridge: &Arc<GatewayChannel>) {
        let now = now_epoch_ms();
        let due_jobs: Vec<CronJob>;

        {
            let jobs = self.jobs.read().await;
            due_jobs = jobs
                .values()
                .filter(|j| {
                    j.state.enabled && j.state.next_run_at_ms.map(|t| t <= now).unwrap_or(false)
                })
                .cloned()
                .collect();
        }

        for job in due_jobs {
            self.execute_job(&job, bridge).await;
        }
    }

    /// Execute a single job.
    async fn execute_job(&self, job: &CronJob, bridge: &Arc<GatewayChannel>) {
        let start_ms = now_epoch_ms();
        info!(job_id = %job.id, job_name = %job.name, "executing cron job");

        let result = tokio::time::timeout(
            Duration::from_secs(600), // 10 minute timeout
            self.execute_payload(&job.payload, bridge),
        )
        .await;

        let finish_ms = now_epoch_ms();

        let (status, error_msg, result_preview) = match result {
            Ok(Ok(preview)) => ("ok".to_string(), None, preview),
            Ok(Err(err)) => ("error".to_string(), Some(err.clone()), None),
            Err(_) => (
                "timeout".to_string(),
                Some("job timed out (10m)".to_string()),
                None,
            ),
        };

        // Log the run.
        let run_entry = CronRunEntry {
            job_id: job.id.clone(),
            job_name: job.name.clone(),
            started_at_ms: start_ms,
            finished_at_ms: finish_ms,
            status: status.clone(),
            error: error_msg.clone(),
            result_preview: result_preview.clone(),
        };
        self.log_run(&run_entry).await;

        // Update job state.
        let now = now_epoch_ms();
        {
            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.get_mut(&run_entry.job_id) {
                job.state.last_run_at_ms = Some(finish_ms);
                job.state.last_status = Some(status.clone());

                if status == "ok" {
                    job.state.consecutive_errors = 0;

                    // For one-shot "at" jobs, disable after execution.
                    if matches!(job.schedule, CronSchedule::At { .. }) {
                        job.state.enabled = false;
                        job.state.next_run_at_ms = None;
                    } else {
                        job.state.next_run_at_ms = job.schedule.next_run_ms(now);
                    }
                } else {
                    job.state.consecutive_errors += 1;

                    // Apply backoff.
                    let delay = backoff_delay(job.state.consecutive_errors);
                    job.state.next_run_at_ms = Some(now + delay.as_millis() as u64);

                    // Disable after 5 consecutive errors for one-shot jobs.
                    if matches!(job.schedule, CronSchedule::At { .. })
                        && job.state.consecutive_errors >= 5
                    {
                        job.state.enabled = false;
                        job.state.next_run_at_ms = None;
                        warn!(job_id = %job.id, "one-shot job disabled after 5 consecutive errors");
                    }
                }
            }
        }

        // Persist.
        if let Err(err) = self.save_jobs().await {
            warn!("failed to persist cron jobs after execution: {err}");
        }

        // Deliver results if configured.
        if job.delivery.mode == "announce" {
            let message = if status == "ok" {
                format!(
                    "Cron job `{}` completed successfully.{}",
                    job.name,
                    result_preview.map(|p| format!("\n{p}")).unwrap_or_default()
                )
            } else {
                format!(
                    "Cron job `{}` failed: {}",
                    job.name,
                    error_msg.unwrap_or_else(|| "unknown error".to_string())
                )
            };

            if let Some(channel) = &job.delivery.channel {
                if let Err(err) = bridge
                    .send_platform_message(channel, &message, None, None, None)
                    .await
                {
                    warn!(job_id = %job.id, "failed to deliver cron result: {err}");
                }
            }
        }
    }

    /// Execute the payload for a job.
    async fn execute_payload(
        &self,
        payload: &CronPayload,
        bridge: &Arc<GatewayChannel>,
    ) -> Result<Option<String>, String> {
        match payload {
            CronPayload::SystemEvent { text } => {
                // Inject text into the main agent session.
                match bridge.invoke_agent_text(text, "default").await {
                    Ok(reply) => {
                        let preview = if reply.len() > 200 {
                            format!("{}...", &reply[..200])
                        } else {
                            reply
                        };
                        Ok(Some(preview))
                    }
                    Err(err) => Err(format!("agent invocation failed: {err}")),
                }
            }
            CronPayload::AgentTurn {
                message,
                model,
                timeout_secs: _,
            } => {
                let agent = model.as_deref().unwrap_or("default");
                match bridge.invoke_agent_text(message, agent).await {
                    Ok(reply) => {
                        let preview = if reply.len() > 200 {
                            format!("{}...", &reply[..200])
                        } else {
                            reply
                        };
                        Ok(Some(preview))
                    }
                    Err(err) => Err(format!("agent turn failed: {err}")),
                }
            }
        }
    }

    /// Log a run entry to the job's run log file.
    async fn log_run(&self, entry: &CronRunEntry) {
        let _ = self.ensure_dirs().await;
        let path = self.runs_dir().join(format!("{}.jsonl", entry.job_id));
        let line = match serde_json::to_string(entry) {
            Ok(json) => format!("{json}\n"),
            Err(_) => return,
        };
        // Append to file.
        if let Err(err) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .and_then(|_| Ok(()))
        {
            warn!("failed to open run log: {err}");
            return;
        }
        // Use write approach to append.
        let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        let new_content = format!("{existing}{line}");
        if let Err(err) = tokio::fs::write(&path, new_content).await {
            warn!("failed to write run log: {err}");
        }
    }

    // ─── Public API ────────────────────────────────────────────────────────

    /// List all jobs.
    pub async fn list_jobs(&self) -> Vec<CronJob> {
        let jobs = self.jobs.read().await;
        jobs.values().cloned().collect()
    }

    /// Get a job by ID.
    pub async fn get_job(&self, id: &str) -> Option<CronJob> {
        let jobs = self.jobs.read().await;
        jobs.get(id).cloned()
    }

    /// Get service status.
    pub async fn status(&self) -> CronServiceStatus {
        let jobs = self.jobs.read().await;
        let total = jobs.len();
        let enabled = jobs.values().filter(|j| j.state.enabled).count();
        CronServiceStatus {
            enabled: true,
            total_jobs: total,
            enabled_jobs: enabled,
            running_jobs: 0, // TODO: track currently running
        }
    }

    /// Add a new job. Returns the job ID.
    pub async fn add_job(
        &self,
        name: String,
        schedule: CronSchedule,
        payload: CronPayload,
        delivery: CronDelivery,
        session_target: CronSessionTarget,
    ) -> String {
        let now = now_epoch_ms();
        let id = uuid::Uuid::now_v7().to_string();
        let next_run = schedule.next_run_ms(now);

        let job = CronJob {
            id: id.clone(),
            name,
            schedule,
            payload,
            delivery,
            session_target,
            state: CronJobState {
                enabled: true,
                next_run_at_ms: next_run,
                ..Default::default()
            },
            created_at_ms: now,
        };

        {
            let mut jobs = self.jobs.write().await;
            jobs.insert(id.clone(), job);
        }

        if let Err(err) = self.save_jobs().await {
            warn!("failed to persist cron job: {err}");
        }

        info!(job_id = %id, "cron job added");
        id
    }

    /// Update an existing job.
    pub async fn update_job(
        &self,
        id: &str,
        name: Option<String>,
        schedule: Option<CronSchedule>,
        payload: Option<CronPayload>,
        delivery: Option<CronDelivery>,
        enabled: Option<bool>,
    ) -> Result<CronJob, String> {
        let now = now_epoch_ms();
        let mut jobs = self.jobs.write().await;
        let job = jobs
            .get_mut(id)
            .ok_or_else(|| format!("job '{id}' not found"))?;

        if let Some(name) = name {
            job.name = name;
        }
        if let Some(schedule) = schedule {
            job.schedule = schedule;
            job.state.next_run_at_ms = job.schedule.next_run_ms(now);
        }
        if let Some(payload) = payload {
            job.payload = payload;
        }
        if let Some(delivery) = delivery {
            job.delivery = delivery;
        }
        if let Some(enabled) = enabled {
            job.state.enabled = enabled;
            if enabled {
                job.state.next_run_at_ms = job.schedule.next_run_ms(now);
            }
        }

        let updated = job.clone();
        drop(jobs);

        if let Err(err) = self.save_jobs().await {
            warn!("failed to persist cron job update: {err}");
        }

        Ok(updated)
    }

    /// Remove a job by ID.
    pub async fn remove_job(&self, id: &str) -> bool {
        let removed = {
            let mut jobs = self.jobs.write().await;
            jobs.remove(id).is_some()
        };

        if removed {
            if let Err(err) = self.save_jobs().await {
                warn!("failed to persist cron job removal: {err}");
            }
            info!(job_id = %id, "cron job removed");
        }

        removed
    }

    /// Manually trigger a job (run immediately).
    pub(crate) async fn run_job(
        &self,
        id: &str,
        bridge: &Arc<GatewayChannel>,
    ) -> Result<(), String> {
        let job = {
            let jobs = self.jobs.read().await;
            jobs.get(id).cloned()
        };

        let job = job.ok_or_else(|| format!("job '{id}' not found"))?;
        self.execute_job(&job, bridge).await;
        Ok(())
    }

    /// Get run history for a job.
    pub async fn get_runs(&self, job_id: &str, limit: usize) -> Vec<CronRunEntry> {
        let path = self.runs_dir().join(format!("{job_id}.jsonl"));
        let data = match tokio::fs::read_to_string(&path).await {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };

        let mut runs: Vec<CronRunEntry> = data
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        // Return most recent first, limited.
        runs.reverse();
        runs.truncate(limit);
        runs
    }
}

/// Status summary for the cron service.
#[derive(Debug, Clone, Serialize)]
pub struct CronServiceStatus {
    pub enabled: bool,
    pub total_jobs: usize,
    pub enabled_jobs: usize,
    pub running_jobs: usize,
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Create a `CronService` from the SAVFOX_HOME env or default.
pub fn create_cron_service() -> Arc<CronService> {
    let home = std::env::var("SAVFOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".savfox")
        });
    Arc::new(CronService::from_home(&home))
}
