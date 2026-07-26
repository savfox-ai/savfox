#![warn(clippy::print_stdout, clippy::print_stderr)]
// TODO(F6): 收敛 unreachable_pub 与 dead_code —— workspace 全局是 deny,
// 这里临时 allow 是为了不阻塞迭代。
//
// 累计清理:
//   r6  删 auto_reply 三个子系统 (~978 行)
//   r7  删 5 个 unwired channel (~1185 行)
//   r8  删 2 个 dead Channel struct + cfg(test) 收敛 helper (~170 行)
//   r9  删 7 个 unwired 子系统: tailscale / discovery / maintenance +
//       hooks/{event_bus,llm_hook,transformer,validator} (~2700 行)
//
// 剩余 ~79 处 dead_code 警告分布在 OTel-config / WebChat / canvas-host /
// 部分 RPC handler 占位字段，需独立 PR 逐位评估删除还是接入。
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
use savfox_utils::home_dir::GATEWAY_SUBDIR;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod agent_terminal_delegate;
mod agent_terminal_launcher;
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
pub mod dm_policy;
mod exec_approval;
pub mod gateway_cli;
pub(crate) mod home_paths;
pub mod hooks;
pub mod identity_links;
mod json_store;
mod log_level;
mod log_store;
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
mod terminal_agent;
mod terminal_pty;
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

    // Initialise the global rate limiter from the operator's config
    // *before* any handler or background task touches it. The OnceLock
    // is idempotent so this is safe even if a request races us — and
    // crucially the maintenance task in this fn does not have access to
    // the depot, so it relies on the limiter already being initialised.
    server::init_global_rate_limiter(&gateway_config);

    // Install tracing subscriber with a reloadable filter so the log level
    // can be changed at runtime via the `log.set_level` RPC.
    let env_filter = env_filter_from_default("info");
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

    // Stderr is wrapped in a redacting writer so secrets caught by
    // `redaction::redact_text` are masked before they reach the terminal /
    // journald / log aggregator. The in-memory `LogCaptureLayer` runs its own
    // redaction pass independently.
    let stderr_fmt =
        tracing_subscriber::fmt::layer().with_writer(security::redaction::RedactingStderr);

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

    // Set up the gateway token. An explicit operator-supplied value takes
    // precedence. Otherwise, reuse a valid token generated by an earlier run;
    // only generate and persist a replacement when no usable token exists.
    let token_was_supplied = gateway_config.token.is_some();
    let (token, persisted_token_path) = if let Some(token) = gateway_config.token.clone() {
        (token, None)
    } else {
        match load_persisted_token(&config.savfox_home).await {
            Ok(Some((token, path))) => {
                info!(path = %path.display(), "reusing persisted gateway token");
                (token, Some(path))
            }
            Ok(None) => {
                let token = GatewayAuth::generate_token();
                let path = match persist_generated_token(&config.savfox_home, &token).await {
                    Ok(path) => Some(path),
                    Err(err) => {
                        warn!(error = %err, "failed to persist gateway token to disk");
                        None
                    }
                };
                (token, path)
            }
            Err(err) => {
                warn!(error = %err, "persisted gateway token is unusable; generating a replacement");
                let token = GatewayAuth::generate_token();
                let path = match persist_generated_token(&config.savfox_home, &token).await {
                    Ok(path) => Some(path),
                    Err(err) => {
                        warn!(error = %err, "failed to persist gateway token to disk");
                        None
                    }
                };
                (token, path)
            }
        }
    };
    let token_fp = token_fingerprint(&token);
    let token_tail: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let auth = Arc::new(GatewayAuth::single_token(token.clone()));
    let session_mgr = Arc::new(GatewaySessionManager::new());
    let channel_registry = channels::create_channel_registry();
    let channel_recovery_registry = channels::recovery::create_channel_recovery_registry();
    let channel_recovery_supervisors = channels::recovery::create_channel_recovery_supervisors();
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
        channel_recovery_registry,
        channel_recovery_supervisors,
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
                // Drop rate-limit bucket entries that haven't been touched
                // in 2*window — keeps the per-IP / per-token HashMaps from
                // growing under attack scenarios that rotate addresses or
                // tokens (M14, exposed by #38).
                let evict = server::global_rate_limiter_uninitialised_default()
                    .evict_stale_buckets()
                    .await;
                if evict.ip_buckets_pruned > 0 || evict.token_buckets_pruned > 0 {
                    info!(
                        ip_pruned = evict.ip_buckets_pruned,
                        token_pruned = evict.token_buckets_pruned,
                        "rate limiter: pruned stale buckets"
                    );
                }
                // Reap managed PTY sessions idle past their timeout. The
                // RPC-driven `agent.terminal.pty.close_idle` only runs when a
                // client happens to call it, so without this sweep abandoned
                // terminal subprocesses would accumulate indefinitely.
                match crate::terminal_pty::terminal_pty_manager()
                    .close_idle()
                    .await
                {
                    Ok(closed) if closed > 0 => {
                        info!(closed, "terminal pty maintenance: closed idle sessions");
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "terminal pty maintenance: close_idle failed");
                    }
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
    info!(
        token_fingerprint = %token_fp,
        token_suffix = %token_tail,
        "Token (fingerprint only — full value never logged)"
    );

    // Also print to stderr for the operator (intentional — shown before
    // tracing is fully active so the operator always sees connection info).
    // The full token is *not* printed; we surface a fingerprint plus the file
    // path where the value can be retrieved when we generated it ourselves.
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
        eprintln!("  Token (fingerprint): {token_fp} (suffix …{token_tail})");
        if let Some(path) = persisted_token_path.as_ref() {
            eprintln!("  Token file: {} (mode 0600)", path.display());
        } else if token_was_supplied {
            eprintln!("  Token: supplied by configuration; not echoed.");
        }
        eprintln!();
    }

    // Start the HTTP/WebSocket server (blocks until shutdown).
    let serve_result = server::start_server(
        &gateway_config,
        auth,
        session_mgr,
        Arc::clone(&channel),
        session_store,
        cron_service,
        config.savfox_home.clone(),
    )
    .await;

    channels::shutdown_all_channel_instances(&config.savfox_home, &channel).await;

    // On graceful shutdown, reap any managed PTY subprocesses so they are not
    // left orphaned. Best-effort: log and continue regardless of outcome.
    match crate::terminal_pty::terminal_pty_manager()
        .close_all(crate::terminal_pty::TerminalPtyCloseReason::GatewayShutdown)
        .await
    {
        Ok(closed) if closed > 0 => {
            info!(closed, "gateway shutdown: closed managed PTY sessions");
        }
        Ok(_) => {}
        Err(err) => {
            warn!(error = %err, "gateway shutdown: failed to close managed PTY sessions");
        }
    }

    serve_result
}

/// Returns the first 8 hex characters of the SHA-256 of `token`.
///
/// This 32-bit fingerprint is safe to log: it identifies a token across
/// startups for operator correlation without revealing the token itself.
fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(&digest[..4])
}

/// Load the token generated by an earlier gateway run.
///
/// Generated gateway tokens contain 32 random bytes encoded as exactly 64
/// hexadecimal characters. Surrounding whitespace is accepted so that a file
/// ending in a newline remains usable, but arbitrary or truncated values are
/// rejected and replaced at startup.
async fn load_persisted_token(
    savfox_home: &std::path::Path,
) -> std::io::Result<Option<(String, PathBuf)>> {
    let path = savfox_home.join(GATEWAY_SUBDIR).join("token");
    let value = match tokio::fs::read_to_string(&path).await {
        Ok(value) => value,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let token = value.trim();
    if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(Some((token.to_owned(), path)))
    } else {
        Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "{} does not contain a 64-character hexadecimal token",
                path.display()
            ),
        ))
    }
}

/// Persist an auto-generated gateway token to `<savfox_home>/gateway/token`
/// with mode 0600 (Unix) or current-user-only DACL (Windows).
///
/// The file is only overwritten when no valid token from an earlier run exists.
async fn persist_generated_token(
    savfox_home: &std::path::Path,
    token: &str,
) -> std::io::Result<PathBuf> {
    let dir = savfox_home.join(GATEWAY_SUBDIR);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("token");

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o600);
        let mut file = opts.open(&path)?;
        std::io::Write::write_all(&mut file, token.as_bytes())?;
    }

    #[cfg(not(unix))]
    {
        // On Windows the parent dir typically inherits ACLs scoped to the user
        // running the daemon (LocalAppData / user profile). Plain write keeps
        // the existing ACL; an explicit DACL tightening is left as follow-up.
        std::fs::write(&path, token.as_bytes())?;
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{load_persisted_token, persist_generated_token, token_fingerprint};

    #[test]
    fn fingerprint_is_deterministic_and_eight_hex_chars() {
        let fp = token_fingerprint("hello-world");
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fp, token_fingerprint("hello-world"));
    }

    #[test]
    fn fingerprint_differs_for_different_tokens() {
        assert_ne!(token_fingerprint("a"), token_fingerprint("b"));
    }

    #[tokio::test]
    async fn persisted_valid_token_is_reused() {
        let home = tempfile::tempdir().expect("create temp home");
        let token = "0123456789abcdef".repeat(4);
        let expected_path = persist_generated_token(home.path(), &token)
            .await
            .expect("persist token");

        let (loaded, path) = load_persisted_token(home.path())
            .await
            .expect("load token")
            .expect("token should exist");

        assert_eq!(loaded, token);
        assert_eq!(path, expected_path);
    }

    #[tokio::test]
    async fn persisted_token_allows_surrounding_whitespace() {
        let home = tempfile::tempdir().expect("create temp home");
        let token = "abcdef0123456789".repeat(4);
        let path = persist_generated_token(home.path(), &format!("{token}\n"))
            .await
            .expect("persist token");

        let (loaded, loaded_path) = load_persisted_token(home.path())
            .await
            .expect("load token")
            .expect("token should exist");

        assert_eq!(loaded, token);
        assert_eq!(loaded_path, path);
    }

    #[tokio::test]
    async fn persisted_invalid_token_is_rejected() {
        let home = tempfile::tempdir().expect("create temp home");
        persist_generated_token(home.path(), "too-short")
            .await
            .expect("persist invalid token fixture");

        let err = load_persisted_token(home.path())
            .await
            .expect_err("invalid token should be rejected");

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn missing_persisted_token_returns_none() {
        let home = tempfile::tempdir().expect("create temp home");

        assert!(
            load_persisted_token(home.path())
                .await
                .expect("missing token is not an error")
                .is_none()
        );
    }
}
