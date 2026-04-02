use std::time::Duration;

use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_CANVAS_URL: &str = "http://127.0.0.1:9300";

#[derive(Deserialize)]
struct CanvasArgs {
    /// Canvas action to perform.
    action: String,
    /// Target node ID.
    #[serde(default)]
    node_id: Option<String>,
    /// URL for navigate action.
    #[serde(default)]
    url: Option<String>,
    /// JavaScript code for eval action.
    #[serde(default)]
    code: Option<String>,
    /// Image format for snapshot: "png", "jpg", "jpeg".
    #[serde(default)]
    format: Option<String>,
    /// A2UI command payload (JSON object) for a2ui_push.
    #[serde(default)]
    command: Option<serde_json::Value>,
    /// A2UI component ID or path.
    #[serde(default)]
    component: Option<String>,
    /// A2UI props to push.
    #[serde(default)]
    props: Option<serde_json::Value>,
    /// Title for the canvas presentation.
    #[serde(default)]
    title: Option<String>,
    /// Width of the canvas.
    #[serde(default)]
    width: Option<u32>,
    /// Height of the canvas.
    #[serde(default)]
    height: Option<u32>,
}

/// Canvas tool for A2UI (Agent-to-UI) rendering on connected node displays.
///
/// Communicates with the node canvas service via HTTP REST endpoints.
/// The canvas service URL is configured via `SAVFOX_CANVAS_URL` (default: `http://127.0.0.1:9300`).
pub struct CanvasHandler;

#[async_trait::async_trait]
impl ToolHandler for CanvasHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("CanvasHandler received unsupported payload"),
        };
        let args: CanvasArgs = parse_arguments(&arguments)?;
        let canvas_url =
            std::env::var("SAVFOX_CANVAS_URL").unwrap_or_else(|_| DEFAULT_CANVAS_URL.to_owned());

        let timeout = if args.action == "snapshot" {
            SNAPSHOT_TIMEOUT
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
            "present" => {
                let mut body = serde_json::Map::new();
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                if let Some(url) = &args.url {
                    body.insert("url".to_owned(), serde_json::Value::String(url.clone()));
                }
                if let Some(title) = &args.title {
                    body.insert("title".to_owned(), serde_json::Value::String(title.clone()));
                }
                if let Some(width) = args.width {
                    body.insert("width".to_owned(), serde_json::Value::Number(width.into()));
                }
                if let Some(height) = args.height {
                    body.insert(
                        "height".to_owned(),
                        serde_json::Value::Number(height.into()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/present",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "hide" => {
                let mut body = serde_json::Map::new();
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/hide",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "navigate" => {
                let url = args.url.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel("navigate requires 'url' field".to_owned())
                })?;
                let mut body = serde_json::Map::new();
                body.insert("url".to_owned(), serde_json::Value::String(url.to_owned()));
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/navigate",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "eval" => {
                let code = args.code.as_deref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "eval requires 'code' field with JavaScript".to_owned(),
                    )
                })?;
                let mut body = serde_json::Map::new();
                body.insert(
                    "code".to_owned(),
                    serde_json::Value::String(code.to_owned()),
                );
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/eval",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "snapshot" => {
                let mut body = serde_json::Map::new();
                if let Some(format) = &args.format {
                    body.insert(
                        "format".to_owned(),
                        serde_json::Value::String(format.clone()),
                    );
                }
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/snapshot",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "a2ui_push" => {
                let mut body = serde_json::Map::new();
                if let Some(command) = &args.command {
                    body.insert("command".to_owned(), command.clone());
                }
                if let Some(component) = &args.component {
                    body.insert(
                        "component".to_owned(),
                        serde_json::Value::String(component.clone()),
                    );
                }
                if let Some(props) = &args.props {
                    body.insert("props".to_owned(), props.clone());
                }
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/a2ui/push",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "a2ui_reset" => {
                let mut body = serde_json::Map::new();
                if let Some(node_id) = &args.node_id {
                    body.insert(
                        "nodeId".to_owned(),
                        serde_json::Value::String(node_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &canvas_url,
                    "/a2ui/reset",
                    serde_json::Value::Object(body),
                )
                .await
            }
            other => model_err(format!(
                "unknown canvas action: {other}. Valid actions: present, hide, navigate, eval, \
                 snapshot, a2ui_push, a2ui_reset"
            )),
        }
    }
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
            FunctionCallError::RespondToModel(format!("canvas request failed: {err}"))
        })?;

    let status = response.status();
    let body_bytes = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read canvas response: {err}"))
    })?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return model_err(format!(
            "canvas service returned HTTP {status}: {body_text}"
        ));
    }

    Ok(ToolOutput::ok(body_text))
}
