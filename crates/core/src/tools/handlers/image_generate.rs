use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Generates an image via the OpenAI DALL-E API and saves it to disk.
pub struct ImageGenerateHandler;

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
        "1024x1024".to_string()
    }

    pub fn model() -> String {
        "dall-e-3".to_string()
    }

    pub fn quality() -> String {
        "standard".to_string()
    }
}

#[async_trait]
impl ToolHandler for ImageGenerateHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "image_generate requires function payload".to_string(),
                ));
            }
        };

        let args: ImageGenerateArgs = parse_arguments(&arguments)?;

        let api_base = std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            FunctionCallError::RespondToModel(
                "OPENAI_API_KEY environment variable not set".to_string(),
            )
        })?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

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
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("image generation request failed: {err}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            return Err(FunctionCallError::RespondToModel(format!(
                "image generation API error (HTTP {status}): {resp_body}"
            )));
        }

        let resp_json: serde_json::Value = response.json().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse response: {err}"))
        })?;

        let image_url = resp_json["data"][0]["url"].as_str().ok_or_else(|| {
            FunctionCallError::RespondToModel("no image URL in response".to_string())
        })?;

        // Download the generated image.
        let image_response = client.get(image_url).send().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to download image: {err}"))
        })?;

        let image_bytes = image_response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read image: {err}"))
        })?;

        // Save to the specified output path.
        let output_path = std::path::Path::new(&args.output_path);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to create directory: {err}"))
            })?;
        }
        std::fs::write(output_path, &image_bytes).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to save image: {err}"))
        })?;

        let result = serde_json::json!({
            "status": "success",
            "path": args.output_path,
            "size": args.size,
            "model": args.model,
            "bytes": image_bytes.len(),
        });

        Ok(ToolOutput::Function {
            content: result.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
