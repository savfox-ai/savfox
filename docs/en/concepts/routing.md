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

## Group Activation

For group chats, the agent does not respond to every message by default. The
`group_activation` setting controls when the agent activates:

- **mention** -- only respond when the bot is mentioned by name.
- **keyword** -- respond when the message contains a configured trigger word.
- **always** -- respond to every message in the group.
- **command** -- only respond to slash commands (for example, `/savfox`).

This is configured per-agent in the agent config. A live session may override
`group_activation` through `sessions.patch`, but the rest of the trigger policy
is currently agent-scoped.

Additional trigger-related agent fields include:

- `group_keywords` -- keywords used when `group_activation = "keyword"`.
- `agent_aliases` -- explicit names that route a message to the agent when used
  as a leading target such as `reviewer: ...`.
- `ingest_policy` -- controls whether non-reply messages are buffered into
  ambient context.
- `external_bot_policy` -- controls whether third-party bot messages are
  ignored, buffered, or allowed to reply.
- `idle_reply` -- lets the agent wait for room inactivity and then send a
  one-time fallback reply.

## Trigger Decision Model

The runtime does not treat inbound messages as a simple yes/no reply decision.
Every inbound message ends in one of three states:

- `Reply` -- invoke the agent and send a message back to the channel.
- `IngestOnly` -- do not reply now, but preserve the message as ambient context
  for the next real reply in the same session.
- `Ignore` -- do not reply and do not preserve the message.

This decision is made in two layers:

1. A platform-neutral base trigger decision from normalized message metadata.
2. Agent-level policy overrides (`group_activation`, `ingest_policy`,
   `external_bot_policy`, `group_keywords`, `agent_aliases`, `idle_reply`).

## Base Trigger Strategy

The base trigger logic is intentionally conservative for group traffic and more
permissive for direct conversations.

### Messages ignored immediately

These messages are ignored before agent policy is evaluated:

- Messages sent by the current bot itself.
- Messages sent by the system's own agent ghost users.
- Bridge ghost/system mirror messages.
- Third-party bot messages, unless `external_bot_policy` overrides that
  behavior later.

### Messages that reply immediately

These conditions produce an immediate `Reply` in the base layer:

- The message replies to the agent's own previous message.
- The message is recognized as a command.
- The message explicitly mentions or targets the current agent.
- The conversation is a direct message.
- The room has exactly two participants. Pair rooms are treated as DM-like
  conversations even if the upstream platform labels them as a group.

### Messages that usually do not reply immediately

For larger rooms, plain-text fallback traffic is treated conservatively:

- If a platform parser forwards a plain group message through a permissive
  fallback path, the base decision becomes `IngestOnly`.
- If a message clearly targets some other agent, the base decision becomes
  `IngestOnly`.

This allows the system to keep useful context without generating message storms
in shared rooms.

## Agent-Level Policy Overrides

After the base decision is computed, the runtime loads the target agent's
trigger policy and applies overrides.

### `group_activation`

`group_activation` only affects group-like conversations (`group`,
`broadcast`, or unknown room types). It does not suppress DM or pair-room
replies.

- `mention` -- only messages that explicitly mention the agent stay replyable;
  other group messages degrade to `IngestOnly`.
- `keyword` -- mention still replies; otherwise the message replies only when
  its text contains one of `group_keywords`.
- `always` -- group fallback traffic is promoted from `IngestOnly` to `Reply`.
- `command` -- only commands reply; other group traffic degrades to
  `IngestOnly`.
- `off` -- group traffic never auto-replies and is reduced to `IngestOnly`.

`sessions.patch` can override `group_activation` at runtime for a live session.
The other trigger fields remain agent-scoped.

### `agent_aliases`

`agent_aliases` allows explicit text-based targeting such as:

- `reviewer: inspect this diff`
- `@reviewer summarize the thread`

If a leading alias matches the current agent, the message is treated like a
mention. If it matches a different agent, the current agent suppresses the
reply path and treats the message as targeted elsewhere.

### `external_bot_policy`

This setting controls how third-party bot messages behave:

- `ignore` -- keep the default behavior and drop them.
- `ingest_only` -- preserve the message as ambient context without replying.
- `reply_allowed` -- treat the external bot like a normal speaker for trigger
  purposes.

### `ingest_policy`

This setting decides which non-reply messages should still be kept as ambient
context:

- `preserve_base` -- keep the base `IngestOnly` decisions unchanged.
- `none` / `reply_only` -- drop `IngestOnly` messages instead of buffering
  them.
- `targeted_only` -- only keep `IngestOnly` traffic that was explicitly aimed
  at another agent.
- `all_human_messages` -- convert otherwise ignored human traffic into
  `IngestOnly`.
- `all_non_bot_messages` -- preserve non-bot traffic, including unknown human
  senders.
- `all_messages` -- preserve everything except self/ghost system traffic.

### `idle_reply`

`idle_reply` adds a second trigger path for buffered group traffic. Instead of
replying immediately, the runtime can wait for room inactivity and then step in
once.

Current MVP behavior:

- It only applies to group-like conversations.
- It only considers human messages that ended in `IngestOnly`.
- It does not fire for explicit mentions, commands, reply-to-self traffic, or
  messages clearly aimed at another agent.
- Any later inbound activity in the same session cancels the pending idle
  fallback.
- When the delay expires with no new activity, the agent is invoked once using
  the buffered ambient context plus an idle-fallback prompt.

The `idle_reply` object currently supports:

- `enabled` -- turn the delayed fallback on or off.
- `delay_secs` -- how long the room must stay quiet before the fallback reply
  fires.
- `max_per_hour` -- per-session guardrail limiting how many idle fallback
  replies may be sent in one hour.
- `prompt` -- optional custom instruction used when the delayed fallback
  triggers.

## Ambient Context

When a message ends in `IngestOnly`, it is stored in an in-memory ambient
buffer for that session. The next time the agent actually replies in that
session, the buffered messages are prepended to the prompt as ambient context
and then consumed.

This means the system can stay aware of relevant room activity without replying
to every message.

## Platform Notes

Channel integrations are allowed to differ in how aggressively they forward raw
 traffic into the runtime. The runtime compensates by relying on normalized
 metadata rather than platform-specific reply rules alone.

In practice:

- Direct mentions and explicit replies remain the strongest universal reply
  signals.
- Two-person rooms are treated as direct conversations across platforms.
- Group plain-text fallbacks are generally ingested first and only promoted to
  replies when the agent's trigger policy allows it.

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
