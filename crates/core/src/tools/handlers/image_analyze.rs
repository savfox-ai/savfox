use savfox_protocol::models::{
    ContentItem, ResponseInputItem, local_image_content_items_with_label_number,
};
use serde::Deserialize;
use tokio::fs;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

#[derive(Deserialize)]
struct ImageAnalyzeArgs {
    /// Path to the local image file.
    path: String,
    /// Analysis prompt describing what to look for in the image.
    prompt: String,
    /// Detail level: "low" or "high" (default "high").
    #[serde(default = "defaults::detail")]
    detail: String,
}

mod defaults {
    pub fn detail() -> String {
        "high".to_string()
    }
}

pub struct ImageAnalyzeHandler;

#[async_trait::async_trait]
impl ToolHandler for ImageAnalyzeHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ImageAnalyzeHandler received unsupported payload"),
        };
        let args: ImageAnalyzeArgs = parse_arguments(&arguments)?;

        let session = &invocation.session;
        let turn = &invocation.turn;

        let abs_path = turn.resolve_path(Some(args.path));

        let metadata = fs::metadata(&abs_path).await.map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "unable to locate image at `{}`: {error}",
                abs_path.display()
            ))
        })?;

        if !metadata.is_file() {
            return model_err(format!("image path `{}` is not a file", abs_path.display()));
        }

        // Build content items: image + analysis prompt.
        let mut content: Vec<ContentItem> =
            local_image_content_items_with_label_number(&abs_path, None);

        // Append the user's analysis prompt so the model sees both the image
        // and the instruction in the same message.
        let prompt_text = if args.detail == "low" {
            format!("Analyze this image briefly (low detail). {}", args.prompt)
        } else {
            format!("Analyze this image in detail. {}", args.prompt)
        };

        content.push(ContentItem::InputText { text: prompt_text });

        let input = ResponseInputItem::Message {
            role: "user".to_string(),
            content,
        };

        session
            .inject_response_items(vec![input])
            .await
            .map_err(|_| {
                FunctionCallError::RespondToModel(
                    "unable to attach image for analysis (no active task)".to_string(),
                )
            })?;

        Ok(ToolOutput::ok(format!(
            "Image at `{}` attached for analysis with prompt: {}",
            abs_path.display(),
            args.prompt
        )))
    }
}
