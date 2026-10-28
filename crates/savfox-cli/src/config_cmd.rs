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

            let params = json!({ "patch": config });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "config.patch", params)
                .await
                .map_err(|e| format!("config.patch failed: {e}"))?;
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
            let from_format = if let Some(explicit) = from {
                explicit
            } else {
                let detected =
                    ws_rpc_client::rpc_call(gateway_url, token, "config.format", json!({}))
                        .await
                        .map_err(|e| format!("config.format failed: {e}"))?;
                detected
                    .get("format")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty() && *v != "unknown")
                    .unwrap_or("toml")
                    .to_owned()
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

#[cfg(test)]
mod set_tests {
    use futures_util::{SinkExt, StreamExt};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn setting_one_field_sends_a_patch_and_preserves_unrelated_config() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock config gateway");
        let address = listener
            .local_addr()
            .expect("read the mock gateway address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept the CLI connection");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("complete the mock WebSocket handshake");
            let frame = socket
                .next()
                .await
                .expect("receive a config request frame")
                .expect("read the config request frame");
            let request: Value = serde_json::from_str(
                frame
                    .to_text()
                    .expect("config request must be a text frame"),
            )
            .expect("decode the config JSON-RPC request");
            assert_eq!(request["method"], "config.patch");
            assert_eq!(
                request["params"],
                json!({"patch":{"gateway":{"port":18882}}})
            );
            // Model the gateway's merge-patch contract, including sibling preservation.
            let mut config =
                json!({"model":"provider/model", "gateway":{"host":"localhost", "port":18881}});
            config["gateway"]["port"] = request["params"]["patch"]["gateway"]["port"].clone();
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"jsonrpc":"2.0", "id":request["id"], "result":{"status":"patched"}})
                        .to_string()
                        .into(),
                ))
                .await
                .expect("send the config.patch response");
            config
        });
        super::run(super::ConfigCommand {
            action: super::ConfigAction::Set {
                path: "gateway.port".into(),
                value: "18882".into(),
            },
            gateway_url: format!("http://{address}"),
            token: "test-token".into(),
        })
        .await
        .expect("set the selected config field through the gateway");
        assert_eq!(
            server.await.expect("join the mock config gateway"),
            json!({"model":"provider/model", "gateway":{"host":"localhost", "port":18882}})
        );
    }
}
