#![warn(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub, dead_code)]
#![allow(missing_debug_implementations)]
#![allow(
    clippy::enum_variant_names,
    clippy::field_reassign_with_default,
    clippy::filter_map_next,
    clippy::future_not_send,
    clippy::if_same_then_else,
    clippy::manual_clamp,
    clippy::manual_let_else,
    clippy::module_inception,
    clippy::needless_pass_by_ref_mut,
    clippy::option_option,
    clippy::ptr_arg,
    clippy::question_mark,
    clippy::return_self_not_must_use,
    clippy::should_implement_trait,
    clippy::too_many_arguments,
    clippy::unused_self
)]
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![recursion_limit = "512"]

use std::io::{ErrorKind, Result as IoResult};
use std::path::PathBuf;
use std::sync::Arc;

use savfox_common::service_runtime::env_filter_from_default;
use savfox_core::config::{Config, ConfigBuilder};
use savfox_core::config_loader::CloudRequirementsLoader;
use savfox_feedback::SavfoxFeedback;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod agent_terminal_delegate;
mod approval_policy_store;
pub mod audit;
pub mod auto_reply;
mod cached_db;
pub mod canvas_host;
pub mod channel;
#[path = "channels/mod.rs"]
pub mod channels;
pub(crate) mod chat_attachments;
pub(crate) mod chat_sanitize;
pub(crate) mod chat_session;
pub mod compaction;
pub mod config;
pub mod cron_service;
mod daemon;
pub(crate) mod discovery;
pub mod dm_policy;
mod exec_approval;
pub mod gateway_cli;
pub(crate) mod home_paths;
pub mod hooks;
pub mod identity_links;
mod json_store;
mod log_level;
mod log_store;
pub(crate) mod maintenance;
pub mod media_store;
pub mod media_understanding;
pub mod memory_service;
pub mod message_queue;
pub mod node_actions;
pub(crate) mod otel;
pub mod pairing_store;
pub mod plugin;
pub mod protocol;
pub mod provider_health;
pub mod resilience;
pub mod response_chunker;
pub mod runtime;
pub mod security;
pub mod send_policy;
mod server;
pub mod skills_api;
mod skills_store;
mod static_assets;
pub(crate) mod tailscale;
mod tools_invoke;
pub mod utils;
pub mod voice;
pub mod web;
mod webchat;
mod webhooks;
mod wizard_store;
pub mod ws;
pub mod ws_rpc;

pub use config::{GatewayCommand, GatewaySubcommand};
pub(crate) use runtime::agent_routing;
pub use runtime::{routing, session};
pub use security::{auth, rate_limit, redaction, security_audit, ssrf};
pub use voice::{stt, talk_mode, voice_wake};
pub(crate) use voice::{tts_deepgram, tts_edge, tts_service, voice_store};

use crate::auth::GatewayAuth;
use crate::channel::{BridgeOutgoing, GatewayBridgeArgs, GatewayChannel};
use crate::config::GatewayConfig;
use crate::cron_service::CronService;
use crate::session::{GatewaySessionManager, SessionStore};

/// Main entry point for the gateway server.
///
/// This mirrors the pattern from `savfox_app_server::run_main` but sets up
/// an HTTP/WebSocket server instead of reading from stdin/stdout.
pub async fn run_main(
    gateway_config: GatewayConfig,
    savfox_linux_sandbox_exe: Option<PathBuf>,
) -> IoResult<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Install tracing subscriber with a reloadable filter so the log level
    // can be changed at runtime via the `log.set_level` RPC.
    let env_filter = env_filter_from_default("info");
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

    let stderr_fmt = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    let log_capture = log_store::LogCaptureLayer;

    let _ = tracing_subscriber::registry()
        .with(filter_layer)
        .with(stderr_fmt)
        .with(log_capture)
        .try_init();

    log_level::set_reload_handle(reload_handle);

    info!("Savfox Gateway Server starting...");

    // Load configuration.
    let cloud_requirements = CloudRequirementsLoader::default();

    let config = match ConfigBuilder::default()
        .cloud_requirements(cloud_requirements.clone())
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            error!("failed to load config: {err}");
            Config::load_default_with_cli_overrides(Vec::new()).map_err(|e| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("error loading default config: {e}"),
                )
            })?
        }
    };

    let config = Arc::new(config);
    let feedback = SavfoxFeedback::new();

    info!(savfox_home = %config.savfox_home.display(), "configuration loaded");

    // Set up the gateway token.
    let token = gateway_config
        .token
        .clone()
        .unwrap_or_else(GatewayAuth::generate_token);

    let auth = Arc::new(GatewayAuth::single_token(token.clone()));
    let session_mgr = Arc::new(GatewaySessionManager::new());
    let channel_registry = channels::create_channel_registry();
    let session_store = Arc::new(SessionStore::from_home(&config.savfox_home));
    info!("session store initialized");

    // Create the channel outgoing channel.
    let (outgoing_tx, _outgoing_rx) = mpsc::channel::<BridgeOutgoing>(128);

    let channel = Arc::new(GatewayChannel::new(GatewayBridgeArgs {
        config: Arc::clone(&config),
        session_store: Arc::clone(&session_store),
        cli_overrides: Vec::new(),
        cloud_requirements,
        feedback,
        savfox_linux_sandbox_exe,
        websocket_manager: (*session_mgr).clone(),
        outgoing_tx,
        channel_registry: channel_registry.clone(),
    }));

    info!("gateway channel created");

    // Inject API keys from per-provider files into the runtime env-override
    // map so the core engine can authenticate without a restart.
    ws_rpc::inject_all_provider_auth(&channel).await;

    // Initialize and start the cron background service.
    let cron_service = Arc::new(CronService::from_home(&config.savfox_home));
    cron_service.init().await;
    let _cron_shutdown = cron_service.start(Arc::clone(&channel));
    info!("cron service started");

    // Spawn a background task to periodically prune stale sessions (every 5 minutes).
    {
        let store = Arc::clone(&session_store);
        let config_for_prune = Arc::clone(&config);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let report = store.prune_report().await;
                channels::runtime::cleanup_session_runtime_state(
                    &config_for_prune.savfox_home,
                    &report.session_ids,
                )
                .await;
                let pruned = report.pruned;
                if pruned > 0 {
                    info!(pruned, "session store maintenance: pruned stale entries");
                }
            }
        });
    }

    info!("migrating channel configurations");
    savfox_core::config::channel_store::migrate_channel_configs(&config.savfox_home).await;

    info!("loading channel configurations");
    if let Err(err) = channels::log_all_configured_channels(&config.savfox_home).await {
        warn!(error = %err, "failed to load channel configs for startup logging");
    }
    info!("channel configuration logging complete");

    // Initialize and start configured channels.
    info!("initializing channel instances");
    if let Err(err) = channels::initialize_and_start_channels(
        &config.savfox_home,
        channel_registry.clone(),
        &channel,
        &session_store,
    )
    .await
    {
        warn!(error = %err, "some channels failed to initialize");
    }
    info!("channel initialization complete");

    channels::runtime::resume_pending_idle_replies(
        &config.savfox_home,
        Arc::clone(&channel),
        Arc::clone(&session_store),
    )
    .await;
    info!("idle reply pending timers resumed");

    // Print startup info.
    let scheme = if gateway_config.tls_cert.is_some() {
        "wss"
    } else {
        "ws"
    };
    let http_scheme = if gateway_config.tls_cert.is_some() {
        "https"
    } else {
        "http"
    };

    info!("Savfox Gateway Server v{}", env!("CARGO_PKG_VERSION"));
    info!(
        "WebSocket: {scheme}://{}:{}/ws",
        gateway_config.host, gateway_config.port
    );
    info!(
        "Health: {http_scheme}://{}:{}/health",
        gateway_config.host, gateway_config.port
    );
    info!("Token: {token}");

    // Also print to stderr for the operator (intentional — shown before
    // tracing is fully active so the operator always sees connection info).
    #[allow(clippy::print_stderr)]
    {
        eprintln!("Savfox Gateway Server v{}", env!("CARGO_PKG_VERSION"));
        eprintln!(
            "  WebSocket: {scheme}://{}:{}/ws",
            gateway_config.host, gateway_config.port
        );
        eprintln!(
            "  Health:    {http_scheme}://{}:{}/health",
            gateway_config.host, gateway_config.port
        );
        eprintln!("  Token:     {token}");
        eprintln!();
    }

    // Start the HTTP/WebSocket server (blocks until shutdown).
    server::start_server(
        &gateway_config,
        auth,
        session_mgr,
        channel,
        session_store,
        cron_service,
        config.savfox_home.clone(),
    )
    .await
}
