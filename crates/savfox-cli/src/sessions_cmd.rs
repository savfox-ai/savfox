//! `savfox sessions` — Manage sessions from CLI.

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Manage gateway sessions
#[derive(Debug, Parser)]
pub struct SessionsCommand {
    #[clap(subcommand)]
    pub action: SessionsAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum SessionsAction {
    /// List active sessions
    List {
        /// Output format
        #[clap(long, default_value = "table")]
        format: String,
        /// Maximum number of sessions to show
        #[clap(long, default_value_t = 50)]
        limit: usize,
    },
    /// Delete a session
    Delete {
        /// Session ID to delete
        id: String,
    },
    /// Export a session
    Export {
        /// Session ID to export
        id: String,
        /// Output format (json, markdown, text)
        #[clap(long, default_value = "markdown")]
        format: String,
        /// Output file path (stdout if not specified)
        #[clap(long)]
        output: Option<String>,
    },
    /// Show cross-channel identity links.
    IdentityLinks,
    /// Link one canonical identity to one or more platform IDs.
    Link {
        /// Canonical identity id (for example: user@example.com).
        canonical: String,
        /// Platform IDs in `platform:id` format. Repeat for multiple IDs.
        #[clap(long = "id", required = true)]
        ids: Vec<String>,
    },
}

pub async fn run(
    cmd: SessionsCommand,
    gateway_url: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd.action {
        SessionsAction::List { format: _, limit } => {
            let params = json!({ "limit": limit });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "sessions.list", params)
                .await
                .map_err(|e| format!("sessions.list failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        SessionsAction::Delete { id } => {
            let params = json!({ "id": id });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "sessions.delete", params)
                .await
                .map_err(|e| format!("sessions.delete failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        SessionsAction::Export { id, format, output } => {
            let params = json!({ "id": id, "format": format });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "sessions.preview", params)
                .await
                .map_err(|e| format!("sessions.preview failed: {e}"))?;

            if let Some(path) = output {
                let content =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                tokio::fs::write(&path, content).await?;
                eprintln!("Exported session {id} to {path}");
            } else {
                ws_rpc_client::print_json(&result);
            }
        }
        SessionsAction::IdentityLinks => {
            let result = ws_rpc_client::rpc_call(
                gateway_url,
                token,
                "sessions.identity_links.get",
                json!({}),
            )
            .await
            .map_err(|e| format!("sessions.identity_links.get failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        SessionsAction::Link { canonical, ids } => {
            let params = json!({ "canonical": canonical, "ids": ids });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "identity.link", params)
                .await
                .map_err(|e| format!("identity.link failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
    }
    Ok(())
}
