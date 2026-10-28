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
        #[clap(long, default_value = "markdown", value_parser = ["json", "markdown", "text"])]
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
            let params = json!({ "session_id": id });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "sessions.delete", params)
                .await
                .map_err(|e| format!("sessions.delete failed: {e}"))?;
            ws_rpc_client::print_json(&result);
        }
        SessionsAction::Export { id, format, output } => {
            // chat.history defaults to the last 50 messages. An export requests
            // the complete transcript rather than silently truncating it.
            let params = json!({ "session_id": id, "limit": u64::MAX });
            let result = ws_rpc_client::rpc_call(gateway_url, token, "chat.history", params)
                .await
                .map_err(|e| format!("chat.history failed: {e}"))?;
            let content = render_export(&id, &format, &result)?;

            if let Some(path) = output {
                tokio::fs::write(&path, content).await?;
                eprintln!("Exported session {id} to {path}");
            } else {
                print!("{content}");
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

fn render_export(id: &str, format: &str, history: &serde_json::Value) -> Result<String, String> {
    if let Some(error) = history.get("error") {
        return Err(format!("failed to export session: {error}"));
    }
    let messages = history
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or("chat.history result missing messages array")?;
    if format == "json" {
        return serde_json::to_string_pretty(history)
            .map(|text| format!("{text}\n"))
            .map_err(|error| error.to_string());
    }
    let markdown = match format {
        "markdown" => true,
        "text" => false,
        _ => return Err(format!("unsupported export format '{format}'")),
    };
    let mut output = if markdown {
        format!("# Session {id}\n\n")
    } else {
        format!("Session {id}\n\n")
    };
    for message in messages {
        let role = message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .ok_or("history message missing role")?;
        let text = message
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or("history message missing text")?;
        if markdown {
            output.push_str(&format!("## {role}\n\n{text}\n\n"));
        } else {
            output.push_str(&format!("{role}:\n{text}\n\n"));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    async fn mock_rpc(
        expected_method: &'static str,
        result: Value,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(request["method"], expected_method);
            assert_eq!(request["params"]["session_id"], "session-1");
            assert!(request["params"].get("id").is_none());
            if expected_method == "chat.history" {
                assert_eq!(request["params"]["limit"], json!(u64::MAX));
            }
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"jsonrpc":"2.0", "id":request["id"], "result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        (format!("http://{address}"), server)
    }

    #[tokio::test]
    async fn delete_uses_the_gateway_session_id_parameter() {
        let (url, server) = mock_rpc("sessions.delete", json!({"status":"deleted"})).await;
        run(
            SessionsCommand {
                action: SessionsAction::Delete {
                    id: "session-1".into(),
                },
            },
            &url,
            "test-token",
        )
        .await
        .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn export_writes_all_history_in_each_requested_format() {
        let directory = tempfile::tempdir().unwrap();
        let messages = (0..60).map(|index| json!({"role":if index % 2 == 0 {"user"} else {"assistant"}, "text":format!("message {index} 你好")})).collect::<Vec<_>>();
        for format in ["json", "markdown", "text"] {
            let (url, server) = mock_rpc(
                "chat.history",
                json!({"session_id":"session-1", "messages":messages}),
            )
            .await;
            let path = directory.path().join(format!("export.{format}"));
            run(
                SessionsCommand {
                    action: SessionsAction::Export {
                        id: "session-1".into(),
                        format: format.into(),
                        output: Some(path.to_string_lossy().into_owned()),
                    },
                },
                &url,
                "test-token",
            )
            .await
            .unwrap();
            server.await.unwrap();
            let content = tokio::fs::read_to_string(path).await.unwrap();
            assert!(content.contains("message 0 你好"));
            assert!(content.contains("message 59 你好"));
            if format == "json" {
                assert_eq!(
                    serde_json::from_str::<Value>(&content).unwrap()["messages"]
                        .as_array()
                        .unwrap()
                        .len(),
                    60
                );
            } else if format == "markdown" {
                assert!(content.starts_with("# Session session-1\n\n## user\n\n"));
            } else {
                assert!(content.starts_with("Session session-1\n\nuser:\n"));
            }
        }
    }

    #[tokio::test]
    async fn history_errors_do_not_overwrite_an_export_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.md");
        tokio::fs::write(&path, "existing export").await.unwrap();
        let (url, server) = mock_rpc(
            "chat.history",
            json!({"error":"outside execution scope", "messages":[]}),
        )
        .await;
        let result = run(
            SessionsCommand {
                action: SessionsAction::Export {
                    id: "session-1".into(),
                    format: "markdown".into(),
                    output: Some(path.to_string_lossy().into_owned()),
                },
            },
            &url,
            "test-token",
        )
        .await;
        server.await.unwrap();
        assert!(result.is_err());
        assert_eq!(
            tokio::fs::read_to_string(path).await.unwrap(),
            "existing export"
        );
    }
}
