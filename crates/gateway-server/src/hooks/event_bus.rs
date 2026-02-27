use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, error, info, warn};

// ─── Hook events ────────────────────────────────────────────────────────────

/// All gateway lifecycle events that can trigger hooks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookEvent {
    // Session lifecycle
    SessionStart,
    SessionEnd,
    SessionCompact,

    // Message lifecycle
    MessageReceived,
    MessageSent,
    MessageError,

    // Agent lifecycle
    AgentStart,
    AgentComplete,
    AgentError,

    // Cron lifecycle
    CronStarted,
    CronCompleted,
    CronFailed,

    // Memory lifecycle
    MemoryCreated,
    MemoryUpdated,
    MemoryDeleted,

    // Config lifecycle
    ConfigChanged,
    ConfigReloaded,
}

impl HookEvent {
    /// Return the colon-delimited category string for pattern matching.
    ///
    /// For example, `SessionStart` becomes `"session:start"` and
    /// `MemoryCreated` becomes `"memory:created"`.
    pub(crate) fn as_pattern_str(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session:start",
            HookEvent::SessionEnd => "session:end",
            HookEvent::SessionCompact => "session:compact",

            HookEvent::MessageReceived => "message:received",
            HookEvent::MessageSent => "message:sent",
            HookEvent::MessageError => "message:error",

            HookEvent::AgentStart => "agent:start",
            HookEvent::AgentComplete => "agent:complete",
            HookEvent::AgentError => "agent:error",

            HookEvent::CronStarted => "cron:started",
            HookEvent::CronCompleted => "cron:completed",
            HookEvent::CronFailed => "cron:failed",

            HookEvent::MemoryCreated => "memory:created",
            HookEvent::MemoryUpdated => "memory:updated",
            HookEvent::MemoryDeleted => "memory:deleted",

            HookEvent::ConfigChanged => "config:changed",
            HookEvent::ConfigReloaded => "config:reloaded",
        }
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_pattern_str())
    }
}

// ─── Hook handlers ──────────────────────────────────────────────────────────

/// The action a hook registration executes when a matching event fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum HookHandler {
    /// Execute a shell command.  The event payload is passed as the
    /// `HOOK_PAYLOAD` environment variable (JSON-encoded).
    Shell {
        command: String,
        #[serde(default = "default_timeout_secs")]
        timeout_secs: u32,
    },
    /// Execute a script file.  The event payload is passed as the first
    /// positional argument (JSON-encoded).
    Script { path: PathBuf },
    /// POST the event payload as JSON to a remote URL.
    Webhook {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

fn default_timeout_secs() -> u32 {
    30
}

// ─── Registration ───────────────────────────────────────────────────────────

/// A single hook subscription that pairs an event pattern with a handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HookRegistration {
    /// Unique identifier for this registration.
    pub id: String,
    /// Glob-style event pattern.
    ///
    /// Supports:
    /// - Exact match: `"session:start"`
    /// - Wildcard suffix: `"session:*"` (matches all session events)
    /// - Match-all: `"*"` (matches every event)
    pub event_pattern: String,
    /// The handler to execute when the pattern matches.
    pub handler: HookHandler,
    /// Whether this registration is active.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

fn enabled_default() -> bool {
    true
}

// ─── Pattern matching ───────────────────────────────────────────────────────

/// Check whether `event_str` matches the given glob-style `pattern`.
///
/// Rules:
/// - `"*"` matches everything.
/// - A pattern ending in `:*` (e.g. `"session:*"`) matches any event string
///   that starts with the prefix before `:*`.
/// - Otherwise, an exact (case-sensitive) match is required.
fn pattern_matches(pattern: &str, event_str: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(":*") {
        return event_str.starts_with(prefix)
            && event_str
                .as_bytes()
                .get(prefix.len())
                .is_some_and(|&b| b == b':');
    }
    pattern == event_str
}

// ─── Event bus ──────────────────────────────────────────────────────────────

/// Central event bus for the hooks system.
///
/// Hook registrations subscribe to event patterns.  When an event is emitted,
/// all matching (and enabled) handlers are executed concurrently.
#[derive(Debug, Default)]
pub(crate) struct EventBus {
    subscriptions: Vec<HookRegistration>,
}

impl EventBus {
    /// Create an empty `EventBus`.
    pub(crate) fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Register a new hook subscription.
    pub(crate) fn subscribe(&mut self, reg: HookRegistration) {
        info!(
            id = %reg.id,
            pattern = %reg.event_pattern,
            enabled = reg.enabled,
            "event bus: hook registered"
        );
        self.subscriptions.push(reg);
    }

    /// Remove a hook subscription by ID.  Returns `true` if the registration
    /// existed and was removed.
    pub(crate) fn unsubscribe(&mut self, id: &str) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|r| r.id != id);
        let removed = self.subscriptions.len() < before;
        if removed {
            info!(id = %id, "event bus: hook unregistered");
        }
        removed
    }

    /// Return a slice of all current registrations.
    pub(crate) fn list(&self) -> &[HookRegistration] {
        &self.subscriptions
    }

    /// Emit an event, running all matching enabled handlers concurrently.
    ///
    /// Handler errors are logged but do not propagate  - the bus provides
    /// best-effort delivery.
    pub(crate) async fn emit(&self, event: HookEvent, data: Value) {
        let event_str = event.as_pattern_str();
        let matching: Vec<&HookRegistration> = self
            .subscriptions
            .iter()
            .filter(|r| r.enabled && pattern_matches(&r.event_pattern, event_str))
            .collect();

        if matching.is_empty() {
            debug!(event = %event_str, "event bus: no matching handlers");
            return;
        }

        info!(
            event = %event_str,
            handler_count = matching.len(),
            "event bus: dispatching event"
        );

        let mut handles = Vec::with_capacity(matching.len());

        for reg in matching {
            let handler = reg.handler.clone();
            let id = reg.id.clone();
            let event_str_owned = event_str.to_owned();
            let data_clone = data.clone();

            handles.push(tokio::spawn(async move {
                execute_handler(&id, &event_str_owned, &handler, &data_clone).await;
            }));
        }

        // Await all handlers.  Individual failures are logged inside
        // `execute_handler` so we only need to catch panics here.
        for handle in handles {
            if let Err(err) = handle.await {
                error!("event bus: handler task panicked: {err}");
            }
        }
    }
}

// ─── Handler execution ──────────────────────────────────────────────────────

/// Execute a single hook handler.
async fn execute_handler(id: &str, event: &str, handler: &HookHandler, data: &Value) {
    match handler {
        HookHandler::Shell {
            command,
            timeout_secs,
        } => {
            execute_shell(id, event, command, *timeout_secs, data).await;
        }
        HookHandler::Script { path } => {
            execute_script(id, event, path, data).await;
        }
        HookHandler::Webhook { url, headers } => {
            execute_webhook(id, event, url, headers, data).await;
        }
    }
}

/// Run a shell command with the event payload in `HOOK_PAYLOAD` and
/// `HOOK_EVENT` environment variables.
async fn execute_shell(id: &str, event: &str, command: &str, timeout_secs: u32, data: &Value) {
    let payload = data.to_string();
    let timeout = Duration::from_secs(u64::from(timeout_secs));

    debug!(id = %id, event = %event, command = %command, "event bus: executing shell handler");

    #[cfg(target_os = "windows")]
    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new("cmd")
            .args(["/C", command])
            .env("HOOK_PAYLOAD", &payload)
            .env("HOOK_EVENT", event)
            .output(),
    )
    .await;

    #[cfg(not(target_os = "windows"))]
    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new("sh")
            .args(["-c", command])
            .env("HOOK_PAYLOAD", &payload)
            .env("HOOK_EVENT", event)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                debug!(id = %id, "event bus: shell handler completed successfully");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    id = %id,
                    exit_code = ?output.status.code(),
                    stderr = %stderr,
                    "event bus: shell handler exited with error"
                );
            }
        }
        Ok(Err(err)) => {
            error!(id = %id, error = %err, "event bus: shell handler failed to spawn");
        }
        Err(_) => {
            warn!(
                id = %id,
                timeout_secs = timeout_secs,
                "event bus: shell handler timed out"
            );
        }
    }
}

/// Run a script file with the event payload as the first argument.
async fn execute_script(id: &str, event: &str, path: &PathBuf, data: &Value) {
    let payload = data.to_string();

    debug!(id = %id, event = %event, path = %path.display(), "event bus: executing script handler");

    let result = tokio::process::Command::new(path)
        .arg(&payload)
        .env("HOOK_EVENT", event)
        .output()
        .await;

    match result {
        Ok(output) => {
            if output.status.success() {
                debug!(id = %id, "event bus: script handler completed successfully");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    id = %id,
                    exit_code = ?output.status.code(),
                    stderr = %stderr,
                    "event bus: script handler exited with error"
                );
            }
        }
        Err(err) => {
            error!(id = %id, error = %err, "event bus: script handler failed to spawn");
        }
    }
}

/// POST the event payload to a webhook URL.
async fn execute_webhook(
    id: &str,
    event: &str,
    url: &str,
    headers: &HashMap<String, String>,
    data: &Value,
) {
    debug!(id = %id, event = %event, url = %url, "event bus: executing webhook handler");

    let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
    if let Err(err) = crate::ssrf::validate_outbound_url(url, &ssrf_cfg).await {
        warn!(
            id = %id,
            event = %event,
            url = %url,
            error = %err,
            "event bus: blocked outbound webhook by ssrf policy"
        );
        return;
    }

    let client = match crate::ssrf::build_guarded_client(&ssrf_cfg) {
        Ok(client) => client,
        Err(err) => {
            error!(
                id = %id,
                event = %event,
                error = %err,
                "event bus: failed to create guarded http client"
            );
            return;
        }
    };
    let mut builder = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Hook-Event", event)
        .json(data);

    for (key, value) in headers {
        builder = builder.header(key.as_str(), value.as_str());
    }

    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                debug!(id = %id, status = %status, "event bus: webhook handler succeeded");
            } else {
                warn!(
                    id = %id,
                    status = %status,
                    "event bus: webhook handler received non-success status"
                );
            }
        }
        Err(err) => {
            error!(id = %id, error = %err, "event bus: webhook request failed");
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Pattern matching ────────────────────────────────────────────────

    #[test]
    fn wildcard_matches_everything() {
        assert!(pattern_matches("*", "session:start"));
        assert!(pattern_matches("*", "cron:failed"));
        assert!(pattern_matches("*", "anything"));
    }

    #[test]
    fn category_wildcard_matches_category() {
        assert!(pattern_matches("session:*", "session:start"));
        assert!(pattern_matches("session:*", "session:end"));
        assert!(pattern_matches("session:*", "session:compact"));
    }

    #[test]
    fn category_wildcard_does_not_match_other_category() {
        assert!(!pattern_matches("session:*", "message:received"));
        assert!(!pattern_matches("session:*", "cron:started"));
    }

    #[test]
    fn category_wildcard_does_not_match_prefix_substring() {
        // "session:*" should NOT match "sessions:start"  - the prefix must be
        // followed by a colon in the event string.
        assert!(!pattern_matches("session:*", "sessions:start"));
    }

    #[test]
    fn exact_match_works() {
        assert!(pattern_matches("session:start", "session:start"));
        assert!(!pattern_matches("session:start", "session:end"));
    }

    #[test]
    fn non_matching_pattern_returns_false() {
        assert!(!pattern_matches("agent:start", "session:start"));
        assert!(!pattern_matches("nope", "session:start"));
    }

    // ── HookEvent ───────────────────────────────────────────────────────

    #[test]
    fn event_pattern_str_round_trip() {
        let events = [
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::SessionCompact,
            HookEvent::MessageReceived,
            HookEvent::MessageSent,
            HookEvent::MessageError,
            HookEvent::AgentStart,
            HookEvent::AgentComplete,
            HookEvent::AgentError,
            HookEvent::CronStarted,
            HookEvent::CronCompleted,
            HookEvent::CronFailed,
            HookEvent::MemoryCreated,
            HookEvent::MemoryUpdated,
            HookEvent::MemoryDeleted,
            HookEvent::ConfigChanged,
            HookEvent::ConfigReloaded,
        ];

        for event in &events {
            let s = event.as_pattern_str();
            // Verify format is "category:action".
            assert!(s.contains(':'), "event pattern should contain ':'");
            // Verify Display matches.
            assert_eq!(format!("{event}"), s);
        }

        assert_eq!(events.len(), 17, "all event variants should be covered");
    }

    #[test]
    fn event_serde_roundtrip() {
        let event = HookEvent::SessionStart;
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: HookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, event);
    }

    // ── HookHandler ─────────────────────────────────────────────────────

    #[test]
    fn handler_shell_serde_roundtrip() {
        let handler = HookHandler::Shell {
            command: "echo hello".into(),
            timeout_secs: 10,
        };
        let json = serde_json::to_string(&handler).unwrap();
        let deser: HookHandler = serde_json::from_str(&json).unwrap();
        match deser {
            HookHandler::Shell {
                command,
                timeout_secs,
            } => {
                assert_eq!(command, "echo hello");
                assert_eq!(timeout_secs, 10);
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn handler_shell_default_timeout() {
        let json = r#"{"type": "shell", "command": "ls"}"#;
        let handler: HookHandler = serde_json::from_str(json).unwrap();
        match handler {
            HookHandler::Shell { timeout_secs, .. } => {
                assert_eq!(timeout_secs, 30);
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn handler_webhook_serde_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("X-Api-Key".into(), "secret".into());
        let handler = HookHandler::Webhook {
            url: "https://example.com/hook".into(),
            headers,
        };
        let json = serde_json::to_string(&handler).unwrap();
        let deser: HookHandler = serde_json::from_str(&json).unwrap();
        match deser {
            HookHandler::Webhook { url, headers } => {
                assert_eq!(url, "https://example.com/hook");
                assert_eq!(headers.get("X-Api-Key").unwrap(), "secret");
            }
            other => panic!("expected Webhook, got {other:?}"),
        }
    }

    #[test]
    fn handler_script_serde_roundtrip() {
        let handler = HookHandler::Script {
            path: PathBuf::from("/usr/local/bin/my-hook.sh"),
        };
        let json = serde_json::to_string(&handler).unwrap();
        let deser: HookHandler = serde_json::from_str(&json).unwrap();
        match deser {
            HookHandler::Script { path } => {
                assert_eq!(path, PathBuf::from("/usr/local/bin/my-hook.sh"));
            }
            other => panic!("expected Script, got {other:?}"),
        }
    }

    // ── HookRegistration ────────────────────────────────────────────────

    #[test]
    fn registration_serde_roundtrip() {
        let reg = HookRegistration {
            id: "hook-001".into(),
            event_pattern: "session:*".into(),
            handler: HookHandler::Shell {
                command: "notify-send".into(),
                timeout_secs: 5,
            },
            enabled: true,
        };
        let json = serde_json::to_string(&reg).unwrap();
        let deser: HookRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.id, "hook-001");
        assert_eq!(deser.event_pattern, "session:*");
        assert!(deser.enabled);
    }

    #[test]
    fn registration_enabled_defaults_to_true() {
        let json = r#"{
            "id": "hook-002",
            "event_pattern": "*",
            "handler": { "type": "shell", "command": "echo test" }
        }"#;
        let reg: HookRegistration = serde_json::from_str(json).unwrap();
        assert!(reg.enabled);
    }

    // ── EventBus ────────────────────────────────────────────────────────

    #[test]
    fn subscribe_adds_registration() {
        let mut bus = EventBus::new();
        assert_eq!(bus.list().len(), 0);

        bus.subscribe(HookRegistration {
            id: "h1".into(),
            event_pattern: "session:*".into(),
            handler: HookHandler::Shell {
                command: "echo".into(),
                timeout_secs: 5,
            },
            enabled: true,
        });

        assert_eq!(bus.list().len(), 1);
        assert_eq!(bus.list()[0].id, "h1");
    }

    #[test]
    fn unsubscribe_removes_registration() {
        let mut bus = EventBus::new();
        bus.subscribe(HookRegistration {
            id: "h1".into(),
            event_pattern: "*".into(),
            handler: HookHandler::Shell {
                command: "true".into(),
                timeout_secs: 1,
            },
            enabled: true,
        });
        bus.subscribe(HookRegistration {
            id: "h2".into(),
            event_pattern: "*".into(),
            handler: HookHandler::Shell {
                command: "true".into(),
                timeout_secs: 1,
            },
            enabled: true,
        });

        assert_eq!(bus.list().len(), 2);
        assert!(bus.unsubscribe("h1"));
        assert_eq!(bus.list().len(), 1);
        assert_eq!(bus.list()[0].id, "h2");
    }

    #[test]
    fn unsubscribe_returns_false_for_unknown_id() {
        let mut bus = EventBus::new();
        assert!(!bus.unsubscribe("nonexistent"));
    }

    #[tokio::test]
    async fn emit_runs_matching_handlers() {
        // Use a shell handler that writes to a temp file so we can verify
        // execution.  This is a lightweight integration-style test.
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("savfox_hook_test_{}", uuid::Uuid::now_v7()));
        let marker_str = marker.display().to_string();

        // On Windows, use `echo.> file` to create the file; on Unix, `touch`.
        #[cfg(target_os = "windows")]
        let command = format!("echo.> \"{}\"", marker_str.replace('/', "\\"));
        #[cfg(not(target_os = "windows"))]
        let command = format!("touch '{marker_str}'");

        let mut bus = EventBus::new();
        bus.subscribe(HookRegistration {
            id: "test-shell".into(),
            event_pattern: "session:*".into(),
            handler: HookHandler::Shell {
                command,
                timeout_secs: 5,
            },
            enabled: true,
        });

        bus.emit(
            HookEvent::SessionStart,
            json!({"session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f"}),
        )
        .await;

        // Give the OS a moment to flush the file.
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(
            marker.exists(),
            "expected marker file to exist at {}",
            marker.display()
        );

        // Cleanup.
        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn emit_skips_disabled_handlers() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!(
            "savfox_hook_test_disabled_{}",
            uuid::Uuid::now_v7()
        ));
        let marker_str = marker.display().to_string();

        #[cfg(target_os = "windows")]
        let command = format!("echo.> \"{}\"", marker_str.replace('/', "\\"));
        #[cfg(not(target_os = "windows"))]
        let command = format!("touch '{marker_str}'");

        let mut bus = EventBus::new();
        bus.subscribe(HookRegistration {
            id: "disabled-hook".into(),
            event_pattern: "*".into(),
            handler: HookHandler::Shell {
                command,
                timeout_secs: 5,
            },
            enabled: false,
        });

        bus.emit(HookEvent::SessionStart, json!({})).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            !marker.exists(),
            "disabled hook should not have created the marker file"
        );
    }

    #[tokio::test]
    async fn emit_skips_non_matching_patterns() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("savfox_hook_test_nomatch_{}", uuid::Uuid::now_v7()));
        let marker_str = marker.display().to_string();

        #[cfg(target_os = "windows")]
        let command = format!("echo.> \"{}\"", marker_str.replace('/', "\\"));
        #[cfg(not(target_os = "windows"))]
        let command = format!("touch '{marker_str}'");

        let mut bus = EventBus::new();
        bus.subscribe(HookRegistration {
            id: "cron-only".into(),
            event_pattern: "cron:*".into(),
            handler: HookHandler::Shell {
                command,
                timeout_secs: 5,
            },
            enabled: true,
        });

        // Emit a session event  - should not match the cron pattern.
        bus.emit(HookEvent::SessionStart, json!({})).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            !marker.exists(),
            "non-matching hook should not have created the marker file"
        );
    }

    #[test]
    fn default_event_bus_is_empty() {
        let bus = EventBus::default();
        assert!(bus.list().is_empty());
    }
}
