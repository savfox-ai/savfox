use std::collections::BTreeMap;
use std::sync::LazyLock;

use savfox_protocol::config_types::ModeKind;
use savfox_protocol::plan_tool::UpdatePlanArgs;
use savfox_protocol::protocol::EventMsg;

use super::parse_arguments;
use crate::client_common::tools::{ResponsesApiTool, ToolSpec};
use crate::function_tool::{FunctionCallError, model_err};
use crate::savfox::{Session, TurnContext};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};
use crate::tools::spec::JsonSchema;

pub static PLAN_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let mut plan_item_props = BTreeMap::new();
    plan_item_props.insert("step".to_owned(), JsonSchema::String { description: None });
    plan_item_props.insert(
        "status".to_owned(),
        JsonSchema::String {
            description: Some("One of: pending, in_progress, completed".to_owned()),
        },
    );

    let plan_items_schema = JsonSchema::Array {
        description: Some("The list of steps".to_owned()),
        items: Box::new(JsonSchema::Object {
            properties: plan_item_props,
            required: Some(vec!["step".to_owned(), "status".to_owned()]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut properties = BTreeMap::new();
    properties.insert(
        "explanation".to_owned(),
        JsonSchema::String { description: None },
    );
    properties.insert("plan".to_owned(), plan_items_schema);

    ToolSpec::Function(ResponsesApiTool {
        name: "update_plan".to_owned(),
        description: r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
"#
        .to_owned(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["plan".to_owned()]),
            additional_properties: Some(false.into()),
        },
    })
});

/// Dummy args type — plan uses its own parsing via `parse_update_plan_arguments`.
#[derive(serde::Deserialize)]
struct PlanArgs {
    #[serde(flatten)]
    _raw: serde_json::Value,
}

pub struct PlanHandler;

#[async_trait::async_trait]
impl ToolHandler for PlanHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("PlanHandler received unsupported payload"),
        };
        let _args: PlanArgs = parse_arguments(&arguments)?;

        let session = &invocation.session;
        let turn = &invocation.turn;
        let call_id = invocation.call_id.clone();

        // Re-extract the raw arguments string from the payload for handle_update_plan.
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => {
                return model_err("update_plan handler received unsupported payload");
            }
        };

        let content =
            handle_update_plan(session.as_ref(), turn.as_ref(), arguments, call_id).await?;

        Ok(ToolOutput::ok(content))
    }
}

/// This function doesn't do anything useful. However, it gives the model a structured way to record
/// its plan that clients can read and render. So it's the _inputs_ to this function that are useful
/// to clients, not the outputs and neither are actually useful for the model other than forcing it
/// to come up and document a plan (TBD how that affects performance).
pub(crate) async fn handle_update_plan(
    session: &Session,
    turn_context: &TurnContext,
    arguments: String,
    _call_id: String,
) -> Result<String, FunctionCallError> {
    if turn_context.collaboration_mode.mode == ModeKind::Plan {
        return model_err("update_plan is a TODO/checklist tool and is not allowed in Plan mode");
    }
    let args = parse_update_plan_arguments(&arguments)?;
    session
        .send_event(turn_context, EventMsg::PlanUpdate(args))
        .await;
    Ok("Plan updated".to_owned())
}

fn parse_update_plan_arguments(arguments: &str) -> Result<UpdatePlanArgs, FunctionCallError> {
    serde_json::from_str::<UpdatePlanArgs>(arguments).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e}"))
    })
}
