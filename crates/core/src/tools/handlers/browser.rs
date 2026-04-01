use std::time::Duration;

use serde::Deserialize;

use super::{parse_arguments, require_field};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_BROWSER_URL: &str = "http://127.0.0.1:9222";

#[derive(Deserialize)]
struct BrowserArgs {
    /// The browser action to perform.
    action: String,
    /// Browser profile: "default", "chrome", or custom.
    #[serde(default)]
    profile: Option<String>,
    /// Tab/page target identifier.
    #[serde(default)]
    target_id: Option<String>,
    /// URL for navigate/open actions.
    #[serde(default)]
    target_url: Option<String>,
    /// Snapshot format: "ai" or "aria".
    #[serde(default)]
    format: Option<String>,
    /// Ref strategy: "role" or "aria".
    #[serde(default)]
    refs: Option<String>,
    /// Maximum characters for snapshot.
    #[serde(default)]
    max_chars: Option<u32>,
    /// CSS selector for targeted operations.
    #[serde(default)]
    selector: Option<String>,
    /// Whether to capture full page screenshot.
    #[serde(default)]
    full_page: Option<bool>,
    /// Image type for screenshot: "png" or "jpeg".
    #[serde(default)]
    image_type: Option<String>,
    /// Browser act request (click, type, press, etc.).
    #[serde(default)]
    request: Option<serde_json::Value>,
    /// File paths for upload action.
    #[serde(default)]
    paths: Option<Vec<String>>,
    /// Whether to accept a dialog.
    #[serde(default)]
    accept: Option<bool>,
    /// Text to enter in a prompt dialog.
    #[serde(default)]
    prompt_text: Option<String>,
    /// Console log level filter: "error", "warning", "log", "info".
    #[serde(default)]
    level: Option<String>,
}

/// Browser automation tool that communicates with an external browser control service
/// via HTTP REST endpoints.
pub struct BrowserHandler;

#[async_trait::async_trait]
impl ToolHandler for BrowserHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("BrowserHandler received unsupported payload"),
        };
        let args: BrowserArgs = parse_arguments(&arguments)?;
        let browser_url =
            std::env::var("SAVFOX_BROWSER_URL").unwrap_or_else(|_| DEFAULT_BROWSER_URL.to_string());

        let timeout = if args.action == "screenshot" || args.action == "pdf" {
            SCREENSHOT_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .or_else(|err| model_err(format!("failed to build HTTP client: {err}")))?;

        match args.action.as_str() {
            "status" => send_get(&client, &browser_url, "/").await,
            "start" => {
                let mut body = serde_json::Map::new();
                if let Some(profile) = &args.profile {
                    body.insert(
                        "profile".to_string(),
                        serde_json::Value::String(profile.clone()),
                    );
                }
                send_post(
                    &client,
                    &browser_url,
                    "/start",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "stop" => {
                send_post(
                    &client,
                    &browser_url,
                    "/stop",
                    serde_json::Value::Object(serde_json::Map::new()),
                )
                .await
            }
            "profiles" => send_get(&client, &browser_url, "/profiles").await,
            "tabs" => send_get(&client, &browser_url, "/tabs").await,
            "open" => {
                let mut body = serde_json::Map::new();
                if let Some(url) = &args.target_url {
                    body.insert("url".to_string(), serde_json::Value::String(url.clone()));
                }
                send_post(
                    &client,
                    &browser_url,
                    "/tabs/open",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "focus" => {
                let target_id = require_field(&args.target_id, "target_id")?;
                let body = serde_json::json!({ "targetId": target_id });
                send_post(&client, &browser_url, "/tabs/focus", body).await
            }
            "close" => {
                let target_id = require_field(&args.target_id, "target_id")?;
                let url = format!("{browser_url}/tabs/{target_id}");
                let response = client
                    .delete(&url)
                    .send()
                    .await
                    .or_else(|err| model_err(format!("browser request failed: {err}")))?;
                response_to_output(response).await
            }
            "snapshot" => {
                let mut query_params = Vec::new();
                if let Some(fmt) = &args.format {
                    query_params.push(format!("format={fmt}"));
                }
                if let Some(refs) = &args.refs {
                    query_params.push(format!("refs={refs}"));
                }
                if let Some(max_chars) = args.max_chars {
                    query_params.push(format!("maxChars={max_chars}"));
                }
                if let Some(selector) = &args.selector {
                    query_params.push(format!("selector={}", urlencoding_simple(selector)));
                }
                if let Some(target_id) = &args.target_id {
                    query_params.push(format!("targetId={target_id}"));
                }
                let qs = if query_params.is_empty() {
                    String::new()
                } else {
                    format!("?{}", query_params.join("&"))
                };
                send_get(&client, &browser_url, &format!("/snapshot{qs}")).await
            }
            "screenshot" => {
                let mut body = serde_json::Map::new();
                if let Some(full_page) = args.full_page {
                    body.insert("fullPage".to_string(), serde_json::Value::Bool(full_page));
                }
                if let Some(image_type) = &args.image_type {
                    body.insert(
                        "imageType".to_string(),
                        serde_json::Value::String(image_type.clone()),
                    );
                }
                if let Some(selector) = &args.selector {
                    body.insert(
                        "selector".to_string(),
                        serde_json::Value::String(selector.clone()),
                    );
                }
                if let Some(target_id) = &args.target_id {
                    body.insert(
                        "targetId".to_string(),
                        serde_json::Value::String(target_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &browser_url,
                    "/screenshot",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "navigate" => {
                let url = require_field(&args.target_url, "target_url")?;
                let mut body = serde_json::Map::new();
                body.insert(
                    "url".to_string(),
                    serde_json::Value::String(url.to_string()),
                );
                if let Some(target_id) = &args.target_id {
                    body.insert(
                        "targetId".to_string(),
                        serde_json::Value::String(target_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &browser_url,
                    "/navigate",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "console" => {
                let mut query_params = Vec::new();
                if let Some(level) = &args.level {
                    query_params.push(format!("level={level}"));
                }
                if let Some(target_id) = &args.target_id {
                    query_params.push(format!("targetId={target_id}"));
                }
                let qs = if query_params.is_empty() {
                    String::new()
                } else {
                    format!("?{}", query_params.join("&"))
                };
                send_get(&client, &browser_url, &format!("/console{qs}")).await
            }
            "pdf" => {
                let mut body = serde_json::Map::new();
                if let Some(target_id) = &args.target_id {
                    body.insert(
                        "targetId".to_string(),
                        serde_json::Value::String(target_id.clone()),
                    );
                }
                send_post(
                    &client,
                    &browser_url,
                    "/pdf",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "upload" => {
                let Some(paths) = args.paths.as_ref() else {
                    return model_err("missing required field: paths");
                };
                let body = serde_json::json!({ "paths": paths });
                send_post(&client, &browser_url, "/hooks/file-chooser", body).await
            }
            "dialog" => {
                let mut body = serde_json::Map::new();
                if let Some(accept) = args.accept {
                    body.insert("accept".to_string(), serde_json::Value::Bool(accept));
                }
                if let Some(text) = &args.prompt_text {
                    body.insert(
                        "promptText".to_string(),
                        serde_json::Value::String(text.clone()),
                    );
                }
                send_post(
                    &client,
                    &browser_url,
                    "/hooks/dialog",
                    serde_json::Value::Object(body),
                )
                .await
            }
            "act" => {
                let Some(request) = args.request else {
                    return model_err("missing required field: request (BrowserActRequest JSON)");
                };
                send_post(&client, &browser_url, "/act", request).await
            }
            other => model_err(format!(
                "unknown browser action: {other}. Valid actions: status, start, stop, profiles, \
                 tabs, open, focus, close, snapshot, screenshot, navigate, console, pdf, upload, \
                 dialog, act"
            )),
        }
    }
}

/// Simple URL encoding for query parameter values.
fn urlencoding_simple(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}

async fn send_get(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> Result<ToolOutput, FunctionCallError> {
    let url = format!("{base_url}{path}");
    let response = client
        .get(&url)
        .send()
        .await
        .or_else(|err| model_err(format!("browser request failed: {err}")))?;
    response_to_output(response).await
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
        .or_else(|err| model_err(format!("browser request failed: {err}")))?;
    response_to_output(response).await
}

async fn response_to_output(response: reqwest::Response) -> Result<ToolOutput, FunctionCallError> {
    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .or_else(|err| model_err(format!("failed to read browser response: {err}")))?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return model_err(format!(
            "browser service returned HTTP {status}: {body_text}"
        ));
    }

    Ok(ToolOutput::ok(body_text))
}
