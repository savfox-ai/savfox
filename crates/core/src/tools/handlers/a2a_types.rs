use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

// ─── A2A Message Types ──────────────────────────────────────────────────────

/// The type of an agent-to-agent message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum A2AMessageType {
    /// A request expecting a response.
    Request,
    /// A response to a prior request (matched by `correlation_id`).
    Response,
    /// A fire-and-forget notification with no expected reply.
    Notification,
}

/// Structured message envelope for agent-to-agent communication.
///
/// Enables typed, traceable messaging between agent sessions. The
/// `correlation_id` field allows request/response pairing across turns,
/// while `timeout_ms` lets the sender cap how long to wait for a reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    /// Identity of the sending agent (session ID or agent name).
    pub from_agent: String,
    /// Identity of the receiving agent (session ID or agent name).
    pub to_agent: String,
    /// Semantics of this message: request, response, or notification.
    pub message_type: A2AMessageType,
    /// Arbitrary payload carried by this message.
    pub content: serde_json::Value,
    /// Optional identifier that ties a response back to its originating request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Optional per-message timeout in milliseconds. When set, the sending
    /// agent should abandon waiting for a reply after this duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Chain of agent identifiers recording delegation history.
    /// The first entry is the original requester; the last is the
    /// most recently delegated-to agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_chain: Vec<String>,
}

impl A2AMessage {
    /// Create a new request message.
    pub fn request(
        from: impl Into<String>,
        to: impl Into<String>,
        content: serde_json::Value,
    ) -> Self {
        let correlation_id = uuid::Uuid::new_v4().to_string();
        Self {
            from_agent: from.into(),
            to_agent: to.into(),
            message_type: A2AMessageType::Request,
            content,
            correlation_id: Some(correlation_id),
            timeout_ms: None,
            delegation_chain: Vec::new(),
        }
    }

    /// Create a response message correlated to a prior request.
    pub fn response(
        from: impl Into<String>,
        to: impl Into<String>,
        content: serde_json::Value,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            from_agent: from.into(),
            to_agent: to.into(),
            message_type: A2AMessageType::Response,
            content,
            correlation_id: Some(correlation_id.into()),
            timeout_ms: None,
            delegation_chain: Vec::new(),
        }
    }

    /// Create a notification (fire-and-forget) message.
    pub fn notification(
        from: impl Into<String>,
        to: impl Into<String>,
        content: serde_json::Value,
    ) -> Self {
        Self {
            from_agent: from.into(),
            to_agent: to.into(),
            message_type: A2AMessageType::Notification,
            content,
            correlation_id: None,
            timeout_ms: None,
            delegation_chain: Vec::new(),
        }
    }

    /// Set a per-message timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Append an agent to the delegation chain.
    #[must_use]
    pub fn with_delegation(mut self, chain: Vec<String>) -> Self {
        self.delegation_chain = chain;
        self
    }
}

// ─── Agent Capabilities ─────────────────────────────────────────────────────

/// Describes the capabilities advertised by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Agent identifier (session ID or agent name).
    pub agent: String,
    /// Tool names this agent can invoke.
    pub tools: Vec<String>,
    /// Skill names installed for this agent.
    pub skills: Vec<String>,
    /// Channel IDs this agent is connected to.
    pub channels: Vec<String>,
    /// Current status of the agent (e.g. "active", "idle", "offline").
    pub status: String,
}

// ─── Delegation Chain Tracking ──────────────────────────────────────────────

/// A single entry in the delegation chain recording a parent-child spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationEntry {
    /// The agent that initiated the delegation.
    pub parent_agent: String,
    /// The agent that was spawned / delegated to.
    pub child_agent: String,
    /// Unix timestamp in milliseconds when the delegation was created.
    pub spawned_at: u64,
    /// Human-readable description of why the delegation occurred.
    pub purpose: String,
}

/// In-process store for delegation entries, keyed by child agent ID.
///
/// This is a lightweight in-memory registry; production deployments may
/// want to persist this via the session store.
fn delegation_store() -> &'static Mutex<HashMap<String, DelegationEntry>> {
    static STORE: OnceLock<Mutex<HashMap<String, DelegationEntry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a delegation between two agents.
pub async fn record_delegation(entry: DelegationEntry) {
    let mut lock = delegation_store().lock().await;
    // Cap the store at 4096 entries; prune oldest half when exceeded.
    if lock.len() > 4096 {
        let mut entries: Vec<_> = lock.values().cloned().collect();
        entries.sort_by_key(|e| e.spawned_at);
        let remove_count = entries.len() / 2;
        for e in entries.into_iter().take(remove_count) {
            lock.remove(&e.child_agent);
        }
    }
    lock.insert(entry.child_agent.clone(), entry);
}

/// List all recorded delegation entries.
pub async fn list_delegations() -> Vec<DelegationEntry> {
    let lock = delegation_store().lock().await;
    let mut entries: Vec<_> = lock.values().cloned().collect();
    entries.sort_by_key(|e| e.spawned_at);
    entries
}

/// Look up the delegation chain for a specific agent, walking up through
/// parent links.
pub async fn delegation_chain_for(agent_id: &str) -> Vec<DelegationEntry> {
    let lock = delegation_store().lock().await;
    let mut chain = Vec::new();
    let mut current = agent_id.to_owned();

    // Walk at most 32 levels to avoid infinite loops.
    for _ in 0..32 {
        if let Some(entry) = lock.get(&current) {
            chain.push(entry.clone());
            current = entry.parent_agent.clone();
        } else {
            break;
        }
    }

    chain.reverse();
    chain
}

/// Remove a delegation entry by child agent ID.
pub async fn remove_delegation(child_agent: &str) -> bool {
    let mut lock = delegation_store().lock().await;
    lock.remove(child_agent).is_some()
}

/// Get the current timestamp in milliseconds.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_message_request_has_correlation_id() {
        let msg = A2AMessage::request(
            "agent-a",
            "agent-b",
            serde_json::json!({"task": "summarize"}),
        );
        assert_eq!(msg.message_type, A2AMessageType::Request);
        assert!(msg.correlation_id.is_some());
        assert_eq!(msg.from_agent, "agent-a");
        assert_eq!(msg.to_agent, "agent-b");
    }

    #[test]
    fn a2a_message_response_preserves_correlation() {
        let msg = A2AMessage::response(
            "agent-b",
            "agent-a",
            serde_json::json!({"summary": "done"}),
            "corr-123",
        );
        assert_eq!(msg.message_type, A2AMessageType::Response);
        assert_eq!(msg.correlation_id.as_deref(), Some("corr-123"));
    }

    #[test]
    fn a2a_message_notification_has_no_correlation() {
        let msg = A2AMessage::notification(
            "agent-a",
            "agent-b",
            serde_json::json!({"event": "task_complete"}),
        );
        assert_eq!(msg.message_type, A2AMessageType::Notification);
        assert!(msg.correlation_id.is_none());
    }

    #[test]
    fn a2a_message_with_timeout() {
        let msg =
            A2AMessage::request("agent-a", "agent-b", serde_json::json!({})).with_timeout(5000);
        assert_eq!(msg.timeout_ms, Some(5000));
    }

    #[test]
    fn a2a_message_serialization_roundtrip() {
        let msg = A2AMessage::request("src", "dst", serde_json::json!({"key": "value"}))
            .with_timeout(3000)
            .with_delegation(vec!["root".to_owned(), "src".to_owned()]);

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: A2AMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.from_agent, "src");
        assert_eq!(deserialized.to_agent, "dst");
        assert_eq!(deserialized.timeout_ms, Some(3000));
        assert_eq!(deserialized.delegation_chain.len(), 2);
        assert_eq!(deserialized.delegation_chain[0], "root");
    }

    #[test]
    fn delegation_entry_serialization() {
        let entry = DelegationEntry {
            parent_agent: "parent".to_owned(),
            child_agent: "child".to_owned(),
            spawned_at: 1700000000000,
            purpose: "test delegation".to_owned(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: DelegationEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.parent_agent, "parent");
        assert_eq!(deserialized.child_agent, "child");
        assert_eq!(deserialized.spawned_at, 1700000000000);
    }

    #[tokio::test]
    async fn delegation_store_operations() {
        let entry = DelegationEntry {
            parent_agent: "test-parent".to_owned(),
            child_agent: "test-child-unique".to_owned(),
            spawned_at: now_ms(),
            purpose: "unit test".to_owned(),
        };

        record_delegation(entry.clone()).await;

        let all = list_delegations().await;
        assert!(all.iter().any(|e| e.child_agent == "test-child-unique"));

        let chain = delegation_chain_for("test-child-unique").await;
        assert!(!chain.is_empty());
        assert_eq!(chain.last().unwrap().child_agent, "test-child-unique");

        let removed = remove_delegation("test-child-unique").await;
        assert!(removed);
    }

    #[test]
    fn agent_capabilities_serialization() {
        let caps = AgentCapabilities {
            agent: "my-agent".to_owned(),
            tools: vec!["shell".to_owned(), "read_file".to_owned()],
            skills: vec!["code-review".to_owned()],
            channels: vec!["discord:12345".to_owned()],
            status: "active".to_owned(),
        };

        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("my-agent"));
        assert!(json.contains("code-review"));

        let deserialized: AgentCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tools.len(), 2);
        assert_eq!(deserialized.status, "active");
    }
}
