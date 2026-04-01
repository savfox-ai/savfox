use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// In-memory cron entry.
#[derive(Debug, Clone, serde::Serialize)]
struct CronEntry {
    name: String,
    schedule: String,
    command: String,
}

/// Shared state for cron entries (persists for the session lifetime).
#[derive(Debug, Default)]
pub struct CronState {
    entries: RwLock<Vec<CronEntry>>,
}

pub struct CronHandler {
    state: Arc<CronState>,
}

impl CronHandler {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CronState::default()),
        }
    }
}

#[derive(Deserialize)]
struct CronArgs {
    /// Action to perform: "list", "add", "remove", "next".
    action: String,
    /// Cron schedule expression (required for "add").
    #[serde(default)]
    schedule: Option<String>,
    /// Name / identifier for the cron job.
    #[serde(default)]
    name: Option<String>,
    /// Shell command to run (required for "add").
    #[serde(default)]
    command: Option<String>,
}

#[async_trait]
impl ToolHandler for CronHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return model_err("cron handler received unsupported payload");
            }
        };

        let args: CronArgs = parse_arguments(&arguments)?;

        match args.action.as_str() {
            "list" => self.handle_list().await,
            "add" => self.handle_add(&args).await,
            "remove" => self.handle_remove(&args).await,
            "next" => self.handle_next(&args).await,
            _ => model_err(format!(
                "unknown cron action: `{}`. Expected one of: list, add, remove, next",
                args.action
            )),
        }
    }
}

impl CronHandler {
    async fn handle_list(&self) -> Result<ToolOutput, FunctionCallError> {
        let entries = self.state.entries.read().await;
        let json = serde_json::to_string_pretty(&*entries).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolOutput::ok(if entries.is_empty() {
            "No cron jobs configured.".to_string()
        } else {
            json
        }))
    }

    async fn handle_add(&self, args: &CronArgs) -> Result<ToolOutput, FunctionCallError> {
        let name = args.name.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            return model_err("cron add requires a `name`");
        }

        let schedule_str = args.schedule.as_deref().unwrap_or("").trim();
        if schedule_str.is_empty() {
            return model_err("cron add requires a `schedule` expression");
        }

        let command = args.command.as_deref().unwrap_or("").trim();
        if command.is_empty() {
            return model_err("cron add requires a `command`");
        }

        // Validate the cron expression.
        cron::Schedule::from_str(schedule_str)
            .or_else(|err| model_err(format!("invalid cron schedule `{schedule_str}`: {err}")))?;

        let entry = CronEntry {
            name: name.to_string(),
            schedule: schedule_str.to_string(),
            command: command.to_string(),
        };

        let mut entries = self.state.entries.write().await;
        // Replace if name already exists.
        entries.retain(|e| e.name != name);
        entries.push(entry);

        Ok(ToolOutput::ok(format!(
            "Cron job `{name}` added with schedule `{schedule_str}`."
        )))
    }

    async fn handle_remove(&self, args: &CronArgs) -> Result<ToolOutput, FunctionCallError> {
        let name = args.name.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            return model_err("cron remove requires a `name`");
        }

        let mut entries = self.state.entries.write().await;
        let before = entries.len();
        entries.retain(|e| e.name != name);

        if entries.len() == before {
            Ok(ToolOutput::fail(format!(
                "No cron job named `{name}` found."
            )))
        } else {
            Ok(ToolOutput::ok(format!("Cron job `{name}` removed.")))
        }
    }

    async fn handle_next(&self, args: &CronArgs) -> Result<ToolOutput, FunctionCallError> {
        let name = args.name.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            return model_err("cron next requires a `name`");
        }

        let entries = self.state.entries.read().await;
        let entry = entries.iter().find(|e| e.name == name);
        let Some(entry) = entry else {
            return model_err(format!("no cron job named `{name}` found"));
        };

        let schedule = cron::Schedule::from_str(&entry.schedule).or_else(|err| {
            model_err(format!(
                "invalid stored schedule `{}`: {err}",
                entry.schedule
            ))
        })?;

        let upcoming: Vec<String> = schedule
            .upcoming(chrono::Utc)
            .take(5)
            .map(|dt| dt.to_rfc3339())
            .collect();

        Ok(ToolOutput::ok(
            serde_json::json!({
                "name": entry.name,
                "schedule": entry.schedule,
                "next_runs": upcoming,
            })
            .to_string(),
        ))
    }
}
