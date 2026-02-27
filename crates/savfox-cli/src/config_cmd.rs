//! `savfox config` — Manage gateway configuration from the CLI.

use clap::Parser;
use serde_json::json;

use crate::ws_rpc_client;

/// Manage gateway configuration via WS-RPC
#[derive(Debug, Parser)]
pub struct ConfigCommand {
    #[clap(subcommand)]
    pub action: ConfigAction,

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
pub enum ConfigAction {
    /// Get current configuration (optionally a specific path)
    Get {
        /// Dot-separated config path to retrieve (e.g. "gateway.port").
        /// If omitted, returns the full configuration.
        path: Option<String>,
    },

    /// Set a configuration value
    Set {
        /// Dot-separated config path (e.g. "gateway.port")
        path: String,

        /// Value to set (JSON literal or plain string)
        value: String,
    },

    /// Validate the current configuration
    Validate,

    /// Reload configuration from disk
    Reload,

    /// Export configuration content in a chosen format
    Export {
        /// Output format: json, yaml, or toml.
        #[clap(long, default_value = "toml")]
        format: String,

        /// Redact known secrets before export.
        #[clap(long)]
        redacted: bool,

        /// Output file path (stdout JSON result when omitted).
        #[clap(long)]
        output: Option<String>,
    },

    /// Convert gateway config between formats (toml/json/yaml)
    Convert {
        /// Source format (auto-detected when omitted).
        #[clap(long)]
        from: Option<String>,

        /// Target format.
        #[clap(long, default_value = "yaml")]
        to: String,

        /// Output file path (stdout JSON result when omitted).
        #[clap(long)]
        output: Option<String>,
    },
}

pub async fn run(cmd: ConfigCommand) -> Result<(), Box<dyn std::error::Error>> {
    let gateway_url = &cmd.gateway_url;
    let token = &cmd.token;

    match cmd.action {
        ConfigAction::Get { path } => {
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.get", json!({}))
                .await
                .map_err(|e| format!("config.get failed: {e}"))?;

            if let Some(key_path) = path {
                // Navigate into the result by dot-separated path.
                let extracted = extract_path(&result, &key_path);
                ws_rpc_client::print_json(&extracted);
            } else {
                ws_rpc_client::print_json(&result);
            }
        }

        ConfigAction::Set { path, value } => {
            // Build a nested config object from the dot path.
            let parsed_value = parse_value(&value);
            let config = build_nested_object(&path, parsed_value);

            let params = json!({ "config": config });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.set", params)
                .await
                .map_err(|e| format!("config.set failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        ConfigAction::Validate => {
            // First get current config, then validate it.
            let config = ws_rpc_client::rpc_call(gateway_url, token, "config.get", json!({}))
                .await
                .map_err(|e| format!("config.get failed: {e}"))?;

            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.validate", config)
                .await
                .map_err(|e| format!("config.validate failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        ConfigAction::Reload => {
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.reload", json!({}))
                .await
                .map_err(|e| format!("config.reload failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }

        ConfigAction::Export {
            format,
            redacted,
            output,
        } => {
            let params = json!({
                "format": format,
                "redacted": redacted,
            });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.export", params)
                .await
                .map_err(|e| format!("config.export failed: {e}"))?;

            if let Some(path) = output {
                let content = result
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or("config.export result missing 'content'")?;
                tokio::fs::write(&path, content)
                    .await
                    .map_err(|e| format!("failed to write export file: {e}"))?;
                eprintln!("Exported config to {path}");
            } else {
                ws_rpc_client::print_json(&result);
            }
        }

        ConfigAction::Convert { from, to, output } => {
            let from_format = match from {
                Some(explicit) => explicit,
                None => {
                    let detected =
                        ws_rpc_client::rpc_call(gateway_url, token, "config.format", json!({}))
                            .await
                            .map_err(|e| format!("config.format failed: {e}"))?;
                    detected
                        .get("format")
                        .and_then(|v| v.as_str())
                        .filter(|v| !v.is_empty() && *v != "unknown")
                        .unwrap_or("toml")
                        .to_string()
                }
            };

            let params = json!({
                "from_format": from_format,
                "to_format": to,
            });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.convert", params)
                .await
                .map_err(|e| format!("config.convert failed: {e}"))?;

            if let Some(path) = output {
                let content = result
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or("config.convert result missing 'content'")?;
                tokio::fs::write(&path, content)
                    .await
                    .map_err(|e| format!("failed to write converted file: {e}"))?;
                eprintln!("Converted config written to {path}");
            } else {
                ws_rpc_client::print_json(&result);
            }
        }
    }

    Ok(())
}

/// Parse a string value as JSON; if that fails, treat it as a plain string.
fn parse_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!(s))
}

/// Build a nested JSON object from a dot-separated path.
/// e.g. "gateway.port" with value 8080 => { "gateway": { "port": 8080 } }
fn build_nested_object(path: &str, value: serde_json::Value) -> serde_json::Value {
    let parts: Vec<&str> = path.split('.').collect();
    let mut result = value;
    for key in parts.into_iter().rev() {
        result = json!({ key: result });
    }
    result
}

/// Extract a nested value from JSON using a dot-separated path.
fn extract_path(value: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut current = value;
    for part in path.split('.') {
        match current.get(part) {
            Some(v) => current = v,
            None => return serde_json::Value::Null,
        }
    }
    current.clone()
}
