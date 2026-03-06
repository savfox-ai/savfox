# Agent Routing

When a message arrives through a chat bridge or API, the gateway must determine
which agent should handle it and which session context to use. This document
describes the routing logic, session key construction, and rule-based routing.

## Message flow overview

```
Inbound message
  |
  v
Bridge parses payload -> ChannelAction
  |
  v
Session key construction (agent + channel + group + peer)
  |
  v
SessionStore lookup (get_or_create)
  |
  v
Agent invocation (ThreadManager)
  |
  v
Response sent back through bridge
```

## Session key construction

Every incoming message is mapped to a session key that determines which
conversation context to use. The key is built from:

1. **Agent ID** -- which agent handles this message (default: `"default"`).
2. **Channel** -- the platform and channel identifier (e.g., `discord:12345`).
3. **Group ID** -- for group chats, identifies the specific group.
4. **Peer ID** -- the sender's identity on the platform.
5. **DmScope** -- controls how direct-message sessions are scoped.

### DmScope modes

The `DmScope` enum determines how sessions are partitioned for direct messages:

| DmScope                 | Key pattern                                           | Behavior                                  |
|-------------------------|-------------------------------------------------------|-------------------------------------------|
| `Main`                  | `agent:{id}:main`                                     | All DMs share one session                 |
| `PerPeer`               | `agent:{id}:peer:{peer_id}`                           | One session per user across channels      |
| `PerChannelPeer`        | `agent:{id}:{channel}:peer:{peer_id}`                 | One session per user per channel          |
| `PerAccountChannelPeer` | `agent:{id}:acct:{account}:{channel}:peer:{peer_id}`  | One session per account + channel + user  |

Group sessions always include the group and channel:
`agent:{id}:{channel}:group:{group_id}`.

### DmScope overrides

Runtime DM scope can be overridden in `{savfox_home}/dm-scope.json`:

```json
{
  "default": "main",
  "agents": {
    "support-agent": "per_peer"
  },
  "channels": {
    "discord": "per_channel_peer",
    "discord:1234567890": "per_account_channel_peer"
  }
}
```

Resolution order is:
1. Explicit inbound override (bridge-provided `dm_scope`)
2. Exact channel override (`platform:channel`)
3. Platform override (`platform`)
4. Agent override (`agents.<agent_id>`)
5. Wildcard/default override
6. `SAVFOX_DM_SCOPE` env var fallback

### Channel tone overrides

Runtime tone/style guidance can be configured in `{savfox_home}/channel-tone.json`.
This lets you apply channel-level style and per-agent-per-channel overrides.

```json
{
  "default": "Keep responses concise and professional.",
  "channels": {
    "slack": "Be more formal on Slack.",
    "discord": "Use emoji sparingly on Discord."
  },
  "agents": {
    "support-agent": {
      "default": "Use empathetic support tone.",
      "channels": {
        "slack": "Be formal and action-oriented.",
        "discord:1234567890": "Use incident-response style, no emojis."
      }
    }
  }
}
```

Tone resolution priority:
1. Agent + exact channel (`agents.<agent>.channels["platform:channel"]`)
2. Agent + platform (`agents.<agent>.channels["platform"]`)
3. Agent wildcard/default (`agents.<agent>.channels["*"]`, then `agents.<agent>.default`)
4. Global exact channel / platform / wildcard (`channels[...]`)
5. Global `default`

### Channel addressing format

Channels use a `platform:identifier` format:

| Platform   | Example                          |
|------------|----------------------------------|
| Discord    | `discord:1234567890`             |
| Telegram   | `telegram:987654321`             |
| Slack      | `slack:C01234567`                |
| Matrix     | `matrix:!abcdef:matrix.org`      |
| Webhook    | `webhook:my-integration`         |

## Bridge action routing

Each bridge parses its platform-specific payload into a `ChannelAction`:

| ChannelAction     | Description                                          |
|------------------|------------------------------------------------------|
| `StartThread`    | Create a new agent thread with the user's prompt     |
| `SendToThread`   | Route a message to an existing thread                |
| `Approve`        | Respond to an execution approval request             |
| `Ignore`         | No action needed (e.g., verification ping)           |

### StartThread flow

1. The bridge runtime (`bridges/runtime.rs`) receives a `StartThread` action.
2. It calls `track_inbound_message()` to create or update a session entry.
3. The agent is invoked via `bridge.invoke_agent_text_with_metadata()`.
4. The response is sent back through the platform API.
5. On failure, the runtime retries up to 3 times before logging a warning.

## Routing rules

The gateway supports configurable routing rules that determine which agent
handles messages from specific channels or matching specific patterns. Rules
are managed via WS-RPC:

| Method               | Scope | Description                   |
|----------------------|-------|-------------------------------|
| `routing.rules.list` | Read  | List current routing rules    |
| `routing.rules.set`  | Write | Update routing rules          |

### Rule structure

Each routing rule specifies:

- **Pattern** -- a match condition (channel prefix, keyword, regex).
- **Agent** -- the agent ID to route matching messages to.
- **Priority** -- higher priority rules are evaluated first.

Rules are evaluated in priority order. The first matching rule determines the
target agent. If no rules match, the message is routed to the `"default"` agent.

## Group activation

For group chats, the agent does not respond to every message by default. The
`group_activation` setting controls when the agent activates:

- **mention** -- only respond when the bot is mentioned by name.
- **keyword** -- respond when the message contains a configured trigger word.
- **always** -- respond to every message in the group.
- **command** -- only respond to slash commands (e.g., `/savfox`).

This is configured per-agent or per-session via the `group_activation` field in
`sessions.patch`.

## Multi-agent routing

When multiple agents are defined, each can be assigned to different channels or
use cases:

1. Define agents via `agents.create` with unique IDs and configurations.
2. Set routing rules via `routing.rules.set` to map channels to agents.
3. Each agent maintains independent session state, tools, and model settings.

Messages can also be explicitly routed to a specific agent by including the
`agent` parameter in `chat.send` or `agent` calls.

## Identity linking

Cross-platform identity linking (`sessions.identity_links`) maps a single user
across multiple platforms. When identity links are configured, messages from the
same user on different platforms can share session context:

```json
{
  "links": {
    "user@company.com": ["discord:12345", "slack:U01234", "telegram:98765"]
  }
}
```

This allows the agent to maintain context continuity when a user switches
between Discord and Slack, for example.

Identity links can be updated incrementally with `identity.link`:

```json
{
  "canonical": "chris",
  "ids": ["discord:12345", "slack:u01234", "telegram:98765"]
}
```

Conflict behavior: if an ID already belongs to another canonical identity, it is
merged into the new canonical identity.

The runtime also performs heuristic auto-linking: if a new sender has the same
display name as an already linked identity, it can be auto-attached.

## Deduplication

The bridge runtime maintains a deduplication cache with a 10-minute TTL. If the
same event key arrives within the TTL window, the duplicate is dropped. This
prevents double-processing when platforms retry webhook deliveries.
