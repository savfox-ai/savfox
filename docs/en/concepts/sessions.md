# Sessions

Sessions track the state of a conversation between a user and the Savfox agent. Each session maintains message history, token usage, and delivery context.

## Session Identity

Every session has a unique `session_id` (UUID v7). The gateway may also keep an
internal `routing_id` to map inbound channel messages to that persisted
`session_id`. Routing IDs are derived from:

- **Agent ID** -- Which agent configuration is active.
- **Channel** -- The source platform (e.g., `discord`, `telegram`, `api`).
- **Group ID** -- For group chats, the group or channel identifier.
- **Peer ID** -- For DMs, the user's platform-specific identifier.
- **Thread ID** -- For threaded conversations (Discord threads, Slack threads).

Example routing IDs (internal):

```
agent:default:main                      # Default DM session
agent:default:discord:group:123456      # Discord channel
agent:default:telegram:peer:789012      # Telegram DM
agent:default:slack:group:C01234:topic:T56789  # Slack thread
```

## DM Scoping

Direct message sessions can be scoped in three ways:

| Scope | Routing Pattern | Behavior |
|-------|-------------|----------|
| `Main` (default) | `agent:{id}:main` | Single session per agent, shared across all DM peers |
| `PerPeer` | `agent:{id}:peer:{peer_id}` | Separate session per user |
| `PerChannelPeer` | `agent:{id}:{channel}:peer:{peer_id}` | Separate session per user per platform |

## Session Metadata

Each session entry stores:

| Field | Description |
|-------|-------------|
| `session_id` | Unique UUID v7 |
| `routing_id` | Internal routing identifier |
| `created_at` | Creation timestamp (epoch ms) |
| `updated_at` | Last activity timestamp (epoch ms) |
| `channel` | Current platform (discord, telegram, etc.) |
| `model` | Model override for this session |
| `input_tokens` | Cumulative input token count |
| `output_tokens` | Cumulative output token count |
| `compaction_count` | Number of context compactions performed |
| `chat_type` | "dm", "group", or "channel" |

## Persistence

Sessions are stored as JSONL in `{savfox_home}/sessions/<session_id>.jsonl`.
The store uses:

- **In-memory cache** with a 45-second TTL for fast lookups.
- **File-based persistence** for durability across gateway restarts.
- **Atomic writes** to prevent corruption on crash.

Configuration defaults:

| Setting | Default |
|---------|---------|
| Max age | 30 days |
| Max entries | 500 |
| Max file size | 10 MB |
| Max backups | 3 |
| Cache TTL | 45 seconds |

## Pruning

Stale sessions are pruned automatically every 5 minutes. A session is considered stale if its `updated_at` timestamp is older than the configured `max_age` (default: 30 days).

When the entry count exceeds `max_entries`, the oldest sessions (by `updated_at`) are removed first.

## Rotation

Each session is persisted in its own file named by UUID v7 `session_id`.

## Context Compaction

As a session accumulates messages, it may exceed the LLM's context window. When this happens, the ThreadManager performs compaction:

1. Older messages are summarized into a condensed context block.
2. The original messages are replaced with the summary.
3. The `compaction_count` is incremented.

This lets long-running sessions continue without losing important context.

## Managing Sessions

### CLI

```bash
savfox resume              # Interactive session picker
savfox resume --last       # Resume the most recent session
savfox resume <SESSION_ID> # Resume a specific session
```

### Gateway API

```bash
# List all sessions
curl http://localhost:18881/api/sessions -H "Authorization: Bearer <token>"

# View session history
curl http://localhost:18881/api/sessions/<id>/history -H "Authorization: Bearer <token>"
```

### WebSocket RPC

```json
{"jsonrpc": "2.0", "id": 1, "method": "sessions.list", "params": {}}
{"jsonrpc": "2.0", "id": 2, "method": "sessions.delete", "params": {"session_id": "..."}}
{"jsonrpc": "2.0", "id": 3, "method": "sessions.reset", "params": {"session_id": "..."}}
```
