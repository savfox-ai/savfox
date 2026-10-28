//! `savfox cron` — Manage cron jobs on the gateway from the CLI.

use clap::Parser;
use savfox_core::cron::{CronPayload, CronSchedule};
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
            let schedule_value = parse_schedule(&schedule)?;
            let payload = parse_payload(&payload_type, message)?;

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

fn parse_schedule(schedule: &str) -> Result<CronSchedule, String> {
    let schedule = schedule.trim();
    if schedule == "every" {
        return Err("interval is missing after 'every'".into());
    }
    let Some(interval) = schedule.strip_prefix("every ") else {
        return Ok(CronSchedule::Cron {
            expression: schedule.to_owned(),
            timezone: None,
        });
    };
    let interval = interval.trim();
    let (amount, multiplier) = match interval.as_bytes().last() {
        Some(b's') => (&interval[..interval.len() - 1], 1),
        Some(b'm') => (&interval[..interval.len() - 1], 60),
        Some(b'h') => (&interval[..interval.len() - 1], 3600),
        Some(b'd') => (&interval[..interval.len() - 1], 86400),
        _ => return Err("interval must be a positive integer followed by s, m, h, or d".into()),
    };
    let seconds = amount
        .parse::<u64>()
        .ok()
        .and_then(|amount| amount.checked_mul(multiplier))
        .filter(|seconds| *seconds > 0 && seconds.checked_mul(1000).is_some())
        .ok_or("interval must be positive and fit in milliseconds")?;
    Ok(CronSchedule::Every {
        interval_secs: seconds,
        anchor_ms: 0,
    })
}

fn parse_payload(kind: &str, message: Option<String>) -> Result<CronPayload, String> {
    match kind {
        "agentTurn" | "agent_turn" => Ok(CronPayload::AgentTurn {
            message: message.unwrap_or_default(),
            model: None,
            timeout_secs: None,
        }),
        "systemEvent" | "system_event" => Ok(CronPayload::SystemEvent {
            text: message.unwrap_or_else(|| "cron.fire".into()),
        }),
        _ => Err(format!(
            "unsupported payload type '{kind}'; expected systemEvent or agentTurn"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    #[test]
    fn schedules_and_payloads_use_gateway_types() {
        assert_eq!(
            serde_json::to_value(parse_schedule("every 5m").unwrap()).unwrap(),
            json!({"kind":"every", "interval_secs":300, "anchor_ms":0})
        );
        assert_eq!(
            serde_json::to_value(parse_schedule("0 */5 * * * *").unwrap()).unwrap(),
            json!({"kind":"cron", "expression":"0 */5 * * * *", "timezone":null})
        );
        assert_eq!(
            serde_json::to_value(parse_payload("agentTurn", Some("hello".into())).unwrap())
                .unwrap()["type"],
            "agent_turn"
        );
        for invalid in [
            "every 0s",
            "every 5x",
            "every -1m",
            "every 18446744073709551615d",
            "every ",
        ] {
            assert!(parse_schedule(invalid).is_err(), "{invalid}");
        }
        assert!(parse_payload("typo", None).is_err());
    }

    #[tokio::test]
    async fn add_request_deserializes_as_the_gateway_contract() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(request["method"], "cron.add");
            assert_eq!(
                serde_json::from_value::<CronSchedule>(request["params"]["schedule"].clone())
                    .unwrap(),
                CronSchedule::Every {
                    interval_secs: 300,
                    anchor_ms: 0
                }
            );
            assert_eq!(
                serde_json::from_value::<CronPayload>(request["params"]["payload"].clone())
                    .unwrap(),
                CronPayload::SystemEvent {
                    text: "hello".into()
                }
            );
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"jsonrpc":"2.0", "id":request["id"], "result":{"id":"job"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        run(CronCommand {
            action: CronAction::Add {
                name: "job".into(),
                schedule: "every 5m".into(),
                payload_type: "systemEvent".into(),
                message: Some("hello".into()),
                enabled: true,
            },
            gateway_url: format!("http://{address}"),
            token: "test-token".into(),
        })
        .await
        .unwrap();
        server.await.unwrap();
    }
}
