# WS-RPC API Reference

Complete reference for all gateway WebSocket JSON-RPC 2.0 methods. Connect to
`ws://host:port/ws` (or `wss://` with TLS). All methods use standard JSON-RPC
2.0 request/response framing.

---

## WebSocket Connection and Authentication

### Quick connect (token in query string)

```
ws://localhost:18881/ws?token=YOUR_TOKEN
```

### Challenge-response handshake

1. Client opens a WebSocket to `/ws` (no query token).
2. Server sends `ConnectChallenge`:
   ```json
   { "type": "connectChallenge", "nonce": "random-uuid", "ts": 1700000000000 }
   ```
3. Client replies with `Connect`. The `token` field may be a **raw token**
   (backwards compatible) or `HMAC-SHA256(nonce, token)` hex for replay
   protection:
   ```json
   { "type": "connect", "token": "raw-or-hmac-hex", "clientInfo": { "name": "my-app", "version": "1.0" } }
   ```
4. Server validates and sends `Connected`:
   ```json
   { "type": "connected", "sessionId": "uuid-v7", "serverVersion": "0.5.0", "protocol": 1 }
   ```
5. **Message loop** -- client sends JSON-RPC requests; server replies with
   JSON-RPC responses and pushes `Event` frames.

### JSON-RPC request format

```json
{ "jsonrpc": "2.0", "id": 1, "method": "sessions.list", "params": {} }
```

### JSON-RPC response (success)

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "active_connections": ["..."] } }
```

### JSON-RPC response (error)

```json
{ "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "method not found: foo" } }
```

## Error Codes

| Code   | Name              | Description                          |
|--------|-------------------|--------------------------------------|
| -32700 | Parse Error       | Invalid JSON                         |
| -32600 | Invalid Request   | Not a valid JSON-RPC request         |
| -32601 | Method Not Found  | Requested method does not exist      |
| -32602 | Invalid Params    | Missing or invalid parameters        |
| -32603 | Internal Error    | Server-side error                    |
| -32001 | Permission Denied | Token lacks the required scope       |

## Permission Scopes

| Scope       | TokenScope mapping  | Description                                 |
|-------------|---------------------|---------------------------------------------|
| Read        | OperatorRead        | List, get, status, search, preview, layers  |
| Write       | OperatorWrite       | Create, update, delete, patch, set, compact |
| Admin       | OperatorAdmin       | Config, gateway management                  |
| Approvals   | OperatorApprovals   | Execution approval management               |
| Pairing     | OperatorPairing     | Device and node pairing                     |
| Chat        | Chat                | Chat and message-sending operations         |

The **Operator** token scope implies all sub-scopes. **Viewer** implies Read
and Chat.

---

## Core

### `connect`

Negotiate connection parameters after the WebSocket handshake.

- **Scope**: Read
- **Params**: `{ "version": "1.0.0" }` (optional client version)
- **Response**:
  ```json
  { "status": "connected", "protocol_version": 1, "server_version": "0.5.0", "client_version": "1.0.0" }
  ```

### `health`

Health check endpoint.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "status": "ok", "version": "0.5.0" }`

### `status`

Get gateway status including connected client count.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "connected_clients": 3, "session_ids": ["uuid1", "uuid2"] }`

---

## Agent (single-agent operations)

### `agent`

Invoke the agent with a text prompt and return the response.

- **Scope**: Write
- **Params**:

| Field     | Type   | Required | Description        |
|-----------|--------|----------|--------------------|
| `message` | string | yes      | Text prompt        |
| `agent`   | string | no       | Agent ID (default: `"default"`) |

- **Response**: `{ "response": "The answer is..." }`

### `agent.identity` / `agent.identity.get`

Return the agent's identity information.

- **Scope**: Read
- **Params**: none
- **Response**:
  ```json
  { "name": "savfox", "version": "0.5.0", "capabilities": ["chat","tools","sessions","cron","nodes","tts","a2a","delegation"] }
  ```

### `agent.wait`

Invoke the agent and wait for the full response (synchronous).

- **Scope**: Write
- **Params**:

| Field     | Type   | Required | Description        |
|-----------|--------|----------|--------------------|
| `message` | string | yes      | Text prompt        |
| `agent`   | string | no       | Agent ID           |

- **Response**: `{ "response": "...", "done": true }`

### `agent.capabilities`

Return the agent's tools, skills, connected channels, and status.

- **Scope**: Read
- **Params**: `{ "agent": "default" }` (optional)
- **Response**:
  ```json
  { "agent": "Savfox Agent", "agent_id": "default", "tools": ["shell","read_file",...], "skills": [], "channels": ["discord"], "status": "active" }
  ```

### `agent.delegation.list`

List all recorded delegation entries.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "delegations": [{ "parent_agent": "default", "child_agent": "worker-1", "spawned_at": 1700000000, "purpose": "..." }], "count": 1 }`

### `agent.delegation.chain`

Get the delegation chain for a specific agent (walking up parent links).

- **Scope**: Read
- **Params**:

| Field   | Type   | Required | Description          |
|---------|--------|----------|----------------------|
| `agent` | string | yes      | Agent or agent_id    |

- **Response**: `{ "agent": "worker-1", "chain": [...], "depth": 2 }`

### `agent.delegation.record`

Record a new parent-to-child delegation entry.

- **Scope**: Write
- **Params**:

| Field          | Type   | Required | Description                    |
|----------------|--------|----------|--------------------------------|
| `parent_agent` | string | yes      | Parent agent ID                |
| `child_agent`  | string | yes      | Child agent ID                 |
| `purpose`      | string | no       | Reason for delegation          |

- **Response**: `{ "status": "recorded", "parent_agent": "...", "child_agent": "...", "spawned_at": 1700000000 }`

### `agent.delegation.remove`

Remove a delegation entry by child agent ID.

- **Scope**: Write
- **Params**:

| Field         | Type   | Required | Description      |
|---------------|--------|----------|------------------|
| `child_agent` | string | yes      | Child agent ID   |

- **Response**: `{ "status": "removed", "child_agent": "..." }`

---

## Agents (multi-agent CRUD)

### `agents.list`

List all configured agents (built-in + user-defined).

- **Scope**: Read
- **Params**: none
- **Response**: `{ "agents": [{ "id": "default", "name": "Savfox Agent", "builtin": true }, ...] }`

### `agents.create`

Create a new agent definition.

- **Scope**: Write
- **Params**:

| Field          | Type   | Required | Description              |
|----------------|--------|----------|--------------------------|
| `name`         | string | yes      | Agent display name       |
| `id`           | string | no       | Agent ID (auto-generated)|
| `description`  | string | no       | Description text         |
| `model`        | string | no       | Default model ID         |
| `system_prompt`| string | no       | System prompt            |
| `models`       | object | no       | `{ "primary": "...", "fallbacks": [...] }` |
| `thinking`     | string | no       | Thinking level           |
| `tools`        | object | no       | Tool allow/deny lists    |

- **Response**: `{ "id": "uuid", "name": "...", "status": "created" }`

### `agents.update`

Update an existing agent's configuration.

- **Scope**: Write
- **Params**:

| Field | Type   | Required | Description          |
|-------|--------|----------|----------------------|
| `id`  | string | yes      | Agent ID to update   |

Plus any updatable fields: `name`, `description`, `model`, `system_prompt`, `models`, `thinking`, `tools`, `memory`, `compaction`, `sandbox`, `identity`.

- **Response**: `{ "id": "...", "status": "updated" }`

### `agents.delete`

Delete a user-defined agent (cannot delete `"default"`).

- **Scope**: Write
- **Params**: `{ "id": "agent-id" }`
- **Response**: `{ "id": "...", "status": "deleted" }`

### `agents.files.list`

List files associated with an agent.

- **Scope**: Read
- **Params**: `{ "agent_id": "default" }`
- **Response**: `{ "agent_id": "default", "files": [{ "name": "notes.md", "size": 1234 }] }`

### `agents.files.get`

Read a specific agent file.

- **Scope**: Read
- **Params**: `{ "agent_id": "default", "path": "notes.md" }`
- **Response**: `{ "agent_id": "default", "path": "notes.md", "content": "..." }`

### `agents.files.set`

Write a file for an agent.

- **Scope**: Write
- **Params**: `{ "agent_id": "default", "path": "notes.md", "content": "..." }`
- **Response**: `{ "agent_id": "default", "path": "notes.md", "status": "saved" }`

---

## Chat

### `chat.send`

Send a chat message through the agent and return the response.

- **Scope**: Chat
- **Params**:

| Field     | Type   | Required | Description                          |
|-----------|--------|----------|--------------------------------------|
| `message` | string | yes      | Message text                         |
| `agent`   | string | no       | Agent ID (default: `"default"`)      |

- **Response**: `{ "response": "..." }`

### `chat.history`

Get chat history for a session.

- **Scope**: Read
- **Params**:

| Field         | Type   | Required | Description                   |
|---------------|--------|----------|-------------------------------|
| `session_id` | string | yes      | Session ID (UUID v7)     |
| `limit`       | number | no       | Max messages (default: 50)    |
| `source_channel` | string | no    | Filter user messages by source channel (`platform:channel`) |

- **Response**: `{ "messages": [...], "session_id": "...", "source_channel": "discord:123" }`
  - User messages include `provenance`: `{ channel, user_id, name, timestamp }`

### `chat.abort`

Abort an active agent thread. If `thread_id` is provided, aborts that specific
thread; otherwise aborts all active threads.

- **Scope**: Chat
- **Params**:

| Field         | Type   | Required | Description                     |
|---------------|--------|----------|---------------------------------|
| `thread_id`   | string | no       | Specific thread to abort        |
| `session_id` | string | no       | Alternative to thread_id        |

- **Response**: `{ "status": "aborted", "aborted_count": 1 }`

### `chat.inject`

Inject a message into a session's history without triggering the agent.

- **Scope**: Chat
- **Params**:

| Field         | Type   | Required | Description                               |
|---------------|--------|----------|-------------------------------------------|
| `session_id` | string | yes      | Session ID (UUID v7)                 |
| `content`     | string | yes      | Message content to inject                 |
| `role`        | string | no       | `"system"`, `"user"`, or `"assistant"` (default: `"system"`) |

- **Response**: `{ "status": "injected", "session_id": "...", "role": "system", "content_length": 42 }`

---

## Sessions

### `sessions.list`

List all sessions (active WebSocket connections + persistent store entries).

- **Scope**: Read
- **Params**: none
- **Response**:
  ```json
  { "active_connections": ["uuid1"], "persistent_sessions": ["01952364-8020-7d64-bdbb-f709f7d7f9c7"], "total_persistent": 5, "total_tokens": 12345 }
  ```

### `sessions.preview`

Get full metadata for a specific session.

- **Scope**: Read
- **Params**: `{ "session_id": "01952364-8020-7d64-bdbb-f709f7d7f9c7" }`
- **Response**: `{ "session_id": "...", "entry": { ... }, "identity": "chris", "linked_identities": ["discord:123", "slack:u1"] }`

### `sessions.patch`

Update session metadata (model, provider, label, channel, overrides, title).

- **Scope**: Write
- **Params**:

| Field         | Type   | Required | Description                         |
|---------------|--------|----------|-------------------------------------|
| `session_id` | string | yes      | Session ID (UUID v7)           |
| `patch`       | object | no       | Fields to update (model, label, etc.) |
| `overrides`   | object | no       | Session overrides (model, thinking) |

- **Response**: `{ "session_id": "...", "status": "patched" }`

### `sessions.reset`

Reset a session (remove from WS manager and persistent store).

- **Scope**: Write
- **Params**: `{ "session_id": "01952364-8020-7d64-bdbb-f709f7d7f9c7" }`
- **Response**: `{ "session_id": "...", "status": "reset" }`

### `sessions.delete`

Delete a session permanently.

- **Scope**: Write
- **Params**: `{ "session_id": "01952364-8020-7d64-bdbb-f709f7d7f9c7" }`
- **Response**: `{ "status": "deleted" }`

### `sessions.compact`

Compact a session's context or prune all stale entries.

- **Scope**: Write
- **Params**: `{ "session_id": "..." }` (omit for global prune)
- **Response**: `{ "session_id": "...", "status": "compacted", "compaction_count": 3 }`

### `sessions.overrides.get`

Get per-session override settings.

- **Scope**: Read
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "session_id": "...", "overrides": { "model": "gpt-4o", "thinking": "high" } }`

### `sessions.overrides.set`

Set per-session overrides (model, thinking, verbose, reasoning).

- **Scope**: Write
- **Params**:

| Field         | Type   | Required | Description                |
|---------------|--------|----------|----------------------------|
| `session_id` | string | yes      | Session ID (UUID v7)                |
| `overrides`   | object | yes      | `{ "model": "...", "thinking": "...", "verbose": "...", "reasoning": "..." }` |

- **Response**: `{ "session_id": "...", "overrides": {...}, "status": "updated" }`

### `sessions.identity_links.get`

Get cross-platform identity link mappings.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "links": { "user@email.com": ["discord:123", "telegram:456"] } }`

### `sessions.identity_links.set`

Set cross-platform identity link mappings.

- **Scope**: Write
- **Params**: `{ "links": { "user@email.com": ["discord:123", "telegram:456"] } }`
- **Response**: `{ "status": "updated", "count": 1 }`

### `identity.link`

Add or merge a canonical identity link.

- **Scope**: Write
- **Params**:

| Field       | Type         | Required | Description |
|-------------|--------------|----------|-------------|
| `canonical` | string       | yes      | Canonical identity ID |
| `ids`       | string[]     | yes      | Platform IDs in `platform:id` format |
| `id`        | string       | no       | Single ID shortcut (alternative to `ids`) |

- **Response**: `{ "status": "linked", "summary": { "canonical": "...", "added": 1, "moved_from": [...] } }`

### `sessions.dm_scope.get`

Get DM scope policy (`{savfox_home}/dm-scope.json`).

- **Scope**: Read
- **Params**: none
- **Response**: `{ "policy": { "default": "main", "agents": {}, "channels": {} } }`

### `sessions.dm_scope.set`

Set DM scope policy.

- **Scope**: Write
- **Params**:

| Field     | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `policy`  | object | yes      | DM scope policy object |

- **Response**: `{ "status": "updated", "policy": { ... } }`

### `sessions.dm_scope.migrate`

Re-key existing sessions for a new DM scope mode.

- **Scope**: Write
- **Params**:

| Field     | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `scope`   | string | yes      | Target mode: `main`, `per_peer`, `per_channel_peer`, `per_account_channel_peer` |
| `dry_run` | bool   | no       | If true, only report counts without writing |
| `agent`   | string | no       | Optional agent filter |
| `channel` | string | no       | Optional channel filter (`platform:channel`) |

- **Response**: `{ "status": "migrated", "moved": 12, "merged": 1, "skipped": 2 }`

### `sessions.usage`

Get token usage statistics for a specific session.

- **Scope**: Read
- **Params**: `{ "session_id": "..." }`
- **Response**:
  ```json
  { "session_id": "...", "input_tokens": 1000, "output_tokens": 500, "total_tokens": 1500, "model": "gpt-4o", "context_weight": { "system_prompt_pct": 30, "history_pct": 60, "tools_pct": 10 } }
  ```

### `sessions.elevate`

Elevate a session's permissions temporarily.

- **Scope**: Write
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "elevated" }`

### `sessions.unelevate`

Remove elevated permissions from a session.

- **Scope**: Write
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "unelevated" }`

---

## Typing Indicators

### `typing.start`

Broadcast a typing-start indicator to all connected clients.

- **Scope**: Write
- **Params**: `{ "session_id": "...", "agent_id": "default" }`
- **Response**: `{ "status": "typing_started", "session_id": "..." }`

### `typing.stop`

Broadcast a typing-stop indicator.

- **Scope**: Write
- **Params**: `{ "session_id": "...", "agent_id": "default" }`
- **Response**: `{ "status": "typing_stopped", "session_id": "..." }`

---

## Events (server-push subscriptions)

### `events.subscribe`

Subscribe to server-push event types. Supports wildcards (e.g., `"agent.*"`).

- **Scope**: Read
- **Params**: `{ "events": ["agent.stream", "session.updated", "typing.*"] }`
- **Response**: `{ "status": "subscribed", "events": [...], "count": 3 }`

### `events.unsubscribe`

Unsubscribe from event types.

- **Scope**: Read
- **Params**: `{ "events": ["agent.stream"] }`
- **Response**: `{ "status": "unsubscribed", "events": [...], "count": 1 }`

### `events.list`

List all available event types.

- **Scope**: Read
- **Params**: none
- **Response**:
  ```json
  { "events": [
    {"event":"agent.stream"}, {"event":"agent.complete"}, {"event":"agent.error"},
    {"event":"typing.start"}, {"event":"typing.stop"},
    {"event":"session.updated"}, {"event":"session.created"}, {"event":"session.deleted"},
    {"event":"config.changed"}, {"event":"channel.status"}, {"event":"channel.connected"},
    {"event":"channel.disconnected"}, {"event":"approval.requested"}, {"event":"approval.resolved"},
    {"event":"system.event"}, {"event":"system.presence"},
    {"event":"cron.started"}, {"event":"cron.completed"}, {"event":"memory.updated"}
  ], "count": 18 }
  ```

---

## Send / Wake / Channels

### `send`

Send a text message to a chat platform channel.

- **Scope**: Chat
- **Params**:

| Field     | Type   | Required | Description                               |
|-----------|--------|----------|-------------------------------------------|
| `channel` | string | yes      | Channel address (e.g., `"discord:12345"`) |
| `text`    | string | yes      | Message text                              |

- **Response**: `{ "status": "sent" }`

### `wake`

Wake the agent with a message or heartbeat ping.

- **Scope**: Write
- **Params**:

| Field       | Type   | Required | Description                    |
|-------------|--------|----------|--------------------------------|
| `message`   | string | no       | Prompt (default: `"wake"`)     |
| `agent`     | string | no       | Agent ID (default: `"default"`)|
| `heartbeat` | bool   | no       | If true, acknowledge only      |

- **Response**: `{ "status": "awake", "response": "..." }` or `{ "status": "heartbeat" }`

### `channels.list`

List all supported chat platforms with their webhook endpoints.

- **Scope**: Write
- **Params**: none
- **Response**:
  ```json
  { "channels": [
    {"platform":"discord","endpoint":"/webhooks/discord","type":"channel"},
    {"platform":"telegram","endpoint":"/webhooks/telegram","type":"channel"},
    {"platform":"slack","endpoint":"/webhooks/slack","type":"channel"},
    {"platform":"webhook","endpoint":"/webhooks/webhook","type":"generic"}
  ] }
  ```

### `channels.status`

Get detailed connection status for all configured channels.

- **Scope**: Write
- **Params**:
  - `channel` / `platform` (optional): return one channel only
  - `probe` (optional, bool): refresh probe status
- **Response**:
  ```json
  {
    "channels": {
      "discord": {
        "configured": true,
        "running": true,
        "connected": true,
        "lastMessageTime": "2026-02-15T10:00:00Z",
        "lastEventTime": "2026-02-15T10:01:00Z",
        "reconnectAttemptCount": 2,
        "probeStatus": "ok",
        "uptimeMs": 3600000,
        "errorRate": 0.05
      }
    }
  }
  ```

### `channels.login`

Log in to a channel channel (check if configured).

- **Scope**: Write
- **Params**: `{ "platform": "discord" }`
- **Response**: `{ "platform": "discord", "status": "already_configured", "configured": true }`

### `channels.logout`

Log out from a channel channel (clear runtime credentials).

- **Scope**: Write
- **Params**: `{ "platform": "discord" }`
- **Response**: `{ "platform": "discord", "status": "logged_out" }`

---

## Config

### `config.get`

Get the current gateway configuration as JSON.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "version": "0.5.0", "connected_clients": 2, "endpoints": {...}, "config": {...} }`

### `config.set`

Replace the entire configuration (writes config.toml).

- **Scope**: Admin
- **Params**: `{ "config": { "gateway": { "port": 18881 }, ... } }`
- **Response**: `{ "status": "ok" }`

### `config.apply`

Replace the configuration with a backup of the previous version. Auto-creates
a config snapshot before applying.

- **Scope**: Admin
- **Params**: `{ "config": {...} }`
- **Response**: `{ "status": "applied", "note": "restart required for changes to take effect" }`

### `config.patch`

Merge-patch the configuration (shallow merge, null values remove keys).

- **Scope**: Admin
- **Params**: `{ "patch": { "gateway": { "port": 9000 } } }`
- **Response**: `{ "status": "patched" }`

### `config.schema`

Get the configuration JSON schema.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "schema": { "type": "object", "properties": {...} } }`

### `config.reload`

Reload configuration from disk.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "status": "reloaded" }`

### `config.validate`

Validate a configuration object without applying it.

- **Scope**: Admin
- **Params**: `{ "config": {...} }`
- **Response**: `{ "valid": true, "errors": [] }`

### `config.migrate`

Migrate the configuration to the latest schema version.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "status": "migrated" }`

### `config.snapshot`

Create a point-in-time snapshot of the current configuration.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "snapshot_id": "...", "status": "created" }`

### `config.snapshots.list`

List all configuration snapshots.

- **Scope**: Admin
- **Params**: none
- **Response**: `{ "snapshots": [{ "id": "...", "created_at": "..." }] }`

### `config.restore`

Restore a configuration from a previously saved snapshot.

- **Scope**: Admin
- **Params**: `{ "snapshot_id": "..." }`
- **Response**: `{ "status": "restored" }`

---

## Cron

### `cron.list`

List all scheduled jobs.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "jobs": [{ "id": "...", "name": "...", "schedule": {...}, "enabled": true }] }`

### `cron.status`

Get cron service status (running state, job count).

- **Scope**: Read
- **Params**: none
- **Response**: `{ "running": true, "job_count": 3 }`

### `cron.add`

Add a new scheduled job.

- **Scope**: Write
- **Params**:

| Field      | Type   | Required | Description                                    |
|------------|--------|----------|------------------------------------------------|
| `name`     | string | yes      | Job name                                       |
| `schedule` | object | yes      | `{ "kind": "every", "interval_secs": 3600 }` or `{ "kind": "cron", "expression": "0 9 * * *" }` or `{ "kind": "at", "at_ms": 1700000000000 }` |
| `payload`  | object | yes      | `{ "type": "system_event", "text": "..." }` or `{ "type": "agent_turn", "message": "..." }` |
| `channel`  | string | no       | Delivery channel                               |

- **Response**: `{ "job_id": "...", "status": "created" }`

### `cron.update`

Update an existing job.

- **Scope**: Write
- **Params**: `{ "job_id": "...", "name": "...", "schedule": {...}, "payload": {...} }`
- **Response**: `{ "status": "updated" }`

### `cron.remove`

Remove a scheduled job.

- **Scope**: Write
- **Params**: `{ "job_id": "..." }`
- **Response**: `{ "status": "removed" }`

### `cron.run`

Manually trigger a job immediately.

- **Scope**: Write
- **Params**: `{ "job_id": "..." }`
- **Response**: `{ "status": "triggered" }`

### `cron.runs`

List run history for a job.

- **Scope**: Read
- **Params**: `{ "job_id": "...", "limit": 20 }`
- **Response**: `{ "runs": [{ "run_id": "...", "status": "ok", "started_at": "..." }] }`

---

## Models

### `models.list`

List all available models from configured providers.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "models": [{ "id": "gpt-4o", "provider": "openai" }, ...] }`

### `models.add`

Add a custom model definition.

- **Scope**: Write
- **Params**: `{ "id": "my-model", "provider": "openai", "config": {...} }`
- **Response**: `{ "status": "added" }`

### `models.update`

Update a model definition.

- **Scope**: Write
- **Params**: `{ "id": "my-model", "config": {...} }`
- **Response**: `{ "status": "updated" }`

### `models.delete`

Delete a custom model.

- **Scope**: Write
- **Params**: `{ "id": "my-model" }`
- **Response**: `{ "status": "deleted" }`

### `models.setdefault`

Set the default model for agent sessions.

- **Scope**: Write
- **Params**: `{ "model": "gpt-4o" }`
- **Response**: `{ "status": "ok" }`

### `models.aliases.get`

Get model alias mappings.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "aliases": { "fast": "gpt-4o-mini", "smart": "claude-opus-4-20250514" } }`

### `models.aliases.set`

Set model alias mappings.

- **Scope**: Write
- **Params**: `{ "aliases": { "fast": "gpt-4o-mini", "smart": "claude-opus-4-20250514" } }`
- **Response**: `{ "status": "ok" }`

### `models.resolve`

Resolve a model name or alias to a concrete model ID.

- **Scope**: Read
- **Params**: `{ "model": "fast" }`
- **Response**: `{ "resolved": "gpt-4o-mini", "alias": "fast" }`

---

## TTS (text-to-speech)

### `tts.status`

Get text-to-speech service status.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "enabled": true, "provider": "elevenlabs", "voice": "alloy" }`

### `tts.providers`

List available TTS providers.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "providers": ["elevenlabs", "openai", "system"] }`

### `tts.enable`

Enable TTS with a provider and voice.

- **Scope**: Write
- **Params**: `{ "provider": "openai", "voice": "alloy" }`
- **Response**: `{ "status": "enabled" }`

### `tts.disable`

Disable TTS.

- **Scope**: Write
- **Params**: none
- **Response**: `{ "status": "disabled" }`

### `tts.convert`

Convert text to speech audio.

- **Scope**: Write
- **Params**: `{ "text": "Hello world" }`
- **Response**: `{ "audio_url": "...", "format": "mp3" }`

### `tts.setProvider`

Set the TTS provider and voice.

- **Scope**: Write
- **Params**: `{ "provider": "elevenlabs", "voice": "alloy" }`
- **Response**: `{ "status": "ok" }`

---

## Memory

### `memory.list`

List memory entries across all layers, optionally filtered.

- **Scope**: Read
- **Params**: `{ "layer": "global" }` (optional)
- **Response**: `{ "entries": [{ "slug": "coding-conventions", "layer": "global", ... }] }`

### `memory.get`

Get a specific memory entry by layer and slug.

- **Scope**: Read
- **Params**: `{ "layer": "global", "slug": "coding-conventions" }`
- **Response**: `{ "entry": { "slug": "...", "layer": "...", "body": "...", "frontmatter": {...} } }`

### `memory.create`

Create a new memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "project", "slug": "api-notes", "body": "...", "frontmatter": { "tags": ["api"], "priority": 7 } }`
- **Response**: `{ "status": "created", "slug": "api-notes" }`

### `memory.update`

Update an existing memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "global", "slug": "coding-conventions", "body": "...", "frontmatter": {...} }`
- **Response**: `{ "status": "updated" }`

### `memory.delete`

Delete a memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "project", "slug": "old-notes" }`
- **Response**: `{ "status": "deleted" }`

### `memory.search`

Search memory entries by text query or tags.

- **Scope**: Read
- **Params**: `{ "query": "rust patterns", "layer": "global" }`
- **Response**: `{ "results": [{ "slug": "...", "layer": "...", "score": 0.85 }] }`

### `memory.promote`

Promote a session-layer entry to a persistent layer.

- **Scope**: Write
- **Params**: `{ "slug": "temp-finding", "target_layer": "project" }`
- **Response**: `{ "status": "promoted" }`

### `memory.layers`

List configured memory layer directories and paths.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "layers": [{ "name": "global", "path": "~/.savfox/memory/global" }, ...] }`

---

## Browser

### `browser.request`

Make an HTTP request via the browser service.

- **Scope**: Read
- **Params**: `{ "url": "https://example.com", "method": "GET" }`
- **Response**: `{ "status": 200, "body": "..." }`

### `browser.goto`

Navigate the headless browser to a URL.

- **Scope**: Write
- **Params**: `{ "url": "https://example.com" }`
- **Response**: `{ "status": "navigated", "url": "..." }`

### `browser.click`

Click an element on the page by CSS selector.

- **Scope**: Write
- **Params**: `{ "selector": "#submit-btn" }`
- **Response**: `{ "status": "clicked" }`

### `browser.type`

Type text into a form element.

- **Scope**: Write
- **Params**: `{ "selector": "#input-field", "text": "hello" }`
- **Response**: `{ "status": "typed" }`

### `browser.screenshot`

Take a screenshot of the current page.

- **Scope**: Write
- **Params**: `{ "format": "png" }`
- **Response**: `{ "data": "base64...", "format": "png" }`

### `browser.eval`

Evaluate JavaScript in the browser context.

- **Scope**: Write
- **Params**: `{ "expression": "document.title" }`
- **Response**: `{ "result": "Example Page" }`

---

## Plugins

### `plugins.list`

List all registered plugins and their status.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "plugins": [{ "name": "...", "enabled": true }] }`

### `plugins.enable`

Enable a plugin.

- **Scope**: Write
- **Params**: `{ "name": "my-plugin" }`
- **Response**: `{ "status": "enabled" }`

### `plugins.disable`

Disable a plugin.

- **Scope**: Write
- **Params**: `{ "name": "my-plugin" }`
- **Response**: `{ "status": "disabled" }`

### `plugins.config`

Get or set plugin configuration.

- **Scope**: Write
- **Params**: `{ "name": "my-plugin", "config": {...} }`
- **Response**: `{ "name": "...", "config": {...} }`

---

## Hooks

### `hooks.list`

List all registered event hooks.

- **Scope**: Read
- **Params**: none
- **Response**: `{ "hooks": [{ "name": "...", "event": "...", "enabled": true }] }`

### `hooks.enable`

Enable an event hook.

- **Scope**: Write
- **Params**: `{ "name": "my-hook" }`
- **Response**: `{ "status": "enabled" }`

### `hooks.disable`

Disable an event hook.

- **Scope**: Write
- **Params**: `{ "name": "my-hook" }`
- **Response**: `{ "status": "disabled" }`

---

## Reactions

### `reactions.add`

Add a reaction (emoji) to a message.

- **Scope**: Write
- **Params**: `{ "message_id": "...", "channel": "discord:123", "emoji": "thumbsup" }`
- **Response**: `{ "status": "added" }`

### `reactions.remove`

Remove a reaction from a message.

- **Scope**: Write
- **Params**: `{ "message_id": "...", "channel": "discord:123", "emoji": "thumbsup" }`
- **Response**: `{ "status": "removed" }`

---

## Streaming Config

### `streaming.config.get`

Get streaming configuration (chunking, buffering).

- **Scope**: Read
- **Params**: none
- **Response**: `{ "config": { "chunk_size": 100, "buffer_ms": 50 } }`

### `streaming.config.set`

Set streaming configuration.

- **Scope**: Write
- **Params**: `{ "config": { "chunk_size": 200, "buffer_ms": 100 } }`
- **Response**: `{ "status": "updated" }`

---

## Heartbeat Config

### `heartbeat.config.get`

Get heartbeat configuration (intervals, timeouts).

- **Scope**: Read
- **Params**: none
- **Response**: `{ "config": { "interval_ms": 30000, "timeout_ms": 90000 } }`

### `heartbeat.config.set`

Set heartbeat configuration.

- **Scope**: Write
- **Params**: `{ "config": { "interval_ms": 30000, "timeout_ms": 90000 } }`
- **Response**: `{ "status": "updated" }`

---

## Additional Methods

### Nodes

| Method              | Scope   | Description                           |
|---------------------|---------|---------------------------------------|
| `node.list`         | Read    | List known nodes                      |
| `node.describe`     | Read    | Get node details                      |
| `node.invoke`       | Write   | Invoke a capability on a node         |
| `node.invoke.result`| Read    | Get result of a node invocation       |
| `node.event`        | Write   | Send an event to a node               |
| `node.rename`       | Write   | Rename a node                         |

### Device Pairing

| Method                | Scope   | Description                        |
|-----------------------|---------|------------------------------------|
| `node.pair.request`   | Pairing | Create a new pairing request       |
| `node.pair.list`      | Pairing | List pending pairing requests      |
| `node.pair.approve`   | Pairing | Approve a pairing request          |
| `node.pair.reject`    | Pairing | Reject a pairing request           |
| `node.pair.verify`    | Pairing | Verify a pairing code              |
| `device.pair.list`    | Pairing | List paired devices                |
| `device.pair.approve` | Pairing | Approve a device pairing           |
| `device.pair.reject`  | Pairing | Reject a device pairing            |
| `device.token.rotate` | Pairing | Rotate a device's auth token       |
| `device.token.revoke` | Pairing | Revoke a device's token            |

### Skills

| Method                      | Scope | Description                     |
|-----------------------------|-------|---------------------------------|
| `skills.status`             | Read  | Get skills system status        |
| `skills.bins`               | Read  | List skill binary paths         |
| `skills.install`            | Write | Install a skill                 |
| `skills.update`             | Write | Update a skill                  |
| `skills.registry.search`    | Read  | Search the skill registry       |
| `skills.registry.install`   | Write | Install from registry           |
| `skills.registry.uninstall` | Write | Uninstall a registry skill      |

### Exec Approvals

| Method                      | Scope     | Description                      |
|-----------------------------|-----------|----------------------------------|
| `exec.approvals.get`        | Approvals | Get the current approval policy  |
| `exec.approvals.set`        | Approvals | Set the approval policy          |
| `exec.approvals.node.get`   | Approvals | Get node-specific policy         |
| `exec.approvals.node.set`   | Approvals | Set node-specific policy         |
| `exec.approval.request`     | Approvals | Create an approval request       |
| `exec.approval.resolve`     | Approvals | Resolve an approval request      |

### Usage and Logs

| Method         | Scope | Description                         |
|----------------|-------|-------------------------------------|
| `usage.status` | Read  | Get aggregate token usage stats     |
| `usage.cost`   | Read  | Get estimated cost breakdown        |
| `logs.tail`    | Read  | Get recent log entries              |

### System

| Method            | Scope | Description                        |
|-------------------|-------|------------------------------------|
| `last-heartbeat`  | Read  | Get last heartbeat timestamp       |
| `set-heartbeats`  | Write | Configure heartbeat settings       |
| `system-presence` | Read  | Report system presence status      |
| `system-event`    | Write | Send a system event                |

### Wizard

| Method          | Scope | Description                       |
|-----------------|-------|-----------------------------------|
| `wizard.start`  | Write | Start an interactive setup wizard |
| `wizard.next`   | Write | Advance to next wizard step       |
| `wizard.cancel` | Write | Cancel an active wizard           |
| `wizard.status` | Read  | Get wizard status                 |

### Webhooks

| Method            | Scope | Description                       |
|-------------------|-------|-----------------------------------|
| `webhooks.list`   | Read  | List configured webhooks          |
| `webhooks.get`    | Read  | Get webhook details               |
| `webhooks.create` | Write | Create a new webhook              |
| `webhooks.update` | Write | Update a webhook                  |
| `webhooks.delete` | Write | Delete a webhook                  |
| `webhooks.test`   | Write | Test-fire a webhook               |

### DM Policy

| Method             | Scope | Description                     |
|--------------------|-------|---------------------------------|
| `dm.policy.get`    | Read  | Get DM policy settings          |
| `dm.policy.set`    | Write | Set DM policy settings          |
| `dm.allowlist.get` | Read  | Get DM allowlist                |
| `dm.allowlist.set` | Write | Set DM allowlist                |

### Routing

| Method               | Scope | Description                   |
|----------------------|-------|-------------------------------|
| `routing.rules.list` | Read  | List agent routing rules      |
| `routing.rules.set`  | Write | Set agent routing rules       |

### Canvas

| Method          | Scope | Description                      |
|-----------------|-------|----------------------------------|
| `canvas.create` | Write | Create a new canvas              |
| `canvas.render` | Write | Render canvas content            |
| `canvas.action` | Write | Execute a canvas action          |
| `canvas.state`  | Read  | Get canvas state                 |
| `canvas.close`  | Write | Close a canvas                   |

### Misc

| Method          | Scope | Description                           |
|-----------------|-------|---------------------------------------|
| `tools.invoke`  | Write | Invoke a tool directly                |
| `talk.mode`     | Write | Set talk mode settings                |
| `voicewake.get` | Read  | Get voice wake word settings          |
| `voicewake.set` | Write | Configure voice wake word             |
| `update.run`    | Write | Trigger a gateway update check        |
| `providers.health` | Read | Check provider API health          |
| `stt.transcribe`| Read  | Transcribe audio to text              |
| `stt.providers` | Read  | List STT providers                    |

