use std::collections::HashMap;
use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
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
    /// Chat channel scope: limited to channel-initiated operations.
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
            // Constant-time comparison to avoid leaking the valid HMAC via a
            // timing side-channel on the byte-by-byte `String ==` path.
            if expected.as_bytes().ct_eq(signature_hex.as_bytes()).into() {
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
    /// Passwords are stored as bcrypt hashes (cost 12) in a separate map
    /// keyed by username. On success, returns the token info associated
    /// with the user. Also supports verifying legacy SHA-256 hashes for
    /// backward compatibility (auto-upgrades to bcrypt on successful login).
    pub async fn validate_password(&self, username: &str, password: &str) -> Option<TokenInfo> {
        let passwords = password_store().lock().await;
        if let Some(entry) = passwords.get(username) {
            // Try bcrypt first (new-style hash starts with "$2b$").
            if entry.password_hash.starts_with("$2b$") || entry.password_hash.starts_with("$2a$") {
                if bcrypt::verify(password, &entry.password_hash).unwrap_or(false) {
                    return Some(entry.token_info.clone());
                }
            } else {
                // Legacy SHA-256 path — verify then auto-upgrade to bcrypt.
                use sha2::Digest;
                let sha_hash = hex::encode(sha2::Sha256::digest(password.as_bytes()));
                if sha_hash
                    .as_bytes()
                    .ct_eq(entry.password_hash.as_bytes())
                    .into()
                {
                    let info = entry.token_info.clone();
                    drop(passwords);
                    // Auto-upgrade to bcrypt (best-effort; login still succeeds
                    // on the verified legacy hash even if the upgrade fails).
                    if let Err(err) = self.set_password(username, password, info.clone()).await {
                        tracing::warn!("failed to upgrade legacy password hash to bcrypt: {err}");
                    }
                    return Some(info);
                }
            }
        }
        None
    }

    /// Register a username with a password (stored as bcrypt hash, cost 12).
    ///
    /// Returns `Err` if hashing fails. We deliberately do **not** fall back to
    /// an unsalted SHA-256 hash on failure: storing a weaker hash silently
    /// would degrade every affected credential to a brute-forceable form. A
    /// hashing failure must fail closed (no password set) rather than open.
    pub async fn set_password(
        &self,
        username: &str,
        password: &str,
        info: TokenInfo,
    ) -> Result<(), bcrypt::BcryptError> {
        let hash = bcrypt::hash(password, 12)?;
        let mut passwords = password_store().lock().await;
        passwords.insert(
            username.to_owned(),
            PasswordEntry {
                password_hash: hash,
                token_info: info,
            },
        );
        Ok(())
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
/// - `sessions.*`, `models.*`, `agents.*`, `memory.*`, `usage.*`, `cron.*`, `logs.*`, `skills.*`,
///   `tts.*`, `node.*` (non-pairing), `tools.*`, `wizard.*` -> `Read` for
///   list/get/status/search/layers/preview/tail/runs, `Write` for
///   create/update/delete/patch/set/compact/reset/promote/run
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

    // ── Hooks (privileged: writes register shell handlers) ───────────
    // Creating, updating, enabling, or disabling a hook lets the caller
    // register a `Shell { command }` handler that the gateway will run on
    // the next matching event. Treat the write surface as Admin-only so
    // an OperatorWrite token cannot turn itself into RCE.
    if method == "hooks.create"
        || method == "hooks.update"
        || method == "hooks.delete"
        || method == "hooks.enable"
        || method == "hooks.disable"
    {
        return Scope::Admin;
    }

    // ── Account / OAuth login (privileged) ──────────────────────────
    // S6: account/login/start saves API keys / spawns OAuth servers, and
    // account/login/cancel tears those servers down. Both touch
    // credential state and must require Admin. account/read is a pure
    // read of the saved state so it stays at Read.
    if method == "account/login/start" || method == "account/login/cancel" {
        return Scope::Admin;
    }
    if method == "account/read" {
        return Scope::Read;
    }

    // ── Agent terminal local filesystem/process operations (privileged) ─
    // S6: agent.terminal.launch shells out to a system terminal. Even
    // though it lives in the agent.* namespace (which would otherwise
    // resolve to Write), the operation is Admin-equivalent. Cleanup deletes
    // local terminal session directories, so it follows the same boundary.
    if matches!(
        method,
        "agent.terminal.launch"
            | "agent.terminal.cleanup"
            | "agent.terminal.pty.start"
            | "agent.terminal.pty.write"
            | "agent.terminal.pty.resize"
            | "agent.terminal.pty.close"
            | "agent.terminal.pty.close_idle"
    ) {
        return Scope::Admin;
    }

    // ── Security ─────────────────────────────────────────────────────
    if method == "security.audit" || method == "security.analyze" {
        return Scope::Read;
    }
    if method == "security.rotate" {
        // Rotates the gateway bearer token + webhook signing secrets and rewrites
        // the config file — credential-touching, so require Admin like config.*.
        return Scope::Admin;
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
    // Bare `agent` is a write op (it spawns / drives a subagent). Sub-methods
    // honor the `is_read_suffix` convention so e.g. `agent.delegation.list`
    // resolves to Read.
    if method == "agent" {
        return Scope::Write;
    }
    if method.starts_with("agent.") {
        return if is_read_suffix(method) {
            Scope::Read
        } else {
            Scope::Write
        };
    }

    // ── Misc mutating: wake, update.run, talk.mode, voicewake, heartbeats ──
    if method == "wake"
        || method == "update.run"
        || method.starts_with("talk.")
        || method.starts_with("voicewake.")
        || matches!(
            method,
            "system.heartbeats.set" | "set-heartbeats" | "system.event" | "system-event"
        )
    {
        return Scope::Write;
    }

    // ── Explicit Read allowlist (S6) ────────────────────────────────
    // The previous catch-all was `Scope::Read` for any unmapped method
    // which silently authorised anything new the dispatcher learned to
    // handle without an accompanying scope decision. The closed set
    // below captures every `Scope::Read` method the existing dispatcher
    // intentionally permits — anything not listed falls into the
    // strict deny rule below.
    if matches!(
        method,
        "connect"
            | "health"
            | "status"
            | "system.heartbeat"
            | "last-heartbeat"
            | "system.presence"
            | "system-presence"
    ) {
        return Scope::Read;
    }

    // ── Strict deny default (S6) ───────────────────────────────────
    // Anything we did not explicitly classify is treated as Admin so
    // an unauthenticated bug-induced fallthrough does not silently
    // grant Read or Write access. Adding a new RPC method now
    // requires a deliberate `required_scope` mapping or it will be
    // rejected with PERMISSION_DENIED for non-Admin tokens.
    Scope::Admin
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
            | "read"
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
            | "metrics"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn token_info(scopes: Vec<TokenScope>) -> TokenInfo {
        TokenInfo {
            label: "test".to_owned(),
            scopes,
        }
    }

    // ── TokenScope::implies ────────────────────────────────────────────

    #[test]
    fn operator_implies_every_subscope() {
        let op = TokenScope::Operator;
        for s in [
            TokenScope::Operator,
            TokenScope::OperatorRead,
            TokenScope::OperatorWrite,
            TokenScope::OperatorAdmin,
            TokenScope::OperatorApprovals,
            TokenScope::OperatorPairing,
            TokenScope::Viewer,
            TokenScope::Chat,
        ] {
            assert!(op.implies(s), "Operator should imply {s:?}");
        }
    }

    #[test]
    fn viewer_implies_read_and_self_only() {
        let v = TokenScope::Viewer;
        assert!(v.implies(TokenScope::Viewer));
        assert!(v.implies(TokenScope::OperatorRead));
        assert!(!v.implies(TokenScope::OperatorWrite));
        assert!(!v.implies(TokenScope::OperatorAdmin));
        assert!(!v.implies(TokenScope::Chat));
    }

    #[test]
    fn chat_implies_only_self() {
        let c = TokenScope::Chat;
        assert!(c.implies(TokenScope::Chat));
        assert!(!c.implies(TokenScope::OperatorRead));
        assert!(!c.implies(TokenScope::OperatorWrite));
    }

    #[test]
    fn fine_grained_scopes_only_imply_themselves() {
        for s in [
            TokenScope::OperatorRead,
            TokenScope::OperatorWrite,
            TokenScope::OperatorAdmin,
            TokenScope::OperatorApprovals,
            TokenScope::OperatorPairing,
        ] {
            assert!(s.implies(s));
            // No fine-grained scope should imply Operator (the parent role).
            assert!(
                !s.implies(TokenScope::Operator),
                "{s:?} should not imply Operator"
            );
        }
    }

    // ── TokenInfo::has_scope / has_scope() free fn ────────────────────

    #[test]
    fn has_scope_succeeds_when_token_carries_role() {
        let info = token_info(vec![TokenScope::Operator]);
        assert!(has_scope(&info, &Scope::Read));
        assert!(has_scope(&info, &Scope::Write));
        assert!(has_scope(&info, &Scope::Admin));
        assert!(has_scope(&info, &Scope::Approvals));
        assert!(has_scope(&info, &Scope::Pairing));
        assert!(has_scope(&info, &Scope::Chat));
    }

    #[test]
    fn viewer_token_cannot_satisfy_write() {
        let info = token_info(vec![TokenScope::Viewer]);
        assert!(has_scope(&info, &Scope::Read));
        assert!(!has_scope(&info, &Scope::Write));
        assert!(!has_scope(&info, &Scope::Admin));
        assert!(!has_scope(&info, &Scope::Approvals));
    }

    #[test]
    fn fine_grained_token_only_satisfies_own_scope() {
        let info = token_info(vec![TokenScope::OperatorWrite]);
        assert!(has_scope(&info, &Scope::Write));
        assert!(!has_scope(&info, &Scope::Read));
        assert!(!has_scope(&info, &Scope::Admin));
    }

    // ── required_scope mapping ────────────────────────────────────────

    #[test]
    fn config_methods_require_admin() {
        assert_eq!(required_scope("config.set"), Scope::Admin);
        assert_eq!(required_scope("config.patch"), Scope::Admin);
        assert_eq!(required_scope("gateway.restart"), Scope::Admin);
    }

    #[test]
    fn approval_methods_require_approvals_scope() {
        assert_eq!(required_scope("exec.approval.resolve"), Scope::Approvals);
        assert_eq!(required_scope("exec.approval.list"), Scope::Approvals);
    }

    #[test]
    fn hooks_writes_require_admin() {
        // S9: hooks register shell handlers. Write would let an Operator
        // turn itself into RCE; Admin is required.
        for method in [
            "hooks.create",
            "hooks.update",
            "hooks.delete",
            "hooks.enable",
            "hooks.disable",
        ] {
            assert_eq!(required_scope(method), Scope::Admin, "{method}");
        }
        // Read paths still resolve to Read.
        assert_eq!(required_scope("hooks.list"), Scope::Read);
        assert_eq!(required_scope("hooks.get"), Scope::Read);
    }

    #[test]
    fn account_login_writes_require_admin() {
        // S6: account/login/start saves credentials and spawns OAuth servers;
        // account/login/cancel tears them down. Both touch credential state
        // and must be Admin. The pure-read account/read stays at Read.
        assert_eq!(required_scope("account/login/start"), Scope::Admin);
        assert_eq!(required_scope("account/login/cancel"), Scope::Admin);
        assert_eq!(required_scope("account/read"), Scope::Read);
    }

    #[test]
    fn agent_terminal_launch_requires_admin() {
        // S6: spawning a system terminal is Admin-equivalent regardless
        // of the surrounding agent.* namespace.
        assert_eq!(required_scope("agent.terminal.launch"), Scope::Admin);
        assert_eq!(required_scope("agent.terminal.cleanup"), Scope::Admin);
        assert_eq!(required_scope("agent.terminal.pty.start"), Scope::Admin);
        assert_eq!(required_scope("agent.terminal.pty.write"), Scope::Admin);
        assert_eq!(required_scope("agent.terminal.pty.resize"), Scope::Admin);
        assert_eq!(required_scope("agent.terminal.pty.close"), Scope::Admin);
        assert_eq!(
            required_scope("agent.terminal.pty.close_idle"),
            Scope::Admin
        );
        assert_eq!(required_scope("agent.terminal.pty.read"), Scope::Read);
        assert_eq!(required_scope("agent.terminal.pty.list"), Scope::Read);
        assert_eq!(required_scope("agent.terminal.metrics"), Scope::Read);
        // Other agent.* methods stay at Write.
        assert_eq!(required_scope("agent.list"), Scope::Read);
        assert_eq!(required_scope("agent"), Scope::Write);
    }

    #[test]
    fn pairing_methods_require_pairing_scope() {
        assert_eq!(required_scope("node.pair.start"), Scope::Pairing);
        assert_eq!(required_scope("device.create"), Scope::Pairing);
        assert_eq!(required_scope("device.list"), Scope::Pairing);
    }

    #[test]
    fn chat_send_requires_chat_scope() {
        assert_eq!(required_scope("chat.send"), Scope::Chat);
        assert_eq!(required_scope("send"), Scope::Chat);
    }

    #[test]
    fn channels_methods_require_write() {
        assert_eq!(required_scope("channels.list"), Scope::Write);
        assert_eq!(required_scope("channels.create"), Scope::Write);
        assert_eq!(
            required_scope("channels.arkret.runtime_key_request"),
            Scope::Write
        );
        assert_eq!(
            required_scope("channels.arkret.generate_runtime_key_ref"),
            Scope::Write
        );
    }

    #[test]
    fn read_suffix_resolves_to_read_for_crud_domains() {
        for method in [
            "sessions.list",
            "sessions.get",
            "models.status",
            "agents.search",
            "memory.preview",
        ] {
            assert_eq!(
                required_scope(method),
                Scope::Read,
                "{method} should be Read"
            );
        }
    }

    #[test]
    fn write_suffix_resolves_to_write_for_crud_domains() {
        for method in [
            "sessions.create",
            "sessions.update",
            "sessions.delete",
            "memory.set",
            "agents.compact",
            "agents.reset",
        ] {
            assert_eq!(
                required_scope(method),
                Scope::Write,
                "{method} should be Write"
            );
        }
    }

    #[test]
    fn agent_invocation_requires_write() {
        assert_eq!(required_scope("agent"), Scope::Write);
        // `agent.terminal.launch` is intentionally Admin (added in #34
        // / `agent_terminal_launch_requires_admin`); other agent.* calls
        // resolve to Write.
        assert_eq!(required_scope("agent.delegation.list"), Scope::Read);
        assert_eq!(required_scope("agent.delegation.record"), Scope::Write);
    }

    #[test]
    fn explicit_read_allowlist_methods_resolve_to_read() {
        // S6: every connect-/status-style method that today is permitted
        // for any caller must appear in the explicit allowlist so adding
        // a brand-new RPC method cannot accidentally inherit Read access.
        for method in [
            "connect",
            "health",
            "status",
            "system.heartbeat",
            "last-heartbeat",
            "system.presence",
            "system-presence",
        ] {
            assert_eq!(required_scope(method), Scope::Read, "{method}");
        }
    }

    #[test]
    fn unknown_method_now_requires_admin() {
        // S6: previously the catch-all default was `Scope::Read` which
        // silently authorised any unmapped method. The closed allowlist
        // above (`explicit_read_allowlist_methods_resolve_to_read`)
        // captures every legitimately-public method; everything else
        // requires Admin so a new RPC handler cannot land without an
        // accompanying scope decision.
        assert_eq!(required_scope("totally.made.up"), Scope::Admin);
        assert_eq!(required_scope("not-a-real-method"), Scope::Admin);
    }

    // ── GatewayAuth::validate (single-token happy + sad path) ─────────

    #[tokio::test]
    async fn validate_returns_info_for_known_token_and_none_otherwise() {
        let auth = GatewayAuth::single_token("the-token".to_owned());
        let info = auth.validate("the-token").await.expect("known token");
        assert!(info.has_scope(TokenScope::Operator));
        assert!(auth.validate("wrong-token").await.is_none());
    }

    #[tokio::test]
    async fn add_then_remove_token_round_trip() {
        let auth = GatewayAuth::new(HashMap::new());
        let info = TokenInfo {
            label: "viewer".to_owned(),
            scopes: vec![TokenScope::Viewer],
        };
        auth.add_token("vt".to_owned(), info.clone()).await;
        assert_eq!(
            auth.validate("vt").await.expect("known token").scopes,
            info.scopes
        );
        assert!(auth.remove_token("vt").await);
        assert!(auth.validate("vt").await.is_none());
        // Removing again returns false.
        assert!(!auth.remove_token("vt").await);
    }

    // ── HMAC challenge-response ───────────────────────────────────────

    fn hmac_signature(token: &str, nonce: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(token.as_bytes()).unwrap();
        mac.update(nonce.as_bytes());
        super::hex::encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn challenge_response_accepts_correct_signature() {
        let auth = GatewayAuth::single_token("hmac-token".to_owned());
        let nonce = "abc123";
        let sig = hmac_signature("hmac-token", nonce);
        let info = auth.validate_challenge_response(&sig, nonce).await;
        assert!(info.is_some(), "matching HMAC must yield TokenInfo");
    }

    #[tokio::test]
    async fn challenge_response_rejects_wrong_nonce() {
        let auth = GatewayAuth::single_token("hmac-token".to_owned());
        let sig = hmac_signature("hmac-token", "real-nonce");
        // Verifier passes a different nonce — must fail.
        assert!(
            auth.validate_challenge_response(&sig, "tampered-nonce")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn challenge_response_rejects_unknown_token_signature() {
        let auth = GatewayAuth::single_token("real-token".to_owned());
        // Compute HMAC under a token the server does not know.
        let sig = hmac_signature("attacker-token", "abc");
        assert!(
            auth.validate_challenge_response(&sig, "abc")
                .await
                .is_none()
        );
    }
}
