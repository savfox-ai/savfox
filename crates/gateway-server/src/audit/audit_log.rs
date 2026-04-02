use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEventType {
    AuthSuccess {
        user_id: String,
        method: String,
    },
    AuthFailure {
        user_id: Option<String>,
        method: String,
        reason: String,
    },
    CommandExecuted {
        sender_id: String,
        channel: String,
        command: String,
        args: Option<String>,
    },
    ToolInvoked {
        tool_name: String,
        user_id: Option<String>,
        session_id: Option<String>,
        success: bool,
    },
    SessionCreated {
        session_id: String,
        channel: Option<String>,
    },
    SessionClosed {
        session_id: String,
        duration_ms: Option<u64>,
        message_count: u32,
    },
    ConfigChanged {
        path: String,
        old_value: Option<String>,
        new_value: Option<String>,
        changed_by: String,
    },
    PluginLoaded {
        plugin_id: String,
        plugin_name: String,
    },
    PluginUnloaded {
        plugin_id: String,
        reason: String,
    },
    RateLimited {
        identifier: String,
        limit_type: String,
        retry_after_ms: Option<u64>,
    },
    SecurityAlert {
        severity: AuditSeverity,
        message: String,
        details: HashMap<String, String>,
    },
    ApiRequest {
        endpoint: String,
        method: String,
        status_code: u16,
        duration_ms: u64,
    },
    MessageReceived {
        channel: String,
        sender_id: String,
        content_type: String,
    },
    MessageSent {
        channel: String,
        recipient_id: String,
        content_type: String,
        success: bool,
    },
    BridgeConnected {
        bridge_type: String,
        account_id: Option<String>,
    },
    BridgeDisconnected {
        bridge_type: String,
        account_id: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub severity: AuditSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl AuditEvent {
    #[must_use]
    pub fn new(event_type: AuditEventType, severity: AuditSeverity) -> Self {
        Self {
            id: uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string(),
            timestamp: Utc::now(),
            event_type,
            severity,
            session_id: None,
            channel: None,
            user_id: None,
            metadata: HashMap::new(),
        }
    }

    pub fn auth_success(user_id: impl Into<String>, method: impl Into<String>) -> Self {
        Self::new(
            AuditEventType::AuthSuccess {
                user_id: user_id.into(),
                method: method.into(),
            },
            AuditSeverity::Info,
        )
    }

    pub fn auth_failure(
        user_id: Option<String>,
        method: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::new(
            AuditEventType::AuthFailure {
                user_id,
                method: method.into(),
                reason: reason.into(),
            },
            AuditSeverity::Warning,
        )
    }

    pub fn command_executed(
        sender_id: impl Into<String>,
        channel: impl Into<String>,
        command: impl Into<String>,
        args: Option<String>,
    ) -> Self {
        Self::new(
            AuditEventType::CommandExecuted {
                sender_id: sender_id.into(),
                channel: channel.into(),
                command: command.into(),
                args,
            },
            AuditSeverity::Info,
        )
    }

    pub fn tool_invoked(
        tool_name: impl Into<String>,
        user_id: Option<String>,
        session_id: Option<String>,
        success: bool,
    ) -> Self {
        let severity = if success {
            AuditSeverity::Info
        } else {
            AuditSeverity::Warning
        };
        Self::new(
            AuditEventType::ToolInvoked {
                tool_name: tool_name.into(),
                user_id,
                session_id,
                success,
            },
            severity,
        )
    }

    pub fn security_alert(
        severity: AuditSeverity,
        message: impl Into<String>,
        details: HashMap<String, String>,
    ) -> Self {
        Self::new(
            AuditEventType::SecurityAlert {
                severity,
                message: message.into(),
                details,
            },
            severity,
        )
    }

    pub fn rate_limited(
        identifier: impl Into<String>,
        limit_type: impl Into<String>,
        retry_after_ms: Option<u64>,
    ) -> Self {
        Self::new(
            AuditEventType::RateLimited {
                identifier: identifier.into(),
                limit_type: limit_type.into(),
                retry_after_ms,
            },
            AuditSeverity::Warning,
        )
    }

    pub fn session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
