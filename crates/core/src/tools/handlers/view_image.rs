use savfox_protocol::items::{ImageViewItem, TurnItem};
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
struct ViewImageArgs {
    path: String,
}

pub struct ViewImageHandler;

#[async_trait::async_trait]
impl ToolHandler for ViewImageHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ViewImageHandler received unsupported payload"),
        };
        let args: ViewImageArgs = parse_arguments(&arguments)?;
        let abs_path = invocation.turn.resolve_path(Some(args.path));

        let metadata = fs::metadata(&abs_path).await.map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "unable to locate image at `{}`: {error}",
                abs_path.display()
            ))
        })?;

        if !metadata.is_file() {
            return model_err(format!("image path `{}` is not a file", abs_path.display()));
        }
        let event_path = abs_path.clone();

        let content: Vec<ContentItem> =
            local_image_content_items_with_label_number(&abs_path, None);
        let input = ResponseInputItem::Message {
            role: "user".to_owned(),
            content,
        };

        invocation
            .session
            .inject_response_items(vec![input])
            .await
            .map_err(|_| {
                FunctionCallError::RespondToModel(
                    "unable to attach image (no active task)".to_owned(),
                )
            })?;

        let item = TurnItem::ImageView(ImageViewItem {
            id: invocation.call_id.clone(),
            path: event_path,
        });
        invocation
            .session
            .emit_turn_item_started(invocation.turn.as_ref(), &item)
            .await;
        invocation
            .session
            .emit_turn_item_completed(invocation.turn.as_ref(), item)
            .await;

        Ok(ToolOutput::ok("attached local image path".to_owned()))
    }
}
