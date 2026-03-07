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

use savfox_core::cron::{
    CronDelivery, CronJob, CronJobState, CronPayload, CronRunEntry, CronSchedule,
    CronServiceStatus, CronSessionTarget,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn};

use crate::channel::GatewayChannel;

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

fn payload_timeout_secs(payload: &CronPayload) -> u64 {
    match payload {
        CronPayload::AgentTurn {
            timeout_secs: Some(timeout_secs),
            ..
        } if *timeout_secs > 0 => *timeout_secs,
        _ => 600,
    }
}

fn preview_reply(reply: String) -> String {
    if reply.len() > 200 {
        format!("{}...", &reply[..200])
    } else {
        reply
    }
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
    pub(crate) fn start(self: &Arc<Self>, channel: Arc<GatewayChannel>) -> mpsc::Sender<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        let service = Arc::clone(self);

        tokio::spawn(async move {
            info!("cron service started");
            loop {
                let sleep_ms = service.compute_sleep_ms().await;
                let sleep_dur = Duration::from_millis(sleep_ms.min(60_000)); // Cap at 60s

                tokio::select! {
                    _ = tokio::time::sleep(sleep_dur) => {
                        service.on_timer(&channel).await;
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
    async fn on_timer(&self, channel: &Arc<GatewayChannel>) {
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
            self.execute_job(&job, channel).await;
        }
    }

    /// Execute a single job.
    async fn execute_job(&self, job: &CronJob, channel: &Arc<GatewayChannel>) {
        let start_ms = now_epoch_ms();
        info!(job_id = %job.id, job_name = %job.name, "executing cron job");
        let timeout_secs = payload_timeout_secs(&job.payload);
        let session_id = self.session_id_for_job(job).await;

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.execute_payload(job, channel, session_id.as_deref()),
        )
        .await;

        let finish_ms = now_epoch_ms();

        let (status, error_msg, result_preview) = match result {
            Ok(Ok(preview)) => ("ok".to_string(), None, preview),
            Ok(Err(err)) => ("error".to_string(), Some(err.clone()), None),
            Err(_) => (
                "timeout".to_string(),
                Some(format!("job timed out ({timeout_secs}s)")),
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

            if let Some(channel_id) = &job.delivery.channel {
                if let Err(err) = channel
                    .send_platform_message(channel_id, &message, None, None, None)
                    .await
                {
                    warn!(job_id = %job.id, "failed to deliver cron result: {err}");
                }
            }
        }
    }

    /// Execute the payload for a job.
    async fn session_id_for_job(&self, job: &CronJob) -> Option<String> {
        if !matches!(job.session_target, CronSessionTarget::Main) {
            return None;
        }

        if let Some(session_id) = job.state.main_session_id.clone() {
            return Some(session_id);
        }

        let session_id = {
            let mut jobs = self.jobs.write().await;
            let stored = jobs.get_mut(&job.id)?;
            if let Some(session_id) = stored.state.main_session_id.clone() {
                session_id
            } else {
                let session_id = uuid::Uuid::now_v7().to_string();
                stored.state.main_session_id = Some(session_id.clone());
                session_id
            }
        };

        if let Err(err) = self.save_jobs().await {
            warn!(job_id = %job.id, "failed to persist cron main session id: {err}");
        }

        Some(session_id)
    }

    /// Execute the payload for a job.
    async fn execute_payload(
        &self,
        job: &CronJob,
        channel: &Arc<GatewayChannel>,
        session_id: Option<&str>,
    ) -> Result<Option<String>, String> {
        match &job.payload {
            CronPayload::SystemEvent { text } => {
                match channel
                    .invoke_agent_text_in_session(text, "default", session_id)
                    .await
                {
                    Ok(reply) => Ok(Some(preview_reply(reply))),
                    Err(err) => Err(format!("agent invocation failed: {err}")),
                }
            }
            CronPayload::AgentTurn {
                message,
                model,
                timeout_secs: _,
            } => {
                let model = model.as_deref().unwrap_or("default");
                match channel
                    .invoke_agent_text_in_session(message, model, session_id)
                    .await
                {
                    Ok(reply) => Ok(Some(preview_reply(reply))),
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
        agent_id: Option<String>,
    ) -> String {
        let now = now_epoch_ms();
        let id = uuid::Uuid::now_v7().to_string();
        let next_run = schedule.next_run_ms(now);

        let job = CronJob {
            id: id.clone(),
            name,
            agent_id,
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
        session_target: Option<CronSessionTarget>,
        agent_id: Option<Option<String>>,
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
            job.state.next_run_at_ms = if job.state.enabled {
                job.schedule.next_run_ms(now)
            } else {
                None
            };
        }
        if let Some(payload) = payload {
            job.payload = payload;
        }
        if let Some(delivery) = delivery {
            job.delivery = delivery;
        }
        if let Some(session_target) = session_target {
            job.session_target = session_target;
        }
        if let Some(agent_id) = agent_id {
            job.agent_id = agent_id;
        }
        if let Some(enabled) = enabled {
            job.state.enabled = enabled;
            job.state.next_run_at_ms = if enabled {
                job.schedule.next_run_ms(now)
            } else {
                None
            };
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
        channel: &Arc<GatewayChannel>,
    ) -> Result<(), String> {
        let job = {
            let jobs = self.jobs.read().await;
            jobs.get(id).cloned()
        };

        let job = job.ok_or_else(|| format!("job '{id}' not found"))?;
        self.execute_job(&job, channel).await;
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
