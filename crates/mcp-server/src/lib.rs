//! Prototype MCP server.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub)]
#![allow(clippy::manual_let_else, clippy::needless_continue)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::io::{ErrorKind, Result as IoResult};
use std::path::PathBuf;

use rmcp::model::{ClientNotification, ClientRequest, JsonRpcMessage};
use savfox_common::service_runtime::{
    DEFAULT_CHANNEL_CAPACITY, init_stderr_tracing, spawn_stdin_json_reader,
};
use savfox_core::config::Config;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, {self}};
use tokio::sync::mpsc;
use tracing::{error, info};

mod agent_tool_config;
mod agent_tool_runner;
mod exec_approval;
pub(crate) mod message_processor;
mod outgoing_message;
mod patch_approval;

pub use agent_tool_config::{SavfoxToolCallParam, SavfoxToolCallReplyParam};

pub use crate::exec_approval::{ExecApprovalElicitRequestParams, ExecApprovalResponse};
use crate::message_processor::MessageProcessor;
use crate::outgoing_message::{OutgoingJsonRpcMessage, OutgoingMessage, OutgoingMessageSender};
pub use crate::patch_approval::{PatchApprovalElicitRequestParams, PatchApprovalResponse};

type IncomingMessage = JsonRpcMessage<ClientRequest, Value, ClientNotification>;

pub async fn run_main(savfox_linux_sandbox_exe: Option<PathBuf>) -> IoResult<()> {
    init_stderr_tracing("info");

    // Set up channels.
    let (incoming_tx, mut incoming_rx) =
        mpsc::channel::<IncomingMessage>(DEFAULT_CHANNEL_CAPACITY);
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();

    // Task: read from stdin, push to `incoming_tx`.
    let stdin_reader_handle =
        spawn_stdin_json_reader(incoming_tx, "Failed to deserialize JSON-RPC message");

    // Load configuration.
    let config =
        Config::load_with_cli_overrides_and_harness_overrides(Vec::new(), Default::default())
            .await
            .map_err(|e| {
                std::io::Error::new(ErrorKind::InvalidData, format!("error loading config: {e}"))
            })?;

    // Task: process incoming messages.
    let processor_handle = tokio::spawn({
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);
        let mut processor = MessageProcessor::new(
            outgoing_message_sender,
            savfox_linux_sandbox_exe,
            std::sync::Arc::new(config),
        );
        async move {
            while let Some(msg) = incoming_rx.recv().await {
                match msg {
                    JsonRpcMessage::Request(r) => processor.process_request(r).await,
                    JsonRpcMessage::Response(r) => processor.process_response(r).await,
                    JsonRpcMessage::Notification(n) => processor.process_notification(n).await,
                    JsonRpcMessage::Error(e) => processor.process_error(e),
                }
            }

            info!("processor task exited (channel closed)");
        }
    });

    // Task: write outgoing messages to stdout.
    let stdout_writer_handle = tokio::spawn(async move {
        let mut stdout = io::stdout();
        while let Some(outgoing_message) = outgoing_rx.recv().await {
            let msg: OutgoingJsonRpcMessage = outgoing_message.into();
            match serde_json::to_string(&msg) {
                Ok(json) => {
                    if let Err(e) = stdout.write_all(json.as_bytes()).await {
                        error!("Failed to write to stdout: {e}");
                        break;
                    }
                    if let Err(e) = stdout.write_all(b"\n").await {
                        error!("Failed to write newline to stdout: {e}");
                        break;
                    }
                }
                Err(e) => error!("Failed to serialize JSON-RPC message: {e}"),
            }
        }

        info!("stdout writer exited (channel closed)");
    });

    // Wait for all tasks to finish.  The typical exit path is the stdin reader
    // hitting EOF which, once it drops `incoming_tx`, propagates shutdown to
    // the processor and then to the stdout task.
    let _ = tokio::join!(stdin_reader_handle, processor_handle, stdout_writer_handle);

    Ok(())
}
