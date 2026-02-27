use std::collections::HashMap;
use std::sync::Arc;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::RwLock;

type HmacSha256 = Hmac<Sha256>;

/// Permission scope for gateway tokens.
///
/// Supports both coarse-grained roles (Operator, Viewer, Chat) and
/// fine-grained scopes (OperatorRead, OperatorWrite, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// Full access: can start threads, send messages, and approve operations.
    Operator,
    /// Read-only: can subscribe to thread events but cannot start threads or approve.
    Viewer,
    /// Chat bridge scope: limited to bridge-initiated operations.
    Chat,

    // ── Fine-grained scopes ─────────────────────────────────────────
    /// Read access to operator data (sessions, config, logs, usage).
    OperatorRead,
    /// Write access (start threads, send messages, modify config).
    OperatorWrite,
    /// Administrative access (token management, plugin control).
    OperatorAdmin,
    /// Can manage execution approvals.
    OperatorApprovals,
    /// Can pair/manage devices and nodes.
    OperatorPairing,
}

impl TokenScope {
    /// Check if this scope implies another scope.
    /// `Operator` implies all `Operator*` sub-scopes.
    #[must_use]
    pub fn implies(self, other: Self) -> bool {
        match self {
            Self::Operator => matches!(
                other,
                Self::Operator
                    | Self::OperatorRead
                    | Self::OperatorWrite
                    | Self::OperatorAdmin
                    | Self::OperatorApprovals
                    | Self::OperatorPairing
                    | Self::Viewer
                    | Self::Chat
            ),
            Self::Viewer => matches!(other, Self::Viewer | Self::OperatorRead),
            _ => self == other,
        }
    }
}

/// Metadata about a gateway token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    /// Human-readable label for this token.
    pub label: String,
    /// Permission scopes granted.
    pub scopes: Vec<TokenScope>,
}

impl TokenInfo {
    /// Returns `true` if this token includes the given scope.
    ///
    /// Uses `TokenScope::implies()` so that `Operator` automatically
    /// grants access to `OperatorRead`, `OperatorWrite`, etc.
    #[must_use]
    pub fn has_scope(&self, scope: TokenScope) -> bool {
        self.scopes.iter().any(|s| s.implies(scope))
    }
}

/// Manages gateway bearer tokens and validates incoming requests.
#[derive(Debug, Clone)]
pub struct GatewayAuth {
    tokens: Arc<RwLock<HashMap<String, TokenInfo>>>,
}

impl GatewayAuth {
    /// Create a new auth manager with the given initial tokens.
    #[must_use]
    pub fn new(tokens: HashMap<String, TokenInfo>) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(tokens)),
        }
    }

    /// Create an auth manager with a single operator token.
    #[must_use]
    pub fn single_token(token: String) -> Self {
        let mut map = HashMap::new();
        map.insert(
            token,
            TokenInfo {
                label: "default".to_owned(),
                scopes: vec![TokenScope::Operator],
            },
        );
        Self::new(map)
    }

    /// Validate a bearer token and return its info if valid.
    pub async fn validate(&self, token: &str) -> Option<TokenInfo> {
        let tokens = self.tokens.read().await;
        tokens.get(token).cloned()
    }

    /// Add a new token.
    pub async fn add_token(&self, token: String, info: TokenInfo) {
        let mut tokens = self.tokens.write().await;
        tokens.insert(token, info);
    }

    /// Remove a token.
    pub async fn remove_token(&self, token: &str) -> bool {
        let mut tokens = self.tokens.write().await;
        tokens.remove(token).is_some()
    }

    /// Validate a challenge-response: the client sent `HMAC-SHA256(nonce, token)`
    /// as a hex string. We check all known tokens to find a match.
    pub async fn validate_challenge_response(
        &self,
        signature_hex: &str,
        nonce: &str,
    ) -> Option<TokenInfo> {
        let tokens = self.tokens.read().await;
        for (token, info) in tokens.iter() {
            // Compute HMAC-SHA256(nonce, key=token)
            let mut mac = match HmacSha256::new_from_slice(token.as_bytes()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            mac.update(nonce.as_bytes());
            let expected = hex::encode(mac.finalize().into_bytes());
            if expected == signature_hex {
                return Some(info.clone());
            }
        }
        None
    }

    /// Generate a random token string (32 hex bytes).
    #[must_use]
    pub fn generate_token() -> String {
        let bytes: [u8; 32] = rand::random();
        hex::encode(bytes)
    }

    // ── Password-based auth (#45) ──────────────────────────────────────

    /// Validate a username + password pair.
    ///
    /// Passwords are stored as SHA-256 hashes in a separate map keyed by
    /// username. On success, returns the token info associated with the user.
    pub async fn validate_password(&self, username: &str, password: &str) -> Option<TokenInfo> {
        let passwords = password_store().lock().await;
        if let Some(entry) = passwords.get(username) {
            // Hash the incoming password and compare
            use sha2::Digest;
            let hash = hex::encode(sha2::Sha256::digest(password.as_bytes()));
            if hash == entry.password_hash {
                return Some(entry.token_info.clone());
            }
        }
        None
    }

    /// Register a username with a password (stored as SHA-256 hash).
    pub async fn set_password(&self, username: &str, password: &str, info: TokenInfo) {
        use sha2::Digest;
        let hash = hex::encode(sha2::Sha256::digest(password.as_bytes()));
        let mut passwords = password_store().lock().await;
        passwords.insert(
            username.to_owned(),
            PasswordEntry {
                password_hash: hash,
                token_info: info,
            },
        );
    }

    /// Remove a password-based user.
    pub async fn remove_password(&self, username: &str) -> bool {
        let mut passwords = password_store().lock().await;
        passwords.remove(username).is_some()
    }

    /// List all password-based usernames.
    pub async fn list_password_users(&self) -> Vec<String> {
        let passwords = password_store().lock().await;
        passwords.keys().cloned().collect()
    }
}

/// Password entry for auth.
#[derive(Debug, Clone)]
struct PasswordEntry {
    password_hash: String,
    token_info: TokenInfo,
}

fn password_store() -> &'static tokio::sync::Mutex<HashMap<String, PasswordEntry>> {
    use std::sync::OnceLock;
    static STORE: OnceLock<tokio::sync::Mutex<HashMap<String, PasswordEntry>>> = OnceLock::new();
    STORE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

// ─── Method-level permission scopes ─────────────────────────────────────────

/// Fine-grained permission scope required to invoke a specific RPC method.
///
/// This maps to the underlying `TokenScope` variants but is expressed at the
/// method level so callers do not need to know the token-scope hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Administrative operations (config, gateway management).
    Admin,
    /// Read-only access to data (list / get / status / search).
    Read,
    /// Mutating operations (create / update / delete / patch / set).
    Write,
    /// Execution-approval management.
    Approvals,
    /// Device and node pairing.
    Pairing,
    /// Chat and message-sending operations.
    Chat,
}

impl Scope {
    /// Convert this method-level scope to the corresponding `TokenScope`.
    #[must_use]
    pub fn to_token_scope(self) -> TokenScope {
        match self {
            Self::Admin => TokenScope::OperatorAdmin,
            Self::Read => TokenScope::OperatorRead,
            Self::Write => TokenScope::OperatorWrite,
            Self::Approvals => TokenScope::OperatorApprovals,
            Self::Pairing => TokenScope::OperatorPairing,
            Self::Chat => TokenScope::Chat,
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin => write!(f, "admin"),
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Approvals => write!(f, "approvals"),
            Self::Pairing => write!(f, "pairing"),
            Self::Chat => write!(f, "chat"),
        }
    }
}

/// Determine the required permission scope for a given RPC method name.
///
/// The mapping follows these rules:
///
/// - `config.*`, `gateway.*` -> `Admin`
/// - `sessions.*`, `models.*`, `agents.*`, `memory.*`, `usage.*`, `cron.*`,
///   `logs.*`, `skills.*`, `tts.*`, `node.*` (non-pairing), `tools.*`, `wizard.*`
///   -> `Read` for list/get/status/search/layers/preview/tail/runs,
///      `Write` for create/update/delete/patch/set/compact/reset/promote/run
/// - `exec.approval*`, `exec.approvals*` -> `Approvals`
/// - `chat.*`, `send` -> `Chat`
/// - `channels.*` -> `Write`
/// - `directory.*` -> `Read`
/// - `node.pair.*`, `device.*` -> `Pairing`
/// - `security.audit`, `security.analyze` -> `Read`; `security.rotate` -> `Write`
/// - Everything else -> `Read`
#[must_use]
pub fn required_scope(method: &str) -> Scope {
    // ── Admin: config / gateway ──────────────────────────────────────
    if method.starts_with("config.") || method.starts_with("gateway.") {
        return Scope::Admin;
    }

    // ── Approvals ────────────────────────────────────────────────────
    if method.starts_with("exec.approval") {
        return Scope::Approvals;
    }

    // ── Security ─────────────────────────────────────────────────────
    if method == "security.audit" || method == "security.analyze" {
        return Scope::Read;
    }
    if method == "security.rotate" {
        return Scope::Write;
    }

    // ── Chat / send ──────────────────────────────────────────────────
    if method.starts_with("chat.") || method == "send" {
        return Scope::Chat;
    }

    // ── Channels (always Write) ──────────────────────────────────────
    if method.starts_with("channels.") {
        return Scope::Write;
    }

    // ── Pairing (must be checked before generic node.*) ──────────────
    if method.starts_with("node.pair.") || method.starts_with("device.") {
        return Scope::Pairing;
    }

    // ── CRUD domains: read vs write based on suffix ──────────────────
    if method.starts_with("sessions.")
        || method.starts_with("models.")
        || method.starts_with("agents.")
        || method.starts_with("memory.")
        || method.starts_with("usage.")
        || method.starts_with("cron.")
        || method.starts_with("logs.")
        || method.starts_with("skills.")
        || method.starts_with("tts.")
        || method.starts_with("stt.")
        || method.starts_with("node.")
        || method.starts_with("tools.")
        || method.starts_with("wizard.")
        || method.starts_with("webhooks.")
        || method.starts_with("dm.")
        || method.starts_with("providers.")
        || method.starts_with("routing.")
        || method.starts_with("plugins.")
        || method.starts_with("events.")
        || method.starts_with("typing.")
        || method.starts_with("canvas.")
        || method.starts_with("hooks.")
        || method.starts_with("reactions.")
        || method.starts_with("streaming.")
        || method.starts_with("heartbeat.")
        || method.starts_with("browser.")
        || method.starts_with("identity.")
        || method.starts_with("directory.")
    {
        return if is_read_suffix(method) {
            Scope::Read
        } else {
            Scope::Write
        };
    }

    // ── Agent operations ─────────────────────────────────────────────
    if method == "agent" || method.starts_with("agent.") {
        return Scope::Write;
    }

    // ── Misc mutating: wake, update.run, talk.mode, voicewake, heartbeats ──
    if method == "wake"
        || method == "update.run"
        || method.starts_with("talk.")
        || method.starts_with("voicewake.")
        || method == "set-heartbeats"
        || method == "system-event"
    {
        return Scope::Write;
    }

    // ── Defaults: connect, health, status, last-heartbeat, system-presence ──
    Scope::Read
}

/// Return `true` if the method suffix indicates a read-only (non-mutating) operation.
fn is_read_suffix(method: &str) -> bool {
    // Extract the final segment after the last dot.
    let suffix = method.rsplit('.').next().unwrap_or(method);
    matches!(
        suffix,
        "list"
            | "get"
            | "status"
            | "search"
            | "layers"
            | "preview"
            | "tail"
            | "runs"
            | "bins"
            | "providers"
            | "cost"
            | "describe"
            | "schema"
            | "identity"
            | "health"
            | "validate"
            | "transcribe"
            | "poll"
    )
}

/// Check whether a `TokenInfo` has the permission required by `scope`.
///
/// Role-based access rules:
/// - **Operator** role has all scopes (via `TokenScope::implies`).
/// - **Viewer** role has `Read` + `Chat`.
/// - **Chat** role has `Chat` only.
/// - Fine-grained `TokenScope` variants are checked directly.
#[must_use]
pub fn has_scope(token_info: &TokenInfo, scope: &Scope) -> bool {
    token_info.has_scope(scope.to_token_scope())
}

/// Helper to produce a hex-encoded string from bytes.
mod hex {
    #[must_use]
    pub(super) fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }
}
