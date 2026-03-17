use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use savfox_app_server_protocol::RequestId;
use savfox_core::config::{Config, ConfigService};
use savfox_core::config_loader::{CloudRequirementsLoader, LoaderOverrides};
use savfox_core::{AuthManager, SessionManager};
use savfox_feedback::SavfoxFeedback;
use savfox_login_oauth::ShutdownHandle;
use savfox_protocol::SessionId;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, oneshot};
use toml::Value as TomlValue;
use uuid::Uuid;

use crate::session::{GatewaySessionManager, SessionStore};

mod auth;
mod credential_manager;
mod router;
mod session_bridge;

// Re-export all public items so external `use crate::channel::X` continues to work.
pub(crate) use credential_manager::{
    is_slack_timestamp_fresh, verify_discord_signature, verify_slack_signature,
    verify_telegram_webhook_secret, verify_webhook_hmac,
};

const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INTERNAL_ERROR_CODE: i64 = -32603;
const METHOD_NOT_FOUND_ERROR_CODE: i64 = -32601;

/// Outgoing message from the channel to a WebSocket client.
#[derive(Debug, Clone)]
pub(crate) enum BridgeOutgoing {
    Response {
        id: RequestId,
        result: Value,
    },
    Error {
        id: RequestId,
        error: savfox_app_server_protocol::JSONRPCErrorError,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    ServerRequest(savfox_app_server_protocol::ServerRequest),
}

/// Handles bidirectional message routing between gateway WebSocket clients
/// and the Savfox core engine (SessionManager).
///
/// ## Lock ordering convention
///
/// When acquiring multiple locks, always follow this order to prevent deadlock:
///
/// 1. `runtime_channel_secrets` (RwLock)
/// 2. `pending_requests` (Mutex)
/// 3. `active_login` (Mutex)
/// 4. `logical_session_threads` (Mutex)
/// 5. `GatewaySessionManager::sessions` (RwLock) — accessed via `websocket_manager`
/// 6. `SessionStore::cache` (RwLock) — accessed via `session_store`
///
/// Never acquire a higher-numbered lock while holding a lower-numbered one.
pub(crate) struct GatewayChannel {
    auth_manager: Arc<AuthManager>,
    session_manager: Arc<SessionManager>,
    session_store: Arc<SessionStore>,
    config: Arc<Config>,
    cli_overrides: Vec<(String, TomlValue)>,
    cloud_requirements: CloudRequirementsLoader,
    config_service: ConfigService,
    feedback: SavfoxFeedback,
    savfox_linux_sandbox_exe: Option<PathBuf>,
    websocket_manager: GatewaySessionManager,
    /// Pending server->client requests awaiting a response.
    pending_requests: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Value>>>>,
    /// Outbound message channel.
    outgoing_tx: mpsc::Sender<BridgeOutgoing>,
    /// HTTP client for outbound platform API calls.
    http_client: reqwest::Client,
    /// Runtime channel credentials hot-reloaded from config patch/apply.
    runtime_channel_secrets: Arc<RwLock<RuntimeBridgeSecrets>>,
    /// Started channel instances keyed by saved channel config ID.
    channel_registry: crate::channels::ChannelRegistry,
    /// Active login attempt (browser OAuth or device code).
    active_login: Arc<Mutex<Option<ActiveLogin>>>,
    /// Logical gateway session IDs mapped to active core session IDs.
    logical_session_threads: Arc<Mutex<HashMap<String, SessionId>>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeBridgeSecrets {
    pub(crate) discord_bot_token: Option<String>,
    pub(crate) telegram_bot_token: Option<String>,
    pub(crate) slack_bot_token: Option<String>,
    pub(crate) slack_signing_secret: Option<String>,
    pub(crate) webhook_secret: Option<String>,
}

pub(crate) struct ResolvedMatrixClient {
    pub(crate) client: matrix_bot_sdk::client::MatrixClient,
    pub(crate) access_token: String,
    pub(crate) user_id: String,
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Tracks an active login attempt for cancellation.
struct ActiveLogin {
    shutdown_handle: ShutdownHandle,
    login_id: Uuid,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentInvocationResult {
    pub(crate) reply: String,
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
    pub(crate) rollout_path: Option<PathBuf>,
    pub(crate) last_token_usage: Option<savfox_protocol::protocol::TokenUsage>,
}

pub(crate) struct ResolvedAgentSession {
    pub(crate) session_id: SessionId,
    pub(crate) cleanup_after_turn: bool,
}

/// Arguments needed to construct a `GatewayChannel`.
pub(crate) struct GatewayBridgeArgs {
    pub(crate) config: Arc<Config>,
    pub(crate) session_store: Arc<SessionStore>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: SavfoxFeedback,
    pub(crate) savfox_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) websocket_manager: GatewaySessionManager,
    pub(crate) outgoing_tx: mpsc::Sender<BridgeOutgoing>,
    pub(crate) channel_registry: crate::channels::ChannelRegistry,
}

impl GatewayChannel {
    pub(crate) fn new(args: GatewayBridgeArgs) -> Self {
        let auth_manager = AuthManager::shared(
            args.config.savfox_home.clone(),
            false,
            args.config.cli_auth_credentials_store_mode,
        );

        let session_manager = Arc::new(SessionManager::new(
            args.config.savfox_home.clone(),
            auth_manager.clone(),
            savfox_protocol::protocol::SessionSource::VSCode, /* Gateway acts similarly to
                                                               * app-server */
        ));

        let config_service = ConfigService::new(
            args.config.savfox_home.clone(),
            args.cli_overrides.clone(),
            LoaderOverrides::default(),
            args.cloud_requirements.clone(),
        );

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            auth_manager,
            session_manager,
            session_store: args.session_store,
            config: args.config,
            cli_overrides: args.cli_overrides,
            cloud_requirements: args.cloud_requirements,
            config_service,
            feedback: args.feedback,
            savfox_linux_sandbox_exe: args.savfox_linux_sandbox_exe,
            websocket_manager: args.websocket_manager,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            outgoing_tx: args.outgoing_tx,
            http_client,
            runtime_channel_secrets: Arc::new(RwLock::new(RuntimeBridgeSecrets::default())),
            channel_registry: args.channel_registry,
            active_login: Arc::new(Mutex::new(None)),
            logical_session_threads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// List available model IDs for the OpenAI-compatible API.
    pub(crate) async fn list_models(&self) -> Vec<String> {
        self.session_manager
            .list_models(
                &self.config,
                savfox_core::models_manager::manager::RefreshStrategy::OnlineIfUncached,
            )
            .await
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    /// Get a reference to the thread manager.
    #[must_use]
    pub(crate) fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the WebSocket client manager.
    #[must_use]
    pub(crate) fn websocket_manager(&self) -> &GatewaySessionManager {
        &self.websocket_manager
    }

    /// Get a reference to the HTTP client for platform API calls.
    #[must_use]
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get a reference to the config.
    #[must_use]
    pub(crate) fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Replace runtime channel credentials from a hot config update.
    pub(crate) async fn set_runtime_channel_secrets(&self, secrets: RuntimeBridgeSecrets) {
        let mut lock = self.runtime_channel_secrets.write().await;
        *lock = secrets;
    }

    /// Snapshot current runtime channel credentials.
    #[must_use]
    pub(crate) async fn runtime_channel_secrets(&self) -> RuntimeBridgeSecrets {
        self.runtime_channel_secrets.read().await.clone()
    }

    #[must_use]
    pub(crate) fn channel_registry(&self) -> crate::channels::ChannelRegistry {
        Arc::clone(&self.channel_registry)
    }

    /// Get a receiver for thread-created events from the SessionManager.
    pub(crate) fn thread_created_receiver(&self) -> broadcast::Receiver<SessionId> {
        self.session_manager.subscribe_session_created()
    }
}

#[cfg(test)]
mod tests {
    use super::GatewayChannel;
    use super::credential_manager::{
        escape_telegram_html_text, is_slack_timestamp_fresh, verify_discord_signature,
        verify_slack_signature, verify_telegram_webhook_secret, verify_webhook_hmac,
    };

    #[test]
    fn slack_timestamp_freshness_validation() {
        assert!(is_slack_timestamp_fresh("1000", 300, 1200));
        assert!(!is_slack_timestamp_fresh("899", 300, 1200));
        assert!(!is_slack_timestamp_fresh("1201", 300, 1200));
        assert!(!is_slack_timestamp_fresh("not-a-number", 300, 1200));
    }

    #[test]
    fn feishu_receive_id_type_prefers_chat_and_open_id_prefixes() {
        assert_eq!(
            GatewayChannel::infer_feishu_receive_id_type("oc_123", "open_id"),
            "chat_id"
        );
        assert_eq!(
            GatewayChannel::infer_feishu_receive_id_type("ou_123", "chat_id"),
            "open_id"
        );
        assert_eq!(
            GatewayChannel::infer_feishu_receive_id_type("user-123", "user_id"),
            "user_id"
        );
    }

    #[test]
    fn slack_signature_roundtrip() {
        let secret = "top-secret";
        let timestamp = "1700000000";
        let body = br#"{"type":"event_callback","event":{"text":"/savfox hi"}}"#;

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac init");
        let base = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
        mac.update(base.as_bytes());
        let signature = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_slack_signature(secret, timestamp, &signature, body));
        assert!(!verify_slack_signature(
            secret,
            timestamp,
            "v0=deadbeef",
            body
        ));
    }

    #[test]
    fn webhook_hmac_roundtrip() {
        let secret = "webhook-secret";
        let body = br#"{"action":"start_thread","prompt":"hello"}"#;

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac init");
        mac.update(body);
        let digest = hex::encode(mac.finalize().into_bytes());

        assert!(verify_webhook_hmac(secret, &digest, body));
        assert!(verify_webhook_hmac(
            secret,
            &format!("sha256={digest}"),
            body
        ));
        assert!(!verify_webhook_hmac(secret, "sha256=bad", body));
        assert!(!verify_webhook_hmac(
            secret,
            &digest,
            br#"{"action":"different"}"#
        ));
    }

    #[test]
    fn discord_signature_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey};

        let secret = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&secret);
        let public_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
        let timestamp = "1700000000";
        let body = br#"{"type":2,"data":{"name":"savfox"}}"#;

        let mut msg = timestamp.as_bytes().to_vec();
        msg.extend_from_slice(body);
        let sig = signing_key.sign(&msg);
        let signature_hex = hex::encode(sig.to_bytes());

        assert!(verify_discord_signature(
            &public_key_hex,
            &signature_hex,
            timestamp,
            body
        ));
        assert!(!verify_discord_signature(
            &public_key_hex,
            "deadbeef",
            timestamp,
            body
        ));
    }

    #[test]
    fn telegram_secret_verification() {
        assert!(verify_telegram_webhook_secret("abc123", "abc123"));
        assert!(!verify_telegram_webhook_secret("abc123", "wrong"));
    }

    #[test]
    fn telegram_html_escape_handles_reserved_chars() {
        assert_eq!(
            escape_telegram_html_text("x < y && z > 0"),
            "x &lt; y &amp;&amp; z &gt; 0"
        );
    }
}
