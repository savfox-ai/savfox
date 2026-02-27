# Session Management

Sessions track ongoing conversations between users and the Savfox agent. The
gateway manages two complementary session layers: in-memory WebSocket sessions
for real-time connections and a persistent file-based store for long-lived
session metadata.

## In-memory sessions (GatewaySessionManager)

`GatewaySessionManager` tracks active WebSocket clients. Each `ClientSession`
contains:

- **session_id** -- unique UUID v7 assigned at connection time.
- **token_info** -- the token's scopes, used for per-method authorization.
- **outgoing channel** -- an mpsc channel (capacity 256) for pushing messages
  to the client.
- **thread subscriptions** -- which agent thread events to forward.
- **log subscriptions** -- for real-time log streaming.

When a WebSocket disconnects, the in-memory session is removed and all
subscriptions are cleaned up.

## Persistent sessions (SessionStore)

`SessionStore` (`crates/gateway-server/src/session_store.rs`) persists session
metadata as JSONL at `{savfox_home}/sessions/<session_id>.jsonl` where
`session_id` is UUID v7.

### SessionEntry fields

Each session entry stores:

| Field               | Type        | Description                              |
|---------------------|-------------|------------------------------------------|
| `session_id`        | string      | UUID v7                                  |
| `routing_id`        | string?     | Internal routing identifier              |
| `created_at`        | u64         | Epoch milliseconds                       |
| `updated_at`        | u64         | Last activity timestamp                  |
| `channel`           | string?     | Current channel (e.g., `discord:12345`)  |
| `identity`          | string?     | Canonical linked identity ID              |
| `model`             | string?     | Model override                           |
| `provider`          | string?     | Provider override                        |
| `label`             | string?     | Human-readable label                     |
| `thread_id`         | string?     | Active thread/topic id                   |
| `parent_thread_id`  | string?     | Parent thread for inherited routing      |
| `reply_target`      | string?     | Platform message id used for replies     |
| `provenance`        | array       | Inbound source records for user messages |
| `input_tokens`      | u64         | Cumulative input tokens                  |
| `output_tokens`     | u64         | Cumulative output tokens                 |
| `total_tokens`      | u64         | Cumulative total tokens                  |
| `compaction_count`  | u32         | Number of context compactions            |
| `title`             | string?     | User-set title                           |
| `derived_title`     | string?     | Auto-generated title                     |
| `overrides`         | object?     | Per-session settings                     |

### Routing IDs (internal)

`routing_id` values are derived from agent ID, channel, group ID, thread ID,
and peer ID. They are used internally to map inbound channel messages to a
persisted `session_id` (UUID v7). The `DmScope` enum controls DM routing:

| DmScope          | Routing pattern                       |
|------------------|---------------------------------------|
| `Main`           | `agent:{id}:main`                     |
| `PerPeer`        | `agent:{id}:peer:{peer_id}`           |
| `PerChannelPeer` | `agent:{id}:{channel}:peer:{peer_id}` |
| `PerAccountChannelPeer` | `agent:{id}:acct:{account}:{channel}:peer:{peer_id}` |

Group sessions always include the group ID:
`agent:{id}:{channel}:group:{group_id}`.

## Session lifecycle

1. **Creation** -- A session is created when a message arrives on a channel for
   a routing ID that does not yet exist.
   `SessionStore::get_or_create_for_routing_id()` initializes a new
   `SessionEntry` with a UUID v7 `session_id`.

2. **Activity** -- Each message updates `updated_at` via `entry.touch()`. Token
   usage counters are incremented after each agent turn.

3. **Overrides** -- Clients can set per-session overrides (model, thinking
   level, verbosity, reasoning mode) via `sessions.overrides.set` or through
   the `sessions.patch` method.

4. **Compaction** -- When the context window grows too large, the compaction
   service summarizes older messages. The `compaction_count` field tracks how
   many times this has happened.

5. **Reset / Delete** -- `sessions.reset` removes a session from both the WS
   manager and the persistent store. `sessions.delete` is equivalent.

## Persistence mechanics

### TTL cache

The store keeps an in-memory cache with a 45-second TTL. On read, if the cache
has expired, the store checks the file modification time and reloads from disk
if it has changed.

### Pruning

A background timer runs every 5 minutes and removes sessions that are:
- Older than 30 days, or
- Exceeding the maximum count (500 entries).

The `sessions.compact` method with no `session_id` triggers a manual prune.

### File layout

Each persisted session is stored separately as
`{savfox_home}/sessions/<session_id>.jsonl`.

## Session overrides

Each session can carry per-session overrides:

| Override    | Values                           | Description                   |
|-------------|----------------------------------|-------------------------------|
| `model`     | any model ID                     | Override the default model    |
| `thinking`  | `off`, `low`, `medium`, `high`   | Thinking budget               |
| `verbose`   | `off`, `on`, `full`              | Verbosity level               |
| `reasoning` | `off`, `on`, `stream`            | Reasoning mode                |

Overrides are applied by merging non-null fields. Overrides set via
`sessions.overrides.set` or the `overrides` field in `sessions.patch` are
persisted and restored across gateway restarts.

## Session elevation

Sessions can be temporarily elevated for operations that require higher
permissions. The `sessions.elevate` and `sessions.unelevate` methods control
this. Elevated sessions gain additional capabilities without changing the
underlying token scopes.

## Context compaction

The `CompactionService` monitors context window size and triggers automatic
summarization when the token count exceeds a threshold (default: 80% of
`max_tokens`). Three modes are available:

| Mode       | Behavior                                           |
|------------|----------------------------------------------------|
| `Auto`     | Compact automatically when threshold is reached    |
| `Manual`   | Only compact when explicitly requested via RPC     |
| `Disabled` | Context grows unbounded                            |

During compaction:
1. Pinned messages and tool results are preserved verbatim.
2. Older messages are condensed into a compact summary block.
3. The summary uses at most `summary_max_tokens` (default: 2000) tokens.
4. `compaction_count` is incremented on the session entry.

## WS-RPC methods

| Method                       | Scope | Description                          |
|------------------------------|-------|--------------------------------------|
| `sessions.list`              | Read  | List active + persistent sessions    |
| `sessions.preview`           | Read  | Get full metadata for a session      |
| `sessions.patch`             | Write | Update session metadata              |
| `sessions.reset`             | Write | Reset a session                      |
| `sessions.delete`            | Write | Delete a session                     |
| `sessions.compact`           | Write | Compact context or prune stale       |
| `sessions.overrides.get`     | Read  | Get per-session overrides            |
| `sessions.overrides.set`     | Write | Set per-session overrides            |
| `sessions.identity_links.get`| Read  | Get cross-platform identity links    |
| `sessions.identity_links.set`| Write | Set cross-platform identity links    |
| `identity.link`             | Write | Incrementally link/merge identity IDs |
| `sessions.dm_scope.get`     | Read  | Get DM scope policy config            |
| `sessions.dm_scope.set`     | Write | Set DM scope policy config            |
| `sessions.dm_scope.migrate` | Write | Re-key sessions for a new DM scope    |
| `sessions.usage`             | Read  | Get token usage for a session        |
| `sessions.elevate`           | Write | Elevate session permissions          |
| `sessions.unelevate`         | Write | Remove elevation                     |
