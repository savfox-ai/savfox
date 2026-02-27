use async_trait::async_trait;
use savfox_protocol::models::{
    ContentItem, ResponseInputItem, local_image_content_items_with_label_number,
};
use serde::Deserialize;
use tokio::fs;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct ImageAnalyzeHandler;

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

#[async_trait]
impl ToolHandler for ImageAnalyzeHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "image_analyze handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ImageAnalyzeArgs = parse_arguments(&arguments)?;

        let abs_path = turn.resolve_path(Some(args.path));

        let metadata = fs::metadata(&abs_path).await.map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "unable to locate image at `{}`: {error}",
                abs_path.display()
            ))
        })?;

        if !metadata.is_file() {
            return Err(FunctionCallError::RespondToModel(format!(
                "image path `{}` is not a file",
                abs_path.display()
            )));
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

        Ok(ToolOutput::Function {
            content: format!(
                "Image at `{}` attached for analysis with prompt: {}",
                abs_path.display(),
                args.prompt
            ),
            content_items: None,
            success: Some(true),
        })
    }
}
