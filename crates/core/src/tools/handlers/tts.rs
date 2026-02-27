use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct TtsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct TtsArgs {
    /// Text to convert to speech.
    text: String,
    /// Voice name (e.g., "alloy", "echo", "fable", "onyx", "nova", "shimmer").
    #[serde(default = "defaults::voice")]
    voice: String,
    /// Output file path for the audio (MP3).
    output_path: String,
}

mod defaults {
    pub fn voice() -> String {
        "alloy".to_string()
    }
}

#[async_trait]
impl ToolHandler for TtsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { turn, payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "tts handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: TtsArgs = parse_arguments(&arguments)?;

        if args.text.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "text must not be empty".to_string(),
            ));
        }

        // Resolve the output path.
        let output_path = turn.resolve_path(Some(args.output_path));

        // Ensure parent directory exists.
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to create output directory `{}`: {err}",
                    parent.display()
                ))
            })?;
        }

        // Get the API key from the environment.
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            FunctionCallError::RespondToModel(
                "TTS requires OPENAI_API_KEY environment variable to be set.".to_string(),
            )
        })?;

        let api_base = std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        let body = serde_json::json!({
            "model": "tts-1",
            "input": args.text,
            "voice": args.voice,
        });

        let url = format!("{api_base}/audio/speech");
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("TTS API request failed: {err}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response.bytes().await.unwrap_or_default();
            let err_text = String::from_utf8_lossy(&err_body);
            return Err(FunctionCallError::RespondToModel(format!(
                "TTS API returned HTTP {status}: {err_text}"
            )));
        }

        let audio_bytes = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read TTS response: {err}"))
        })?;

        tokio::fs::write(&output_path, &audio_bytes)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to write audio to `{}`: {err}",
                    output_path.display()
                ))
            })?;

        Ok(ToolOutput::Function {
            content: format!(
                "Audio generated ({} bytes) and saved to `{}`",
                audio_bytes.len(),
                output_path.display()
            ),
            content_items: None,
            success: Some(true),
        })
    }
}
