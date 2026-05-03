#![deny(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub)]
#![allow(
    clippy::manual_let_else,
    clippy::needless_pass_by_ref_mut,
    clippy::unused_self
)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::io::{ErrorKind, Result as IoResult};
use std::path::PathBuf;

use savfox_app_server_protocol::{
    ConfigLayerSource, ConfigWarningNotification, JsonRpcMessage, TextPosition as AppTextPosition,
    TextRange as AppTextRange,
};
use savfox_common::service_runtime::{DEFAULT_CHANNEL_CAPACITY, spawn_stdin_json_reader};
use savfox_core::config::{Config, ConfigBuilder};
use savfox_core::config_loader::{
    CloudRequirementsLoader, ConfigLayerStackOrdering, ConfigLoadError, LoaderOverrides,
    TextRange as CoreTextRange,
};
use savfox_core::{ExecPolicyError, check_execpolicy_for_warnings};
use savfox_feedback::SavfoxFeedback;
use tokio::io::{AsyncWriteExt, {self}};
use tokio::sync::mpsc;
use toml::Value as TomlValue;
use tracing::{debug, error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::message_processor::{MessageProcessor, MessageProcessorArgs};
use crate::outgoing_message::{OutgoingMessage, OutgoingMessageSender};

mod bespoke_event_handling;
mod config_api;
mod dynamic_tools;
mod error_code;
mod filters;
mod fuzzy_file_search;
mod message_processor;
mod models;
mod outgoing_message;
mod savfox_message_processor;

fn config_warning_from_error(
    summary: impl Into<String>,
    err: &std::io::Error,
) -> ConfigWarningNotification {
    let (path, range) = match config_error_location(err) {
        Some((path, range)) => (Some(path), Some(range)),
        None => (None, None),
    };
    ConfigWarningNotification {
        summary: summary.into(),
        details: Some(err.to_string()),
        path,
        range,
    }
}

fn config_error_location(err: &std::io::Error) -> Option<(String, AppTextRange)> {
    err.get_ref()
        .and_then(|err| err.downcast_ref::<ConfigLoadError>())
        .map(|err| {
            let config_error = err.config_error();
            (
                config_error.path.to_string_lossy().to_string(),
                app_text_range(&config_error.range),
            )
        })
}

fn exec_policy_warning_location(err: &ExecPolicyError) -> (Option<String>, Option<AppTextRange>) {
    match err {
        ExecPolicyError::ParsePolicy { path, source } => {
            if let Some(location) = source.location() {
                let range = AppTextRange {
                    start: AppTextPosition {
                        line: location.range.start.line,
                        column: location.range.start.column,
                    },
                    end: AppTextPosition {
                        line: location.range.end.line,
                        column: location.range.end.column,
                    },
                };
                return (Some(location.path), Some(range));
            }
            (Some(path.clone()), None)
        }
        _ => (None, None),
    }
}

fn app_text_range(range: &CoreTextRange) -> AppTextRange {
    AppTextRange {
        start: AppTextPosition {
            line: range.start.line,
            column: range.start.column,
        },
        end: AppTextPosition {
            line: range.end.line,
            column: range.end.column,
        },
    }
}

fn project_config_warning(config: &Config) -> Option<ConfigWarningNotification> {
    let mut disabled_folders = Vec::new();

    for layer in config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
    {
        if !matches!(layer.name, ConfigLayerSource::Project { .. })
            || layer.disabled_reason.is_none()
        {
            continue;
        }
        if let ConfigLayerSource::Project { dot_savfox_folder } = &layer.name {
            disabled_folders.push((
                dot_savfox_folder.as_path().display().to_string(),
                layer
                    .disabled_reason
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "config.toml is disabled.".to_owned()),
            ));
        }
    }

    if disabled_folders.is_empty() {
        return None;
    }

    let mut message = concat!(
        "Project config.toml files are disabled in the following folders. ",
        "Settings in those files are ignored, but skills and exec policies still load.\n",
    )
    .to_owned();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    Some(ConfigWarningNotification {
        summary: message,
        details: None,
        path: None,
        range: None,
    })
}

fn config_stack_startup_warnings(config: &Config) -> Vec<ConfigWarningNotification> {
    config
        .config_layer_stack
        .startup_warnings()
        .iter()
        .map(|warning| ConfigWarningNotification {
            summary: warning.clone(),
            details: None,
            path: None,
            range: None,
        })
        .collect()
}

pub async fn run_main(
    savfox_linux_sandbox_exe: Option<PathBuf>,
    loader_overrides: LoaderOverrides,
    default_analytics_enabled: bool,
) -> IoResult<()> {
    // Set up channels.
    let (incoming_tx, mut incoming_rx) =
        mpsc::channel::<JsonRpcMessage>(DEFAULT_CHANNEL_CAPACITY);
    let (outgoing_tx, mut outgoing_rx) =
        mpsc::channel::<OutgoingMessage>(DEFAULT_CHANNEL_CAPACITY);

    // Task: read from stdin, push to `incoming_tx`.
    let stdin_reader_handle =
        spawn_stdin_json_reader(incoming_tx, "Failed to deserialize JsonRpcMessage");

    // Load configuration.
    // Run personality migration from a preliminary config load.
    if let Ok(config) = ConfigBuilder::default()
        .loader_overrides(loader_overrides.clone())
        .build()
        .await
    {
        let effective_toml = config.config_layer_stack.effective_config();
        match effective_toml.try_into() {
            Ok(config_toml) => {
                if let Err(err) = savfox_core::personality_migration::maybe_migrate_personality(
                    &config.savfox_home,
                    &config_toml,
                )
                .await
                {
                    warn!(error = %err, "Failed to run personality migration");
                }
            }
            Err(err) => {
                warn!(error = %err, "Failed to deserialize config for personality migration");
            }
        }
    }
    let cloud_requirements = CloudRequirementsLoader::default();
    let loader_overrides_for_config_api = loader_overrides.clone();
    let mut config_warnings = Vec::new();
    let config = match ConfigBuilder::default()
        .loader_overrides(loader_overrides)
        .cloud_requirements(cloud_requirements.clone())
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            let message = config_warning_from_error("Invalid configuration; using defaults.", &err);
            config_warnings.push(message);
            Config::load_default_with_cli_overrides(Vec::new()).map_err(|e| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("error loading default config after config error: {e}"),
                )
            })?
        }
    };

    if let Ok(Some(err)) =
        check_execpolicy_for_warnings(&config.features, &config.config_layer_stack).await
    {
        let (path, range) = exec_policy_warning_location(&err);
        let message = ConfigWarningNotification {
            summary: "Error parsing rules; custom rules not applied.".to_owned(),
            details: Some(err.to_string()),
            path,
            range,
        };
        config_warnings.push(message);
    }

    if let Some(warning) = project_config_warning(&config) {
        config_warnings.push(warning);
    }
    config_warnings.extend(config_stack_startup_warnings(&config));

    let feedback = SavfoxFeedback::new();

    let otel = savfox_core::otel_init::build_provider(
        &config,
        env!("CARGO_PKG_VERSION"),
        Some("savfox_app_server"),
        default_analytics_enabled,
    )
    .map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("error loading otel config: {e}"),
        )
    })?;

    // Install a simple subscriber so `tracing` output is visible.  Users can
    // control the log level with `RUST_LOG`.
    let stderr_fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_filter(EnvFilter::from_default_env());

    let feedback_layer = feedback.logger_layer();
    let feedback_metadata_layer = feedback.metadata_layer();

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(stderr_fmt)
        .with(feedback_layer)
        .with(feedback_metadata_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();
    for warning in &config_warnings {
        if let Some(details) = &warning.details {
            error!("{} {}", warning.summary, details)
        } else {
            error!("{}", warning.summary)
        }
    }

    // Task: process incoming messages.
    let processor_handle = tokio::spawn({
        let outgoing_message_sender = OutgoingMessageSender::new(outgoing_tx);
        let cli_overrides: Vec<(String, TomlValue)> = Vec::new();
        let loader_overrides = loader_overrides_for_config_api;
        let mut processor = MessageProcessor::new(MessageProcessorArgs {
            outgoing: outgoing_message_sender,
            savfox_linux_sandbox_exe,
            config: std::sync::Arc::new(config),
            cli_overrides,
            loader_overrides,
            cloud_requirements: cloud_requirements.clone(),
            feedback: feedback.clone(),
            config_warnings,
        });
        let mut session_created_rx = processor.session_created_receiver();
        async move {
            let mut listen_for_sessions = true;
            loop {
                tokio::select! {
                    msg = incoming_rx.recv() => {
                        let Some(msg) = msg else {
                            break;
                        };
                        match msg {
                            JsonRpcMessage::Request(r) => processor.process_request(r).await,
                            JsonRpcMessage::Response(r) => processor.process_response(r).await,
                            JsonRpcMessage::Notification(n) => processor.process_notification(n).await,
                            JsonRpcMessage::Error(e) => processor.process_error(e).await,
                        }
                    }
                    created = session_created_rx.recv(), if listen_for_sessions => {
                        match created {
                            Ok(session_id) => {
                                processor.try_attach_session_listener(session_id).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                // TODO(jif) handle lag.
                                // Assumes session creation volume is low enough that lag never happens.
                                // If it does, we log and continue without resyncing to avoid attaching
                                // listeners for sessions that should remain unsubscribed.
                                warn!("session_created receiver lagged; skipping resync");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                listen_for_sessions = false;
                            }
                        }
                    }
                }
            }

            info!("processor task exited (channel closed)");
        }
    });

    // Task: write outgoing messages to stdout.
    let stdout_writer_handle = tokio::spawn(async move {
        let mut stdout = io::stdout();
        while let Some(outgoing_message) = outgoing_rx.recv().await {
            let Ok(value) = serde_json::to_value(outgoing_message) else {
                error!("Failed to convert OutgoingMessage to JSON value");
                continue;
            };
            match serde_json::to_string(&value) {
                Ok(mut json) => {
                    json.push('\n');
                    if let Err(e) = stdout.write_all(json.as_bytes()).await {
                        error!("Failed to write to stdout: {e}");
                        break;
                    }
                }
                Err(e) => error!("Failed to serialize JsonRpcMessage: {e}"),
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
