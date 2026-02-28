use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::a2a_types::{
    A2AMessage, A2AMessageType, DelegationEntry, now_ms, record_delegation,
};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Agent-to-agent messaging flow: orchestrates multi-turn conversations
/// between agents with optional ping-pong turns and announcement delivery.
///
/// This enables one agent to send a structured `A2AMessage` to another agent
/// session, optionally conduct a multi-turn conversation, track delegation
/// chains, and announce the result to a chat channel.
pub struct SessionsSendA2AHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const ANNOUNCE_SUMMARY_MAX_CHARS: usize = 1200;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResultInjectionFormat {
    Summary,
    FullTranscript,
}

impl ResultInjectionFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::FullTranscript => "full_transcript",
        }
    }
}

#[derive(Deserialize)]
struct SessionsSendA2AArgs {
    /// Source session/agent ID (the requester).
    source_session_id: String,
    /// Target session/agent ID (the responder).
    target_session_id: String,
    /// Initial message to send from source to target.
    message: String,
    /// Maximum number of ping-pong turns between the two agents (0 = single shot).
    #[serde(default)]
    max_ping_pong_turns: u32,
    /// Whether to announce the final result to a channel.
    #[serde(default)]
    announce: bool,
    /// Channel to announce to (e.g., "discord:channel_id").
    #[serde(default)]
    announce_channel: Option<String>,
    /// Optional system prompt for the target agent during this conversation.
    #[serde(default)]
    target_system: Option<String>,
    /// Timeout per turn in seconds.
    #[serde(default = "defaults::turn_timeout")]
    turn_timeout_secs: u64,

    // ── Structured A2A fields ──────────────────────────────────────────
    /// Message type: "request", "response", or "notification".
    /// Defaults to "request" when omitted.
    #[serde(default = "defaults::message_type")]
    message_type: A2AMessageType,
    /// Optional correlation ID for request-response matching.
    /// Auto-generated when omitted for request-type messages.
    #[serde(default)]
    correlation_id: Option<String>,
    /// Optional per-message timeout in milliseconds.
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Purpose description for delegation tracking.
    #[serde(default)]
    delegation_purpose: Option<String>,
    /// Incoming delegation chain from the caller. The handler will
    /// automatically append the source agent.
    #[serde(default)]
    delegation_chain: Vec<String>,
    /// Controls the shape of injected result payload.
    /// `summary` keeps the payload compact; `full_transcript` includes all turns.
    #[serde(default = "defaults::result_injection_format")]
    result_injection_format: ResultInjectionFormat,
}

mod defaults {
    use super::{A2AMessageType, ResultInjectionFormat};

    pub fn turn_timeout() -> u64 {
        60
    }

    pub fn message_type() -> A2AMessageType {
        A2AMessageType::Request
    }

    pub fn result_injection_format() -> ResultInjectionFormat {
        ResultInjectionFormat::Summary
    }
}

#[async_trait]
impl ToolHandler for SessionsSendA2AHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "sessions_send_a2a requires function payload".to_string(),
                ));
            }
        };

        let args: SessionsSendA2AArgs = parse_arguments(&arguments)?;

        if args.source_session_id.is_empty() || args.target_session_id.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "source_session_id and target_session_id must not be empty".to_string(),
            ));
        }
        if args.message.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "message must not be empty".to_string(),
            ));
        }

        // Build the structured A2A message envelope.
        let correlation_id = args
            .correlation_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let mut delegation_chain = args.delegation_chain.clone();
        if !delegation_chain.contains(&args.source_session_id) {
            delegation_chain.push(args.source_session_id.clone());
        }

        let a2a_msg = A2AMessage {
            from_agent: args.source_session_id.clone(),
            to_agent: args.target_session_id.clone(),
            message_type: args.message_type.clone(),
            content: serde_json::json!({ "text": &args.message }),
            correlation_id: Some(correlation_id.clone()),
            timeout_ms: args.timeout_ms,
            delegation_chain: delegation_chain.clone(),
        };

        debug!(
            from = %a2a_msg.from_agent,
            to = %a2a_msg.to_agent,
            msg_type = ?a2a_msg.message_type,
            correlation = %correlation_id,
            chain_len = delegation_chain.len(),
            "A2A message dispatching"
        );

        // Record delegation if this is a request (i.e. source is delegating to target).
        if a2a_msg.message_type == A2AMessageType::Request {
            let purpose = args
                .delegation_purpose
                .clone()
                .unwrap_or_else(|| format!("A2A request from {}", args.source_session_id));

            record_delegation(DelegationEntry {
                parent_agent: args.source_session_id.clone(),
                child_agent: args.target_session_id.clone(),
                spawned_at: now_ms(),
                purpose,
            })
            .await;
        }

        // For notification messages, send once and return immediately.
        if a2a_msg.message_type == A2AMessageType::Notification {
            return self
                .handle_notification(&args, &a2a_msg, &correlation_id, &delegation_chain)
                .await;
        }

        // For request (and response) messages, run the conversation loop.
        self.handle_request_response(&args, &a2a_msg, &correlation_id, &delegation_chain)
            .await
    }
}

impl SessionsSendA2AHandler {
    /// Handle a notification message: send once, no reply expected.
    async fn handle_notification(
        &self,
        args: &SessionsSendA2AArgs,
        a2a_msg: &A2AMessage,
        correlation_id: &str,
        delegation_chain: &[String],
    ) -> Result<ToolOutput, FunctionCallError> {
        let gateway_url = gateway_url();
        let client = build_client(args.turn_timeout_secs)?;

        // Fire the message to the target; we do not wait for a reply.
        let _reply = run_agent_step(
            &client,
            &gateway_url,
            &args.target_session_id,
            &args.message,
            args.target_system.as_deref(),
            args.turn_timeout_secs,
        )
        .await;

        let result = serde_json::json!({
            "source_session_id": args.source_session_id,
            "target_session_id": args.target_session_id,
            "message_type": "notification",
            "correlation_id": correlation_id,
            "delegation_chain": delegation_chain,
            "status": "delivered",
            "a2a_envelope": serde_json::to_value(a2a_msg).unwrap_or_default(),
        });

        Ok(ToolOutput::Function {
            content: result.to_string(),
            content_items: None,
            success: Some(true),
        })
    }

    /// Handle request/response messages with optional ping-pong turns.
    async fn handle_request_response(
        &self,
        args: &SessionsSendA2AArgs,
        a2a_msg: &A2AMessage,
        correlation_id: &str,
        delegation_chain: &[String],
    ) -> Result<ToolOutput, FunctionCallError> {
        let gateway_url = gateway_url();
        let client = build_client(args.turn_timeout_secs)?;

        // Step 1: Send initial message from source to target agent.
        let first_reply = run_agent_step(
            &client,
            &gateway_url,
            &args.target_session_id,
            &args.message,
            args.target_system.as_deref(),
            args.turn_timeout_secs,
        )
        .await?;

        let mut conversation = vec![
            serde_json::json!({
                "from": args.source_session_id,
                "to": args.target_session_id,
                "message": args.message,
                "type": "request",
                "correlation_id": correlation_id,
            }),
            serde_json::json!({
                "from": args.target_session_id,
                "to": args.source_session_id,
                "message": first_reply,
                "type": "response",
                "correlation_id": correlation_id,
            }),
        ];

        let mut last_reply = first_reply;

        // Step 2: Ping-pong turns.
        let max_turns = args.max_ping_pong_turns.min(10); // Cap at 10 turns for safety.
        for turn in 0..max_turns {
            if last_reply.is_empty() {
                break;
            }

            // Source agent processes target's reply.
            let source_reply = run_agent_step(
                &client,
                &gateway_url,
                &args.source_session_id,
                &last_reply,
                None,
                args.turn_timeout_secs,
            )
            .await?;

            conversation.push(serde_json::json!({
                "from": args.source_session_id,
                "to": args.target_session_id,
                "turn": turn + 1,
                "message": source_reply,
                "type": "request",
                "correlation_id": correlation_id,
            }));

            if source_reply.is_empty() {
                break;
            }

            // Target agent processes source's reply.
            let target_reply = run_agent_step(
                &client,
                &gateway_url,
                &args.target_session_id,
                &source_reply,
                args.target_system.as_deref(),
                args.turn_timeout_secs,
            )
            .await?;

            conversation.push(serde_json::json!({
                "from": args.target_session_id,
                "to": args.source_session_id,
                "turn": turn + 1,
                "message": target_reply,
                "type": "response",
                "correlation_id": correlation_id,
            }));

            last_reply = target_reply;
        }

        // Step 3: Announce if requested.
        let mut announced = false;
        let mut announce_channel_used: Option<String> = None;
        if args.announce && !last_reply.is_empty() {
            announce_channel_used = resolve_announce_channel(&client, &gateway_url, args).await;
            if let Some(channel) = announce_channel_used.as_deref() {
                let summary = build_announcement_summary(
                    &args.source_session_id,
                    &args.target_session_id,
                    correlation_id,
                    conversation.len(),
                    &last_reply,
                );
                announced = announce_to_channel(&client, &gateway_url, channel, &summary).await;
            }
        }

        let mut result = serde_json::json!({
            "source_session_id": args.source_session_id,
            "target_session_id": args.target_session_id,
            "message_type": a2a_msg.message_type,
            "correlation_id": correlation_id,
            "delegation_chain": delegation_chain,
            "turns": conversation.len(),
            "last_reply": last_reply,
            "announced": announced,
            "announce_channel": announce_channel_used,
            "a2a_envelope": serde_json::to_value(a2a_msg).unwrap_or_default(),
            "result_injection_format": args.result_injection_format.as_str(),
        });
        match args.result_injection_format {
            ResultInjectionFormat::FullTranscript => {
                result["conversation"] = serde_json::json!(conversation);
            }
            ResultInjectionFormat::Summary => {
                let preview: Vec<_> = conversation.iter().rev().take(4).cloned().collect();
                result["conversation_summary"] = serde_json::json!({
                    "total_messages": conversation.len(),
                    "preview_latest_first": preview,
                });
            }
        }

        Ok(ToolOutput::Function {
            content: result.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn gateway_url() -> String {
    std::env::var("SAVFOX_GATEWAY_URL").unwrap_or_else(|_| "http://127.0.0.1:18881".to_string())
}

fn build_client(timeout_secs: u64) -> Result<reqwest::Client, FunctionCallError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.max(5)))
        .build()
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
        })
}

/// Sends a prompt to an agent session via the gateway and returns the reply.
async fn run_agent_step(
    client: &reqwest::Client,
    gateway_url: &str,
    session_id: &str,
    prompt: &str,
    system: Option<&str>,
    timeout_secs: u64,
) -> Result<String, FunctionCallError> {
    let idempotency_key = uuid::Uuid::new_v4().to_string();
    let mut body = serde_json::json!({
        "session_id": session_id,
        "message": prompt,
        "idempotency_key": idempotency_key,
    });

    if let Some(system) = system {
        body["system"] = serde_json::Value::String(system.to_owned());
    }

    let agent_url = format!("{gateway_url}/api/agent");
    let response = client
        .post(&agent_url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|err| FunctionCallError::RespondToModel(format!("agent step failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        return Err(FunctionCallError::RespondToModel(format!(
            "agent step returned HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        )));
    }

    let run_result: serde_json::Value = response.json().await.unwrap_or_default();
    let run_id = run_result
        .get("run_id")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_owned();

    // Wait for completion.
    if !run_id.is_empty() {
        let wait_url = format!("{gateway_url}/api/agent/wait");
        let wait_body = serde_json::json!({
            "run_id": run_id,
            "timeout_secs": timeout_secs,
        });

        let _ = client
            .post(&wait_url)
            .header("Content-Type", "application/json")
            .body(wait_body.to_string())
            .send()
            .await;
    }

    // Read latest assistant reply.
    let history_url = format!("{gateway_url}/api/sessions/{session_id}/history?limit=50");
    let response = match client
        .get(&history_url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            warn!(status = %resp.status(), "history fetch returned non-success status");
            return Ok(String::new());
        }
        Err(err) => {
            warn!(%err, "history fetch failed");
            return Ok(String::new());
        }
    };

    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let messages = body.get("messages").and_then(|m| m.as_array());

    if let Some(messages) = messages {
        for msg in messages.iter().rev() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "assistant" {
                continue;
            }
            if msg.get("tool_calls").is_some() {
                continue;
            }
            if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.to_owned());
                }
            }
        }
    }

    Ok(String::new())
}

/// Announce a message to an external channel via the gateway send endpoint.
async fn announce_to_channel(
    client: &reqwest::Client,
    gateway_url: &str,
    channel: &str,
    message: &str,
) -> bool {
    let send_url = format!("{gateway_url}/api/message");
    let body = serde_json::json!({
        "channel": channel,
        "text": message,
    });

    match client
        .post(&send_url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn resolve_announce_channel(
    client: &reqwest::Client,
    gateway_url: &str,
    args: &SessionsSendA2AArgs,
) -> Option<String> {
    if let Some(channel) = args
        .announce_channel
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(channel.to_owned());
    }

    let preview_url = format!("{gateway_url}/api/sessions/preview?limit=500");
    let response = client.get(&preview_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload: Value = response.json().await.ok()?;
    preview_channel_for_source(&payload, &args.source_session_id)
}

fn preview_channel_for_source(payload: &Value, source_session_id: &str) -> Option<String> {
    let sessions = payload.get("sessions").and_then(Value::as_array)?;
    for session in sessions {
        let session_id = session
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if session_id != source_session_id {
            continue;
        }
        let channel = session
            .get("channel")
            .and_then(Value::as_str)
            .or_else(|| session.get("last_channel").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        return Some(channel.to_owned());
    }
    None
}

fn build_announcement_summary(
    source_session_id: &str,
    target_session_id: &str,
    correlation_id: &str,
    turns: usize,
    last_reply: &str,
) -> String {
    let sanitized = last_reply.split_whitespace().collect::<Vec<_>>().join(" ");
    let summary = if sanitized.chars().count() > ANNOUNCE_SUMMARY_MAX_CHARS {
        let trimmed: String = sanitized.chars().take(ANNOUNCE_SUMMARY_MAX_CHARS).collect();
        format!("{trimmed}...")
    } else {
        sanitized
    };
    format!(
        "[Subagent Summary]\nfrom: {source_session_id}\ntarget: {target_session_id}\ncorrelation_id: {correlation_id}\nturns: {turns}\n\n{summary}"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{build_announcement_summary, preview_channel_for_source};

    #[test]
    fn preview_channel_prefers_active_channel() {
        let payload = json!({
            "sessions": [
                {
                    "session_id": "01952364-8020-7d64-bdbb-f709f7d7f9c7",
                    "channel": "discord:123",
                    "last_channel": "discord:old"
                }
            ]
        });

        let channel = preview_channel_for_source(&payload, "01952364-8020-7d64-bdbb-f709f7d7f9c7");
        assert_eq!(channel.as_deref(), Some("discord:123"));
    }

    #[test]
    fn preview_channel_falls_back_to_last_channel() {
        let payload = json!({
            "sessions": [
                {
                    "session_id": "01952364-8021-7d64-bdbb-f709f7d7f9c8",
                    "last_channel": "telegram:456"
                }
            ]
        });

        let channel = preview_channel_for_source(&payload, "01952364-8021-7d64-bdbb-f709f7d7f9c8");
        assert_eq!(channel.as_deref(), Some("telegram:456"));
    }

    #[test]
    fn announcement_summary_includes_metadata() {
        let summary = build_announcement_summary("parent", "child", "corr-1", 4, "done");
        assert!(summary.contains("from: parent"));
        assert!(summary.contains("target: child"));
        assert!(summary.contains("correlation_id: corr-1"));
        assert!(summary.contains("turns: 4"));
        assert!(summary.contains("done"));
    }
}
