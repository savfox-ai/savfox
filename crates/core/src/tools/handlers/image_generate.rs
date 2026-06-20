use std::time::Duration;

use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
struct ImageGenerateArgs {
    /// Text prompt describing the image to generate.
    prompt: String,
    /// Local file path where the generated image will be saved.
    output_path: String,
    /// Image dimensions (e.g. "1024x1024", "1792x1024", "1024x1792").
    #[serde(default = "defaults::size")]
    size: String,
    /// Model to use for generation.
    #[serde(default = "defaults::model")]
    model: String,
    /// Image quality ("standard" or "hd").
    #[serde(default = "defaults::quality")]
    quality: String,
}

mod defaults {
    pub fn size() -> String {
        "1024x1024".to_owned()
    }

    pub fn model() -> String {
        "dall-e-3".to_owned()
    }

    pub fn quality() -> String {
        "standard".to_owned()
    }
}

/// Generates an image via the OpenAI DALL-E API and saves it to disk.
pub struct ImageGenerateHandler;

#[async_trait::async_trait]
impl ToolHandler for ImageGenerateHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ImageGenerateHandler received unsupported payload"),
        };
        let args: ImageGenerateArgs = parse_arguments(&arguments)?;
        let turn = &invocation.turn;

        let api_base = std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| model_err("OPENAI_API_KEY environment variable not set"))?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .or_else(|err| model_err(format!("failed to build HTTP client: {err}")))?;

        let url = format!("{api_base}/images/generations");
        let body = serde_json::json!({
            "model": args.model,
            "prompt": args.prompt,
            "n": 1,
            "size": args.size,
            "quality": args.quality,
            "response_format": "url",
        });

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .or_else(|err| model_err(format!("image generation request failed: {err}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            return model_err(format!(
                "image generation API error (HTTP {status}): {resp_body}"
            ));
        }

        let resp_json: serde_json::Value = response
            .json()
            .await
            .or_else(|err| model_err(format!("failed to parse response: {err}")))?;

        let Some(image_url) = resp_json["data"][0]["url"].as_str() else {
            return model_err("no image URL in response");
        };

        // Download the generated image.
        let image_response = client
            .get(image_url)
            .send()
            .await
            .or_else(|err| model_err(format!("failed to download image: {err}")))?;

        let image_bytes = image_response
            .bytes()
            .await
            .or_else(|err| model_err(format!("failed to read image: {err}")))?;

        // Resolve the output path through the turn sandbox to prevent writes
        // outside the workspace (path traversal / absolute-path escape).
        let output_path = turn.resolve_path(Some(args.output_path));
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .or_else(|err| model_err(format!("failed to create directory: {err}")))?;
        }
        std::fs::write(&output_path, &image_bytes)
            .or_else(|err| model_err(format!("failed to save image: {err}")))?;

        let result = serde_json::json!({
            "status": "success",
            "path": output_path.display().to_string(),
            "size": args.size,
            "model": args.model,
            "bytes": image_bytes.len(),
        });

        Ok(ToolOutput::ok(result.to_string()))
    }
}
