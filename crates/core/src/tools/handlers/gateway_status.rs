use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Queries and manages the gateway server: status, config, health, config
/// patching/applying, and restart.
pub struct GatewayStatusHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct GatewayStatusArgs {
    /// One of "status", "config", "health", "config.get", "config.patch",
    /// "config.apply", "restart".
    action: String,
    /// Raw config content (YAML/JSON) for config.patch and config.apply.
    #[serde(default)]
    raw: Option<String>,
    /// Change description note for config operations.
    #[serde(default)]
    note: Option<String>,
    /// Restart delay in milliseconds.
    #[serde(default)]
    delay_ms: Option<u64>,
    /// Reason for restart.
    #[serde(default)]
    reason: Option<String>,
}

#[async_trait]
impl ToolHandler for GatewayStatusHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "gateway_status requires function payload".to_string(),
                ));
            }
        };

        let args: GatewayStatusArgs = parse_arguments(&arguments)?;
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());

        let timeout = if args.action == "restart" {
            RESTART_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        match args.action.as_str() {
            // Original read-only actions.
            "status" => send_get(&client, &gateway_url, "/api/status").await,
            "config" | "config.get" => send_get(&client, &gateway_url, "/api/config").await,
            "health" => send_get(&client, &gateway_url, "/health").await,

            // Config merge patch.
            "config.patch" => {
                let raw = args.raw.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "config.patch requires 'raw' field with config content".to_string(),
                    )
                })?;
                let mut body = serde_json::json!({ "config": raw });
                if let Some(note) = &args.note {
                    body["note"] = serde_json::Value::String(note.clone());
                }
                send_post(&client, &gateway_url, "/api/config/patch", body).await
            }

            // Config full replacement.
            "config.apply" => {
                let raw = args.raw.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "config.apply requires 'raw' field with config content".to_string(),
                    )
                })?;
                let mut body = serde_json::json!({ "config": raw });
                if let Some(note) = &args.note {
                    body["note"] = serde_json::Value::String(note.clone());
                }
                send_post(&client, &gateway_url, "/api/config/apply", body).await
            }

            // Restart.
            "restart" => {
                let mut body = serde_json::Map::new();
                if let Some(delay_ms) = args.delay_ms {
                    body.insert(
                        "delay_ms".to_string(),
                        serde_json::Value::Number(delay_ms.into()),
                    );
                }
                if let Some(reason) = &args.reason {
                    body.insert(
                        "reason".to_string(),
                        serde_json::Value::String(reason.clone()),
                    );
                }
                send_post(
                    &client,
                    &gateway_url,
                    "/api/restart",
                    serde_json::Value::Object(body),
                )
                .await
            }

            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown gateway action: {other}. Valid actions: status, config, health, \
                 config.get, config.patch, config.apply, restart"
            ))),
        }
    }
}

async fn send_get(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> Result<ToolOutput, FunctionCallError> {
    let url = format!("{base_url}{path}");
    let response = client.get(&url).send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to reach gateway at {url}: {err}"))
    })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read response: {err}"))
    })?;

    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "gateway returned HTTP {status}"
        )));
    }

    Ok(ToolOutput::Function {
        content: String::from_utf8_lossy(&body).into_owned(),
        content_items: None,
        success: Some(true),
    })
}

async fn send_post(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<ToolOutput, FunctionCallError> {
    let url = format!("{base_url}{path}");
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to reach gateway at {url}: {err}"))
        })?;

    let status = response.status();
    let body_bytes = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read response: {err}"))
    })?;

    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "gateway returned HTTP {status}: {}",
            String::from_utf8_lossy(&body_bytes)
        )));
    }

    Ok(ToolOutput::Function {
        content: String::from_utf8_lossy(&body_bytes).into_owned(),
        content_items: None,
        success: Some(true),
    })
}
