use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CronListResponse {
    pub jobs: Vec<CronJob>,
}

#[derive(Debug, Deserialize)]
pub struct CronStatusResponse {
    pub running: Option<bool>,
    pub job_count: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronRunEntry {
    pub job_id: String,
    pub started_at: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub output: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CronRunsResponse {
    pub runs: Vec<CronRunEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronJobDetail {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<serde_json::Value>,
    pub payload: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub agent_id: Option<String>,
    pub session_target: Option<String>,
    pub wake_mode: Option<String>,
    pub delivery: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronRunDetail {
    pub job_id: String,
    pub started_at: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub result_preview: Option<String>,
}
