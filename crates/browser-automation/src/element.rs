use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::cdp::CDPClient;

pub struct Element {
    node_id: u64,
    selector: String,
    session_id: String,
    cdp: Arc<CDPClient>,
}

impl Element {
    pub fn new(node_id: u64, selector: String, session_id: String, cdp: Arc<CDPClient>) -> Self {
        Self {
            node_id,
            selector,
            session_id,
            cdp,
        }
    }

    pub async fn text_content(&self) -> Result<String> {
        let result = self
            .cdp
            .send_request_to_session(
                "DOM.getOuterHTML",
                serde_json::json!({ "nodeId": self.node_id }),
                &self.session_id,
            )
            .await?;

        result
            .get("outerHTML")
            .and_then(|h| h.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("Failed to get element text"))
    }

    pub async fn attribute(&self, name: &str) -> Result<Option<String>> {
        let result = self
            .cdp
            .send_request_to_session(
                "DOM.getAttribute",
                serde_json::json!({
                    "nodeId": self.node_id,
                    "name": name,
                }),
                &self.session_id,
            )
            .await?;

        Ok(result
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    pub async fn set_attribute(&self, name: &str, value: &str) -> Result<()> {
        self.cdp
            .send_request_to_session(
                "DOM.setAttributeValue",
                serde_json::json!({
                    "nodeId": self.node_id,
                    "name": name,
                    "value": value,
                }),
                &self.session_id,
            )
            .await?;
        Ok(())
    }

    pub async fn click(&self) -> Result<()> {
        let scroll = self
            .cdp
            .send_request_to_session(
                "DOM.scrollIntoViewIfNeeded",
                serde_json::json!({ "nodeId": self.node_id }),
                &self.session_id,
            )
            .await;

        if scroll.is_err() {
            self.evaluate("this.scrollIntoView({ block: 'center' })")
                .await?;
        }

        let box_model = self
            .cdp
            .send_request_to_session(
                "DOM.getBoxModel",
                serde_json::json!({ "nodeId": self.node_id }),
                &self.session_id,
            )
            .await?;

        let content = box_model
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow!("Failed to get box model"))?;

        let x = content.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0)
            + content.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0) / 2.0;
        let y = content.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0)
            + content.get(5).and_then(|v| v.as_f64()).unwrap_or(0.0) / 2.0;

        self.cdp
            .send_request_to_session(
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mousePressed",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "clickCount": 1,
                }),
                &self.session_id,
            )
            .await?;

        self.cdp
            .send_request_to_session(
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mouseReleased",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "clickCount": 1,
                }),
                &self.session_id,
            )
            .await?;

        Ok(())
    }

    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.click().await?;

        for ch in text.chars() {
            self.cdp
                .send_request_to_session(
                    "Input.dispatchKeyEvent",
                    serde_json::json!({
                        "type": "keyDown",
                        "text": ch.to_string(),
                    }),
                    &self.session_id,
                )
                .await?;

            self.cdp
                .send_request_to_session(
                    "Input.dispatchKeyEvent",
                    serde_json::json!({
                        "type": "keyUp",
                        "text": ch.to_string(),
                    }),
                    &self.session_id,
                )
                .await?;
        }

        Ok(())
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let resolved = self
            .cdp
            .send_request_to_session(
                "DOM.resolveNode",
                serde_json::json!({ "nodeId": self.node_id }),
                &self.session_id,
            )
            .await?;

        let object_id = resolved
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|o| o.as_str())
            .ok_or_else(|| anyhow!("Failed to resolve node"))?;

        let result = self
            .cdp
            .send_request_to_session(
                "Runtime.callFunctionOn",
                serde_json::json!({
                    "functionDeclaration": format!("function() {{ return ({expression}) }}"),
                    "objectId": object_id,
                    "returnByValue": true,
                }),
                &self.session_id,
            )
            .await?;

        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn focus(&self) -> Result<()> {
        self.evaluate("this.focus()").await?;
        Ok(())
    }

    pub async fn blur(&self) -> Result<()> {
        self.evaluate("this.blur()").await?;
        Ok(())
    }

    pub async fn scroll_into_view(&self) -> Result<()> {
        self.evaluate("this.scrollIntoView({ block: 'center' })")
            .await?;
        Ok(())
    }

    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    pub fn selector(&self) -> &str {
        &self.selector
    }
}
