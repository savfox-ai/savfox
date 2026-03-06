#![warn(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub, dead_code)]
#![recursion_limit = "512"]

use std::io::{ErrorKind, Result as IoResult};
use std::path::PathBuf;
use std::sync::Arc;

use savfox_core::AuthManager;
use savfox_core::config::{Config, ConfigBuilder};
use savfox_core::config_loader::CloudRequirementsLoader;
use savfox_feedback::SavfoxFeedback;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

pub mod agent_routing;
mod approval_policy_store;
pub mod audit;
pub mod auth;
pub mod auto_reply;
#[path = "channel.rs"]
pub mod bridge;
#[path = "channels/mod.rs"]
pub mod bridges;
pub mod canvas_host;
pub(crate) mod chat_attachments;
pub(crate) mod chat_sanitize;
pub(crate) mod chat_session;
pub mod compaction;
pub mod compat;
pub mod config;
pub mod cron_service;
mod daemon;
pub(crate) mod discovery;
pub mod dm_policy;
mod exec_approval;
pub mod gateway_cli;
pub mod hooks;
pub mod identity_links;
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
pub mod rate_limit;
pub mod redaction;
pub mod resilience;
pub mod response_chunker;
pub mod routing;
pub mod security_audit;
pub mod send_policy;
mod server;
pub mod session;
pub mod skill_registry;
pub mod skills_api;
mod skills_store;
pub mod ssrf;
mod static_assets;
pub mod stt;
pub(crate) mod tailscale;
pub mod talk_mode;
pub mod tool_policy;
mod tools_invoke;
mod tts_deepgram;
mod tts_edge;
mod tts_service;
pub mod utils;
mod voice_store;
pub mod voice_wake;
pub(crate) mod webchat;
mod webhooks;
mod wizard_store;
mod ws;
mod ws_rpc;

pub use config::{GatewayCommand, GatewaySubcommand};

use crate::auth::GatewayAuth;
use crate::bridge::{BridgeOutgoing, GatewayBridge, GatewayBridgeArgs};
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
    println!("[startup] Savfox Gateway Server starting...");
    println!("[startup] Initializing logging and configuration...");
    
    // Install tracing subscriber.
    let stderr_fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_filter(EnvFilter::from_default_env());

    let _ = tracing_subscriber::registry().with(stderr_fmt).try_init();

    // Load configuration.
    let cloud_requirements = match ConfigBuilder::default().build().await {
        Ok(config) => {
            let auth_manager = AuthManager::shared(
                config.savfox_home.clone(),
                false,
                config.cli_auth_credentials_store_mode,
            );
            savfox_cloud_requirements::cloud_requirements_loader(
                auth_manager,
                config.chatgpt_base_url,
            )
        }
        Err(err) => {
            warn!(error = %err, "Failed to preload config for cloud requirements");
            CloudRequirementsLoader::default()
        }
    };

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
    
    println!("[startup] Configuration loaded successfully");
    println!("[startup] Savfox home: {:?}", config.savfox_home);

    // Set up the gateway token.
    let token = gateway_config
        .token
        .clone()
        .unwrap_or_else(GatewayAuth::generate_token);

    let auth = Arc::new(GatewayAuth::single_token(token.clone()));
    let session_mgr = Arc::new(GatewaySessionManager::new());

    // Create the bridge outgoing channel.
    let (outgoing_tx, _outgoing_rx) = mpsc::channel::<BridgeOutgoing>(128);

    let bridge = Arc::new(GatewayBridge::new(GatewayBridgeArgs {
        config: Arc::clone(&config),
        cli_overrides: Vec::new(),
        cloud_requirements,
        feedback,
        savfox_linux_sandbox_exe,
        websocket_manager: (*session_mgr).clone(),
        outgoing_tx,
    }));
    
    println!("[startup] Gateway bridge created");

    // Inject API keys from per-provider files into the runtime env-override
    // map so the core engine can authenticate without a restart.
    ws_rpc::inject_all_provider_auth(&bridge).await;

    // Initialize the persistent session store.
    let session_store = Arc::new(SessionStore::from_home(&config.savfox_home));
    info!("session store initialized");
    println!("[startup] Session store initialized");

    // Initialize and start the cron background service.
    let cron_service = Arc::new(CronService::from_home(&config.savfox_home));
    cron_service.init().await;
    let _cron_shutdown = cron_service.start(Arc::clone(&bridge));
    info!("cron service started");
    println!("[startup] Cron service started");

    // Spawn a background task to periodically prune stale sessions (every 5 minutes).
    {
        let store = Arc::clone(&session_store);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let pruned = store.prune().await;
                if pruned > 0 {
                    info!(pruned, "session store maintenance: pruned stale entries");
                }
            }
        });
    }

    println!("[startup] Loading and logging channel configurations...");
    if let Err(err) = bridges::log_all_configured_channels(&config.savfox_home).await {
        warn!(error = %err, "Failed to load channel configs for startup logging");
        println!("[startup] WARNING: Failed to load channel configs: {}", err);
    }
    println!("[startup] Channel configuration logging complete");
    
    // Initialize and start configured channels
    println!("[startup] Initializing channel instances...");
    let channel_registry = bridges::create_channel_registry();
    if let Err(err) = bridges::initialize_and_start_channels(&config.savfox_home, channel_registry.clone(), &bridge).await {
        warn!(error = %err, "Failed to initialize some channels");
        println!("[startup] WARNING: Some channels failed to initialize: {}", err);
    }
    println!("[startup] Channel initialization complete");

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

    // Also print to stderr for the operator.
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

    // Start the HTTP/WebSocket server (blocks until shutdown).
    server::start_server(
        &gateway_config,
        auth,
        session_mgr,
        bridge,
        session_store,
        cron_service,
        config.savfox_home.clone(),
    )
    .await
}
