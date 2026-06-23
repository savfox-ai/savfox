use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::debug;

use crate::cdp::CdpClient;
use crate::element::Element;
use crate::screenshot::ScreenshotOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkResponseEvent {
    pub request_id: String,
    pub url: String,
    pub status: u16,
    pub mime_type: Option<String>,
    pub headers: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadEvent {
    pub guid: String,
    pub url: Option<String>,
    pub suggested_filename: Option<String>,
    pub file_path: String,
    pub state: String,
    pub total_bytes: Option<u64>,
    pub received_bytes: Option<u64>,
}

#[derive(Debug, Default, Clone)]
struct PendingDownload {
    guid: Option<String>,
    url: Option<String>,
    suggested_filename: Option<String>,
}

pub struct Page {
    target_id: String,
    session_id: String,
    cdp: Arc<CdpClient>,
}

impl Page {
    pub fn new(target_id: String, session_id: String, cdp: Arc<CdpClient>) -> Self {
        Self {
            target_id,
            session_id,
            cdp,
        }
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        debug!(url = %url, "Navigating to URL");

        let result = self
            .cdp
            .send_request_to_session(
                "Page.navigate",
                serde_json::json!({ "url": url }),
                &self.session_id,
            )
            .await?;

        // Page.navigate returns an `errorText` field when the navigation could
        // not be started (bad scheme, DNS failure, blocked by client, …). Treat
        // that as an error rather than reporting a success the caller can't trust.
        if let Some(error_text) = result.get("errorText").and_then(|v| v.as_str())
            && !error_text.is_empty()
        {
            return Err(anyhow!("navigation to {url} failed: {error_text}"));
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Ok(())
    }

    pub async fn wait_for_selector(&self, selector: &str, timeout_ms: u64) -> Result<Element> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow!("Timeout waiting for selector: {selector}"));
            }

            match self.query_selector(selector).await {
                Ok(element) => return Ok(element),
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    pub async fn query_selector(&self, selector: &str) -> Result<Element> {
        let document = self
            .cdp
            .send_request_to_session("DOM.getDocument", serde_json::json!({}), &self.session_id)
            .await?;

        let root_node_id = document
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("Failed to get root node ID"))?;

        let result = self
            .cdp
            .send_request_to_session(
                "DOM.querySelector",
                serde_json::json!({
                    "nodeId": root_node_id,
                    "selector": selector,
                }),
                &self.session_id,
            )
            .await?;

        let node_id = result
            .get("nodeId")
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("Element not found: {selector}"))?;

        Ok(Element::new(
            node_id,
            selector.to_owned(),
            self.session_id.clone(),
            self.cdp.clone(),
        ))
    }

    pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<Element>> {
        let document = self
            .cdp
            .send_request_to_session("DOM.getDocument", serde_json::json!({}), &self.session_id)
            .await?;

        let root_node_id = document
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("Failed to get root node ID"))?;

        let result = self
            .cdp
            .send_request_to_session(
                "DOM.querySelectorAll",
                serde_json::json!({
                    "nodeId": root_node_id,
                    "selector": selector,
                }),
                &self.session_id,
            )
            .await?;

        let node_ids = result
            .get("nodeIds")
            .and_then(|n| n.as_array())
            .ok_or_else(|| anyhow!("Failed to get node IDs"))?;

        let elements: Vec<Element> = node_ids
            .iter()
            .filter_map(|n| n.as_u64())
            .map(|node_id| {
                Element::new(
                    node_id,
                    selector.to_owned(),
                    self.session_id.clone(),
                    self.cdp.clone(),
                )
            })
            .collect();

        Ok(elements)
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let result = self
            .cdp
            .send_request_to_session(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
                &self.session_id,
            )
            .await?;

        if let Some(exception) = result.get("exceptionDetails") {
            return Err(anyhow!("JavaScript error: {exception:?}"));
        }

        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn capture_responses(
        &self,
        duration_ms: u64,
        max_events: usize,
    ) -> Result<Vec<NetworkResponseEvent>> {
        self.cdp
            .send_request_to_session("Network.enable", serde_json::json!({}), &self.session_id)
            .await?;

        let cap = max_events.max(1);
        let events = Arc::new(StdMutex::new(Vec::<NetworkResponseEvent>::new()));
        let events_ref = Arc::clone(&events);
        self.cdp
            .on_event("Network.responseReceived", move |params| {
                if let Some(event) = parse_network_response_event(&params)
                    && let Ok(mut lock) = events_ref.lock()
                    && lock.len() < cap
                {
                    lock.push(event);
                }
            })
            .await;

        tokio::time::sleep(Duration::from_millis(duration_ms.max(1))).await;
        let _ = self
            .cdp
            .send_request_to_session("Network.disable", serde_json::json!({}), &self.session_id)
            .await;

        let out = events.lock().map(|items| items.clone()).unwrap_or_default();
        Ok(out)
    }

    pub async fn click_and_wait_for_download(
        &self,
        selector: &str,
        download_dir: &Path,
        timeout_ms: u64,
    ) -> Result<DownloadEvent> {
        tokio::fs::create_dir_all(download_dir).await?;

        self.cdp
            .send_request(
                "Browser.setDownloadBehavior",
                serde_json::json!({
                    "behavior": "allowAndName",
                    "downloadPath": download_dir.to_string_lossy(),
                    "eventsEnabled": true,
                }),
            )
            .await?;

        let pending = Arc::new(StdMutex::new(PendingDownload::default()));
        let pending_ref = Arc::clone(&pending);
        self.cdp
            .on_event("Browser.downloadWillBegin", move |params| {
                if let Ok(mut lock) = pending_ref.lock() {
                    lock.guid = params
                        .get("guid")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned);
                    lock.url = params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned);
                    lock.suggested_filename = params
                        .get("suggestedFilename")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned);
                }
            })
            .await;

        let (tx, rx) = oneshot::channel();
        let tx_ref = Arc::new(StdMutex::new(Some(tx)));
        let tx_clone = Arc::clone(&tx_ref);
        let pending_progress = Arc::clone(&pending);
        let download_dir = download_dir.to_path_buf();
        self.cdp
            .on_event("Browser.downloadProgress", move |params| {
                let state = params
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                if state != "completed" && state != "canceled" {
                    return;
                }

                let pending = pending_progress.lock().ok().map(|v| v.clone());
                let guid = params
                    .get("guid")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .or_else(|| pending.as_ref().and_then(|p| p.guid.clone()));
                let suggested_filename =
                    pending.as_ref().and_then(|p| p.suggested_filename.clone());
                let url = pending.as_ref().and_then(|p| p.url.clone());
                let file_path = params
                    .get("filePath")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .or_else(|| {
                        suggested_filename
                            .as_ref()
                            .map(|name| download_dir.join(name))
                    })
                    .or_else(|| guid.as_ref().map(|id| download_dir.join(id)));

                if let (Some(guid), Some(file_path)) = (guid, file_path) {
                    let event = DownloadEvent {
                        guid,
                        url,
                        suggested_filename,
                        file_path: file_path.to_string_lossy().to_string(),
                        state,
                        total_bytes: params.get("totalBytes").and_then(|v| v.as_u64()),
                        received_bytes: params.get("receivedBytes").and_then(|v| v.as_u64()),
                    };
                    if let Ok(mut slot) = tx_clone.lock()
                        && let Some(sender) = slot.take()
                    {
                        let _ = sender.send(event);
                    }
                }
            })
            .await;

        self.click(selector).await?;
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let event = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| anyhow!("Timed out waiting for browser download event"))?
            .map_err(|_| anyhow!("Download event channel closed"))?;

        if event.state != "completed" {
            return Err(anyhow!("Download did not complete (state={})", event.state));
        }

        Ok(event)
    }

    pub async fn title(&self) -> Result<String> {
        let result = self.evaluate("document.title").await?;
        result
            .as_str()
            .map(|s| s.to_owned())
            .ok_or_else(|| anyhow!("Failed to get page title"))
    }

    pub async fn url(&self) -> Result<String> {
        let result = self.evaluate("window.location.href").await?;
        result
            .as_str()
            .map(|s| s.to_owned())
            .ok_or_else(|| anyhow!("Failed to get page URL"))
    }

    pub async fn content(&self) -> Result<String> {
        let result = self
            .cdp
            .send_request_to_session(
                "DOM.getDocument",
                serde_json::json!({ "depth": -1 }),
                &self.session_id,
            )
            .await?;

        let outer_html = self
            .cdp
            .send_request_to_session(
                "DOM.getOuterHTML",
                serde_json::json!({
                    "nodeId": result.get("root").and_then(|r| r.get("nodeId"))
                }),
                &self.session_id,
            )
            .await?;

        outer_html
            .get("outerHTML")
            .and_then(|h| h.as_str())
            .map(|s| s.to_owned())
            .ok_or_else(|| anyhow!("Failed to get page content"))
    }

    pub async fn screenshot(&self, options: ScreenshotOptions) -> Result<Vec<u8>> {
        let mut params = serde_json::json!({
            "format": format!("{:?}", options.format).to_lowercase(),
            "fromSurface": options.from_surface,
            "captureBeyondViewport": options.capture_beyond_viewport,
            "optimizeForSpeed": options.optimize_for_speed,
        });

        if let Some(quality) = options.quality {
            params["quality"] = serde_json::json!(quality);
        }

        if let Some(ref clip) = options.clip {
            params["clip"] = serde_json::json!(clip);
        }

        let result = self
            .cdp
            .send_request_to_session("Page.captureScreenshot", params, &self.session_id)
            .await?;

        let data = result
            .get("data")
            .and_then(|d| d.as_str())
            .ok_or_else(|| anyhow!("Failed to get screenshot data"))?;

        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(data)?;
        Ok(decoded)
    }

    pub async fn click(&self, selector: &str) -> Result<()> {
        let element = self.query_selector(selector).await?;
        element.click().await
    }

    pub async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let element = self.query_selector(selector).await?;
        element.type_text(text).await
    }

    pub async fn close(&self) -> Result<()> {
        self.cdp
            .send_request(
                "Target.closeTarget",
                serde_json::json!({
                    "targetId": self.target_id,
                }),
            )
            .await?;
        Ok(())
    }

    #[must_use]
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

fn parse_network_response_event(params: &Value) -> Option<NetworkResponseEvent> {
    let response = params.get("response")?;
    let status = response.get("status")?.as_f64()?;
    Some(NetworkResponseEvent {
        request_id: params.get("requestId")?.as_str()?.to_owned(),
        url: response.get("url")?.as_str()?.to_owned(),
        status: status.round() as u16,
        mime_type: response
            .get("mimeType")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        headers: response.get("headers").cloned().unwrap_or(Value::Null),
    })
}
