pub mod a2a_types;
mod agent_step;
mod agents_list;
pub mod apply_patch;
mod browser;
mod canvas;
mod channel_tools;
pub(crate) mod collab;
mod cron;
mod discord_actions;
mod dynamic;
mod gateway;
mod gateway_status;
mod grep_files;
mod image_analyze;
mod image_generate;
mod list_dir;
mod llm_task;
mod mcp;
mod mcp_resource;
mod md_memory;
mod memory;
mod message;
mod nodes;
mod plan;
mod process;
mod read_file;
mod request_user_input;
mod sessions;
mod sessions_send_a2a;
mod sessions_spawn;
mod shell;
mod slack_actions;
mod telegram_actions;
mod test_sync;
mod tts;
mod unified_exec;
mod view_image;
mod web_fetch;
mod web_search;
mod whatsapp_actions;
mod write_file;

pub use agent_step::AgentStepHandler;
pub use agents_list::AgentsListHandler;
pub use apply_patch::ApplyPatchHandler;
pub use browser::BrowserHandler;
pub use canvas::CanvasHandler;
pub use channel_tools::ChannelToolsHandler;
pub use collab::CollabHandler;
pub use cron::CronHandler;
pub use discord_actions::DiscordActionsHandler;
pub use dynamic::DynamicToolHandler;
pub use gateway::GatewayToolHandler;
pub use gateway_status::GatewayStatusHandler;
pub use grep_files::GrepFilesHandler;
pub use image_analyze::ImageAnalyzeHandler;
pub use image_generate::ImageGenerateHandler;
pub use list_dir::ListDirHandler;
pub use llm_task::LlmTaskHandler;
pub use mcp::McpHandler;
pub use mcp_resource::McpResourceHandler;
pub use md_memory::MdMemoryHandler;
pub use memory::MemoryHandler;
pub use message::MessageHandler;
pub use nodes::NodesHandler;
pub use plan::{PLAN_TOOL, PlanHandler};
pub use process::ProcessHandler;
pub use read_file::ReadFileHandler;
pub use request_user_input::RequestUserInputHandler;
use serde::Deserialize;
pub use sessions::SessionsHandler;
pub use sessions_send_a2a::SessionsSendA2AHandler;
pub use sessions_spawn::SessionsSpawnHandler;
pub use shell::{ShellCommandHandler, ShellHandler};
pub use slack_actions::SlackActionsHandler;
pub use telegram_actions::TelegramActionsHandler;
pub use test_sync::TestSyncHandler;
pub use tts::TtsHandler;
pub use unified_exec::UnifiedExecHandler;
pub use view_image::ViewImageHandler;
pub use web_fetch::WebFetchHandler;
pub use web_search::WebSearchHandler;
pub use whatsapp_actions::WhatsAppActionsHandler;
pub use write_file::WriteFileHandler;

use crate::function_tool::FunctionCallError;

const MAX_TOOL_ERROR_BODY_CHARS: usize = 2_000;

fn parse_arguments<T>(arguments: &str) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })
}

/// Extract a required non-empty string from an `Option<String>` field.
pub(crate) fn require_field<'a>(
    field: &'a Option<String>,
    name: &str,
) -> Result<&'a str, FunctionCallError> {
    field
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| FunctionCallError::RespondToModel(format!("missing required field: {name}")))
}

pub(crate) fn reqwest_error_without_url(prefix: &str, err: reqwest::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("{prefix}: {}", err.without_url()))
}

pub(crate) fn sanitize_error_body(body: &str, secrets: &[&str]) -> String {
    let mut sanitized = body.to_owned();
    for secret in secrets {
        let secret = secret.trim();
        if !secret.is_empty() {
            sanitized = sanitized.replace(secret, "[redacted]");
        }
    }

    if sanitized.chars().count() <= MAX_TOOL_ERROR_BODY_CHARS {
        return sanitized;
    }

    let truncated = sanitized
        .chars()
        .take(MAX_TOOL_ERROR_BODY_CHARS)
        .collect::<String>();
    format!("{truncated}\n[response truncated]")
}
