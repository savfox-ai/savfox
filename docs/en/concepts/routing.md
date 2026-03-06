# Message Routing

This document describes how messages flow from external chat channels through the gateway to the agent and back.

## Overview

```
User Message          Agent Response
     |                     ^
     v                     |
  Channel                Channel
  (Discord, Telegram,   (sends reply back
   Slack, etc.)          to platform)
     |                     ^
     v                     |
  Webhook Handler      Response Sender
     |                     ^
     v                     |
  Session Router       Session Manager
     |                     ^
     v                     |
  Agent (Core Engine)  ----+
```

## Inbound Flow

### 1. Platform Receives Message

A user sends a message on a chat platform (Discord channel, Telegram DM, Slack thread, etc.). The platform delivers it to the gateway via webhook or real-time connection.

### 2. Webhook Verification

The gateway verifies the request is authentic:

- **Discord**: Ed25519 signature verification (`x-signature-ed25519` + `x-signature-timestamp`).
- **Slack**: HMAC-SHA256 verification (`x-slack-signature`) with a 5-minute replay window.
- **Telegram**: Secret token comparison (`x-telegram-bot-api-secret-token`).
- **Generic Webhook**: HMAC-SHA256 (`x-signature` or `x-hub-signature-256`).

Unsigned or invalid requests receive `401` and are dropped.

### 3. Message Normalization

Each channel normalizes the platform-specific message format into a common internal structure containing:

- `text` -- The message content.
- `sender` -- User identifier on the platform.
- `channel` -- Platform and channel identifier (e.g., `discord:123456`).
- `group_id` -- Group/channel ID for group chats, `None` for DMs.
- `thread_id` -- Thread ID for threaded conversations.
- `chat_type` -- `"dm"`, `"group"`, or `"channel"`.

### 4. Session Resolution

The session router builds a **session key** from the normalized message components and the DM scoping mode:

```
agent:{agent_id}:{channel}:group:{group_id}:topic:{thread_id}
```

It then looks up the session in the session store:

- If a session exists with this key, it is reused.
- If no session exists, a new one is created with a fresh UUID v7.

The session's `updated_at` timestamp is touched on every message.

### 5. Agent Invocation

The message is appended to the session's conversation history and forwarded to the core engine. The ThreadManager:

1. Loads memory context (4-layer system).
2. Builds the full prompt with system instructions, memory, and message history.
3. Streams the request to the configured LLM provider.
4. Processes tool calls (shell, file edit, search, etc.) in a loop until the agent is done.

### 6. Context Compaction

If the conversation history exceeds the model's context window, the ThreadManager compacts older messages into a summary before sending the request.

## Outbound Flow

### 7. Response Delivery

The agent's final text response is routed back through the channel:

1. The session manager identifies which channel and channel the response should go to (from the session's `channel` and delivery context fields).
2. The channel formats the response for the target platform (Markdown to Slack mrkdwn, Discord embeds, etc.).
3. The channel sends the response via the platform's API.

For long responses, channels may split the text into multiple messages to respect platform character limits.

## WebSocket Clients

WebSocket clients follow a similar but simpler path:

1. Client connects to `/ws` and authenticates (token via query param or `Connect` message).
2. Client sends a `Request` message with method `chat.send`.
3. The gateway routes it through the session manager to the agent.
4. Streaming responses are pushed back as `Event` messages.
5. The final response is sent as a `Response` message.

## Rate Limiting

The gateway applies token-bucket rate limiting per authentication token. Requests that exceed the rate limit receive a `429 Too Many Requests` response. This applies to both REST and WebSocket requests.

## Approval Routing

When the agent wants to execute a command that requires approval:

1. The agent emits an approval request.
2. The gateway forwards it to the originating channel (shown as an interactive prompt in Discord, Telegram, etc.).
3. The user approves or denies.
4. The resolution is routed back to the agent, which proceeds or aborts.

Approvals can also be managed via `savfox gateway approvals` from the CLI.

## Multi-Channel Sessions

A session can span multiple channels. If a user starts a conversation on Discord and later continues on Telegram (using the same agent and scoping), the session's `channel` and `last_channel` fields track the migration. Message history is preserved.
