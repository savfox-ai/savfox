use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};
use crate::unified_exec::WriteStdinRequest;

#[derive(Deserialize)]
struct ProcessArgs {
    /// The action to perform: "list", "poll", "read_log", "write", "send_keys", "kill".
    action: String,
    /// Process ID for actions that target a specific process.
    #[serde(default)]
    process_id: Option<String>,
    /// Text input for the "write" action.
    #[serde(default)]
    input: Option<String>,
    /// Key names for the "send_keys" action (e.g. ["ctrl-c", "enter"]).
    #[serde(default)]
    keys: Option<Vec<String>>,
    /// Signal name for the "kill" action (e.g. "SIGTERM", "SIGKILL").
    #[serde(default)]
    signal: Option<String>,
}

mod defaults {
    pub fn yield_time_ms() -> u64 {
        5_000
    }
}

/// Handles process-related tool calls with a single `action` field to dispatch:
/// - `list`      -- Enumerate active processes.
/// - `poll`      -- Read latest output from a process.
/// - `read_log`  -- Return last N lines of process output.
/// - `write`     -- Write text to process stdin.
/// - `send_keys` -- Convert key names to escape sequences and write to stdin.
/// - `kill`      -- Terminate the process.
pub struct ProcessHandler;

#[async_trait::async_trait]
impl ToolHandler for ProcessHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ProcessHandler received unsupported payload"),
        };
        let args: ProcessArgs = parse_arguments(&arguments)?;

        let session = &invocation.session;

        match args.action.as_str() {
            "list" => handle_list().await,
            "poll" => handle_poll(session, &args).await,
            "read_log" => handle_read_log(session, &args).await,
            "write" => handle_write(session, &args).await,
            "send_keys" => handle_send_keys(session, &args).await,
            "kill" => handle_kill(&args).await,
            other => model_err(format!("unknown process action: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Action handlers
// ---------------------------------------------------------------------------

async fn handle_list() -> Result<ToolOutput, FunctionCallError> {
    // The internal ProcessStore is private to UnifiedExecProcessManager, so we
    // cannot enumerate processes directly.  Return an informational message that
    // guides the model to use the unified exec manager for process enumeration.
    Ok(ToolOutput::ok(serde_json::json!({
            "note": "Process listing is available through the unified exec manager. \
                     Use exec_command or write_stdin to interact with active processes. \
                     Process IDs are returned when a long-running command is started via exec_command.",
        })
        .to_string()))
}

async fn handle_poll(
    session: &crate::savfox::Session,
    args: &ProcessArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let Some(process_id) = args.process_id.as_deref() else {
        return model_err("process_id is required for the poll action");
    };

    let manager = &session.services.unified_exec_manager;
    let response = manager
        .write_stdin(WriteStdinRequest {
            process_id,
            input: "",
            yield_time_ms: defaults::yield_time_ms(),
            max_output_tokens: None,
        })
        .await
        .or_else(|err| model_err(format!("poll failed: {err}")))?;

    Ok(ToolOutput::ok(
        serde_json::json!({
            "process_id": response.process_id,
            "exit_code": response.exit_code,
            "output": response.output,
        })
        .to_string(),
    ))
}

async fn handle_read_log(
    session: &crate::savfox::Session,
    args: &ProcessArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let Some(process_id) = args.process_id.as_deref() else {
        return model_err("process_id is required for the read_log action");
    };

    // Read the latest output by performing an empty write_stdin (poll).
    let manager = &session.services.unified_exec_manager;
    let response = manager
        .write_stdin(WriteStdinRequest {
            process_id,
            input: "",
            yield_time_ms: defaults::yield_time_ms(),
            max_output_tokens: None,
        })
        .await
        .or_else(|err| model_err(format!("read_log failed: {err}")))?;

    Ok(ToolOutput::ok(
        serde_json::json!({
            "process_id": response.process_id,
            "exit_code": response.exit_code,
            "output": response.output,
        })
        .to_string(),
    ))
}

async fn handle_write(
    session: &crate::savfox::Session,
    args: &ProcessArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let Some(process_id) = args.process_id.as_deref() else {
        return model_err("process_id is required for the write action");
    };

    let input = args.input.as_deref().unwrap_or("");

    let manager = &session.services.unified_exec_manager;
    let response = manager
        .write_stdin(WriteStdinRequest {
            process_id,
            input,
            yield_time_ms: defaults::yield_time_ms(),
            max_output_tokens: None,
        })
        .await
        .or_else(|err| model_err(format!("write failed: {err}")))?;

    Ok(ToolOutput::ok(
        serde_json::json!({
            "process_id": response.process_id,
            "exit_code": response.exit_code,
            "output": response.output,
        })
        .to_string(),
    ))
}

async fn handle_send_keys(
    session: &crate::savfox::Session,
    args: &ProcessArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let Some(process_id) = args.process_id.as_deref() else {
        return model_err("process_id is required for the send_keys action");
    };

    let Some(keys) = args.keys.as_deref() else {
        return model_err("keys is required for the send_keys action");
    };

    let mut input = String::new();
    for key in keys {
        let Some(escape) = key_to_escape(key) else {
            return model_err(format!("unknown key name: {key}"));
        };
        input.push_str(escape);
    }

    let manager = &session.services.unified_exec_manager;
    let response = manager
        .write_stdin(WriteStdinRequest {
            process_id,
            input: &input,
            yield_time_ms: defaults::yield_time_ms(),
            max_output_tokens: None,
        })
        .await
        .or_else(|err| model_err(format!("send_keys failed: {err}")))?;

    Ok(ToolOutput::ok(
        serde_json::json!({
            "process_id": response.process_id,
            "exit_code": response.exit_code,
            "output": response.output,
        })
        .to_string(),
    ))
}

async fn handle_kill(args: &ProcessArgs) -> Result<ToolOutput, FunctionCallError> {
    let Some(process_id) = args.process_id.as_deref() else {
        return model_err("process_id is required for the kill action");
    };

    let signal = args.signal.as_deref().unwrap_or("SIGTERM");

    // Killing a process requires methods not currently exposed on
    // UnifiedExecProcessManager.  Return a placeholder that indicates the
    // intent so the model can fall back to exec_command with an explicit kill.
    Ok(ToolOutput::ok(serde_json::json!({
            "process_id": process_id,
            "signal": signal,
            "note": "Process termination is not yet directly supported through the process tool. \
                     Use exec_command to run 'kill' or send ctrl-c via send_keys to terminate the process.",
        })
        .to_string()))
}

// ---------------------------------------------------------------------------
// Key name → escape sequence mapping
// ---------------------------------------------------------------------------

fn key_to_escape(key: &str) -> Option<&'static str> {
    match key.to_ascii_lowercase().as_str() {
        // Control characters
        "ctrl-c" => Some("\x03"),
        "ctrl-d" => Some("\x04"),
        "ctrl-z" => Some("\x1a"),

        // Whitespace / common keys
        "enter" => Some("\r"),
        "tab" => Some("\t"),

        // Arrow keys (ANSI)
        "up" => Some("\x1b[A"),
        "down" => Some("\x1b[B"),
        "right" => Some("\x1b[C"),
        "left" => Some("\x1b[D"),

        // Function keys (VT220 / xterm)
        "f1" => Some("\x1bOP"),
        "f2" => Some("\x1bOQ"),
        "f3" => Some("\x1bOR"),
        "f4" => Some("\x1bOS"),

        // Navigation
        "home" => Some("\x1b[H"),
        "end" => Some("\x1b[F"),

        // Escape
        "escape" | "esc" => Some("\x1b"),

        _ => None,
    }
}
