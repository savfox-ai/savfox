//! `savfox cron` — Manage cron jobs on the gateway from the CLI.

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Manage gateway cron jobs via WS-RPC
#[derive(Debug, Parser)]
pub struct CronCommand {
    #[clap(subcommand)]
    pub action: CronAction,

    /// Gateway URL
    #[clap(
        long,
        env = "SAVFOX_GATEWAY_URL",
        default_value = "http://127.0.0.1:18881"
    )]
    pub gateway_url: String,

    /// Authentication token
    #[clap(long, env = "SAVFOX_GATEWAY_TOKEN", default_value = "")]
    pub token: String,
}

#[derive(Debug, clap::Subcommand)]
pub enum CronAction {
    /// List all cron jobs
    List,

    /// Add a new cron job
    Add {
        /// Job name
        name: String,

        /// Cron expression (e.g. "*/5 * * * *") or interval (e.g. "every 5m")
        schedule: String,

        /// Payload type: "systemEvent" or "agentTurn"
        #[clap(long, default_value = "systemEvent")]
        payload_type: String,

        /// Payload message or event name
        #[clap(long)]
        message: Option<String>,

        /// Enable the job immediately
        #[clap(long)]
        enabled: bool,
    },

    /// Trigger a cron job to run immediately
    Run {
        /// Job ID to run
        id: String,
    },

    /// Remove a cron job
    Remove {
        /// Job ID to remove
        id: String,
    },
}

pub async fn run(cmd: CronCommand) -> Result<(), Box<dyn std::error::Error>> {
    let gateway_url = &cmd.gateway_url;
    let token = &cmd.token;

    match cmd.action {
        CronAction::List => {
            let result = ws_rpc_client::rpc_call(gateway_url, token, "cron.list", json!({}))
                .await
                .map_err(|e| format!("cron.list failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        CronAction::Add {
            name,
            schedule,
            payload_type,
            message,
            enabled,
        } => {
            // Determine schedule format: if it looks like a cron expression use
            // the "cron" schedule type, otherwise try to parse as an interval.
            let schedule_value = if schedule.starts_with("every ") {
                // e.g. "every 5m" -> { "type": "every", "interval": "5m" }
                let interval = schedule.trim_start_matches("every ").trim();
                json!({ "type": "every", "interval": interval })
            } else {
                // Assume cron expression.
                json!({ "type": "cron", "expression": schedule })
            };

            let payload = match payload_type.as_str() {
                "agentTurn" => json!({
                    "type": "agentTurn",
                    "message": message.unwrap_or_default(),
                }),
                _ => json!({
                    "type": "systemEvent",
                    "event": message.unwrap_or_else(|| "cron.fire".to_owned()),
                }),
            };

            let params = json!({
                "name": name,
                "schedule": schedule_value,
                "payload": payload,
                "enabled": enabled,
            });

            let result = ws_rpc_client::rpc_call(gateway_url, token, "cron.add", params)
                .await
                .map_err(|e| format!("cron.add failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        CronAction::Run { id } => {
            let params = json!({ "id": id });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "cron.run", params)
                .await
                .map_err(|e| format!("cron.run failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        CronAction::Remove { id } => {
            let params = json!({ "id": id });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "cron.remove", params)
                .await
                .map_err(|e| format!("cron.remove failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
    }

    Ok(())
}
