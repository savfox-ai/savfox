use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronSchedule {
    At {
        at_ms: u64,
    },
    Every {
        interval_secs: u64,
        #[serde(default)]
        anchor_ms: u64,
    },
    Cron {
        expression: String,
        #[serde(default)]
        timezone: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CronPayload {
    SystemEvent {
        text: String,
    },
    AgentTurn {
        message: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
}

impl CronPayload {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SystemEvent { .. } => "system_event",
            Self::AgentTurn { .. } => "agent_turn",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CronDelivery {
    #[serde(default = "default_delivery_mode")]
    pub mode: String,
    #[serde(default)]
    pub channel: Option<String>,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CronSessionTarget {
    #[default]
    Main,
    Isolated,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CronJob {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub payload: Option<CronPayload>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub agent_id: Option<String>,
    pub session_target: Option<CronSessionTarget>,
    pub delivery: Option<CronDelivery>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CronListResponse {
    pub jobs: Vec<CronJob>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CronStatusResponse {
    pub running: Option<bool>,
    pub job_count: Option<usize>,
    pub enabled_jobs: Option<usize>,
    pub running_jobs: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CronRunEntry {
    pub job_id: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    #[serde(rename = "result_preview", alias = "output")]
    pub output: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CronRunsResponse {
    pub runs: Vec<CronRunEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronJobDetail {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<CronSchedule>,
    pub payload: Option<CronPayload>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub agent_id: Option<String>,
    pub session_target: Option<CronSessionTarget>,
    pub wake_mode: Option<String>,
    pub delivery: Option<CronDelivery>,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CronPayload, CronRunEntry, CronSchedule};

    #[test]
    fn cron_schedule_and_payload_use_gateway_wire_shape() {
        let schedule = CronSchedule::Every {
            interval_secs: 3_600,
            anchor_ms: 0,
        };
        let payload = CronPayload::AgentTurn {
            message: "Summarize activity".to_owned(),
            model: None,
            timeout_secs: Some(300),
        };

        assert_eq!(
            serde_json::to_value(schedule).expect("schedule should serialize"),
            json!({ "kind": "every", "interval_secs": 3600, "anchor_ms": 0 })
        );
        assert_eq!(
            serde_json::to_value(payload).expect("payload should serialize"),
            json!({
                "type": "agent_turn",
                "message": "Summarize activity",
                "model": null,
                "timeout_secs": 300
            })
        );
    }

    #[test]
    fn cron_run_accepts_legacy_output_field() {
        let run: CronRunEntry = serde_json::from_value(json!({
            "job_id": "daily-summary",
            "output": "done"
        }))
        .expect("legacy cron run should deserialize");

        assert_eq!(run.output.as_deref(), Some("done"));
        let serialized = serde_json::to_value(run).expect("cron run should serialize");
        assert_eq!(serialized["result_preview"], "done");
        assert!(serialized.get("output").is_none());
    }
}
