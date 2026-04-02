//! `savfox security` — Security audit and credential/token rotation.
#![allow(clippy::nonminimal_bool)]

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Security operations against a running gateway.
#[derive(Debug, Parser)]
pub struct SecurityCommand {
    #[clap(subcommand)]
    pub action: SecurityAction,

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
pub enum SecurityAction {
    /// Run gateway security audit checks.
    Audit {
        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },

    /// Rotate gateway token and/or webhook secrets.
    Rotate {
        /// Rotate gateway bearer token only (if no target flag is given, both are rotated).
        #[clap(long)]
        gateway_token: bool,

        /// Rotate enabled webhook secrets only (if no target flag is given, both are rotated).
        #[clap(long)]
        webhook_secrets: bool,

        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },
}

pub async fn run(cmd: SecurityCommand) -> Result<(), Box<dyn std::error::Error>> {
    let gateway_url = &cmd.gateway_url;
    let token = &cmd.token;

    match cmd.action {
        SecurityAction::Audit { json: raw_json } => {
            let result = ws_rpc_client::rpc_call(gateway_url, token, "security.audit", json!({}))
                .await
                .map_err(|e| format!("security.audit failed: {e}"))?;

            if raw_json {
                ws_rpc_client::print_json(&result);
            } else {
                print_audit_table(&result);
            }
        }
        SecurityAction::Rotate {
            gateway_token,
            webhook_secrets,
            json: raw_json,
        } => {
            // If no explicit target flag is passed, rotate both categories.
            let rotate_gateway = gateway_token || (!gateway_token && !webhook_secrets);
            let rotate_webhooks = webhook_secrets || (!gateway_token && !webhook_secrets);

            let params = json!({
                "gateway_token": rotate_gateway,
                "webhook_secrets": rotate_webhooks,
            });

            let result = ws_rpc_client::rpc_call(gateway_url, token, "security.rotate", params)
                .await
                .map_err(|e| format!("security.rotate failed: {e}"))?;

            if raw_json {
                ws_rpc_client::print_json(&result);
            } else {
                print_rotate_result(&result);
            }
        }
    }

    Ok(())
}

fn print_audit_table(result: &serde_json::Value) {
    let summary = result.get("summary").unwrap_or(&serde_json::Value::Null);
    let score = summary.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
    let critical = summary
        .get("critical")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let warn = summary.get("warn").and_then(|v| v.as_u64()).unwrap_or(0);
    let info = summary.get("info").and_then(|v| v.as_u64()).unwrap_or(0);

    println!("Security Audit");
    println!("Score: {score}/100  Critical: {critical}  Warn: {warn}  Pass: {info}");
    println!();
    println!("{:<28} {:<7} Details", "Check", "Status");
    println!("{}", "-".repeat(96));

    let checks = result
        .get("checks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for item in checks {
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let status = item
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let details = item.get("details").and_then(|v| v.as_str()).unwrap_or("");
        println!(
            "{:<28} {:<7} {}",
            name,
            status.to_ascii_uppercase(),
            details
        );
    }

    let suggestions = result
        .get("suggestions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !suggestions.is_empty() {
        println!();
        println!("Suggested fixes:");
        for suggestion in suggestions {
            if let Some(line) = suggestion.as_str() {
                println!("- {line}");
            }
        }
    }
}

fn print_rotate_result(result: &serde_json::Value) {
    let gateway_rotated = result
        .get("rotated")
        .and_then(|r| r.get("gateway_token"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let webhook_rotated = result
        .get("rotated")
        .and_then(|r| r.get("webhook_secrets"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    println!("Security rotation completed");
    println!(
        "gateway token: {}",
        if gateway_rotated {
            "rotated"
        } else {
            "unchanged"
        }
    );
    println!("webhook secrets rotated: {webhook_rotated}");

    if let Some(hint) = result.get("gateway_token_hint").and_then(|v| v.as_str()) {
        println!("new token hint: {hint}");
    }
    if let Some(path) = result.get("gateway_config_path").and_then(|v| v.as_str()) {
        println!("config path: {path}");
    }
    if result
        .get("restart_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        println!("restart required: yes");
    }

    let suggestions = result
        .get("suggestions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !suggestions.is_empty() {
        println!();
        println!("Next steps:");
        for suggestion in suggestions {
            if let Some(line) = suggestion.as_str() {
                println!("- {line}");
            }
        }
    }

    if let Some(failures) = result.get("failures").and_then(|v| v.as_array())
        && !failures.is_empty()
    {
        println!();
        println!("Rotation failures:");
        for failure in failures {
            if let Some(line) = failure.as_str() {
                println!("- {line}");
            }
        }
    }
}
