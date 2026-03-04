# WS-RPC API Reference

The gateway exposes a JSON-RPC 2.0 API over WebSocket at `/ws`. All methods
follow the standard JSON-RPC 2.0 request/response format.

## Connection

Connect to `ws://host:port/ws` (or `wss://` with TLS). Authentication is
required before any RPC methods can be called. See the
[Architecture](../concepts/architecture.md) document for the full WebSocket
lifecycle.

### Quick connect with token query parameter

```
ws://localhost:18881/ws?token=YOUR_TOKEN
```

### JSON-RPC request format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sessions.list",
  "params": {}
}
```

### JSON-RPC response format

Success:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": { ... }
}
```

Error:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32601,
    "message": "method not found: unknown.method"
  }
}
```

## Error codes

| Code    | Name              | Description                              |
|---------|-------------------|------------------------------------------|
| -32700  | Parse Error       | Invalid JSON                             |
| -32600  | Invalid Request   | Not a valid JSON-RPC request             |
| -32601  | Method Not Found  | Requested method does not exist          |
| -32603  | Internal Error    | Server-side error                        |
| -32001  | Permission Denied | Token lacks the required scope           |

## Permission scopes

Every method requires a specific scope. If the token does not have the required
scope, the request is rejected with error code `-32001`.

| Scope       | Description                                    | TokenScope mapping   |
|-------------|------------------------------------------------|----------------------|
| `Read`      | List, get, status, search operations           | `OperatorRead`       |
| `Write`     | Create, update, delete, mutating operations    | `OperatorWrite`      |
| `Admin`     | Config, gateway management                     | `OperatorAdmin`      |
| `Approvals` | Execution approval management                  | `OperatorApprovals`  |
| `Pairing`   | Device and node pairing                        | `OperatorPairing`    |
| `Chat`      | Chat and message-sending operations            | `Chat`               |

The `Operator` token scope implies all sub-scopes. `Viewer` implies `Read`.

---

## Core methods

### `connect`

Establish a connection (typically handled during the WebSocket handshake).

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "status": "ok" }`

### `health`

Health check.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "status": "ok", "version": "0.1.0" }`

### `status`

Get gateway status including connected client count.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "connected_clients": 3, "session_ids": [...] }`

---

## Agent methods (single-agent operations)

### `agent`

Invoke the agent with a text prompt.

- **Scope**: Write
- **Params**:

| Field     | Type   | Required | Description              |
|-----------|--------|----------|--------------------------|
| `message` | string | Yes      | Text prompt              |
| `model`   | string | No       | Model override           |

- **Response**: `{ "status": "started", "run_id": "uuid" }`

### `agent.wait`

Wait for an agent run to complete.

- **Scope**: Write
- **Params**:

| Field          | Type   | Required | Description              |
|----------------|--------|----------|--------------------------|
| `run_id`       | string | Yes      | Run ID from `agent`      |
| `timeout_secs` | number | No       | Timeout (default: 60)    |

- **Response**: `{ "status": "completed", "run_id": "...", "result": "..." }`

### `agent.identity` / `agent.identity.get`

Get the agent identity information.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "name": "savfox", "version": "..." }`

### `agent.capabilities`

Get the agent's current capabilities.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "tools": [...], "models": [...] }`

### `agent.delegation.list`

List active agent delegations.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "delegations": [...] }`

### `agent.delegation.chain`

Get the delegation chain for a thread.

- **Scope**: Read
- **Params**: `{ "thread_id": "..." }`
- **Response**: `{ "chain": [...] }`

### `agent.delegation.record`

Record a new delegation.

- **Scope**: Write
- **Params**: `{ "parent_thread_id": "...", "child_thread_id": "...", "role": "worker" }`
- **Response**: `{ "status": "ok" }`

### `agent.delegation.remove`

Remove a delegation record.

- **Scope**: Write
- **Params**: `{ "thread_id": "..." }`
- **Response**: `{ "status": "ok" }`

---

## Agents methods (multi-agent CRUD)

### `agents.list`

List all configured agents.

- **Scope**: Read
- **Response**: `{ "agents": [...] }`

### `agents.create`

Create a new agent definition.

- **Scope**: Write
- **Params**: `{ "name": "...", "role": "worker", "config": {...} }`
- **Response**: `{ "agent_id": "...", "status": "created" }`

### `agents.update`

Update an existing agent.

- **Scope**: Write
- **Params**: `{ "agent_id": "...", "config": {...} }`
- **Response**: `{ "status": "ok" }`

### `agents.delete`

Delete an agent.

- **Scope**: Write
- **Params**: `{ "agent_id": "..." }`
- **Response**: `{ "status": "ok" }`

### `agents.files.list`

List files associated with an agent.

- **Scope**: Read
- **Params**: `{ "agent_id": "..." }`
- **Response**: `{ "files": [...] }`

### `agents.files.get`

Get a specific agent file.

- **Scope**: Read
- **Params**: `{ "agent_id": "...", "path": "..." }`
- **Response**: `{ "content": "..." }`

### `agents.files.set`

Write a file for an agent.

- **Scope**: Write
- **Params**: `{ "agent_id": "...", "path": "...", "content": "..." }`
- **Response**: `{ "status": "ok" }`

---

## Chat methods

### `chat.send`

Send a chat message through the agent.

- **Scope**: Chat
- **Params**:

| Field       | Type   | Required | Description                    |
|-------------|--------|----------|--------------------------------|
| `message`   | string | Yes      | Message text                   |
| `channel`   | string | No       | Target channel                 |
| `session_id`| string | No       | Session to send to             |

- **Response**: `{ "status": "ok", "reply": "..." }`

### `chat.history`

Get chat history for a session.

- **Scope**: Read
- **Params**: `{ "session_id": "...", "limit": 50, "source_channel": "discord:123" }`
- **Response**: `{ "messages": [...], "source_channel": "discord:123" }`
  - User messages include `provenance`: `{ channel, user_id, name, timestamp }`

### `chat.abort`

Abort an active chat session.

- **Scope**: Chat
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "ok" }`

---

## Session methods

### `sessions.list`

List all sessions (both active WebSocket and persistent store entries).

- **Scope**: Read
- **Response**: `{ "active": [...], "stored": [...] }`

### `sessions.preview`

Get a preview of recent sessions.

- **Scope**: Read
- **Params**: `{ "limit": 20 }`
- **Response**: `{ "sessions": [...] }`

### `sessions.patch`

Update session metadata.

- **Scope**: Write
- **Params**: `{ "session_id": "...", "label": "...", "model": "..." }`
- **Response**: `{ "status": "ok" }`

### `sessions.reset`

Reset a session (clear history, start fresh).

- **Scope**: Write
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "ok" }`

### `sessions.delete`

Delete a session.

- **Scope**: Write
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "ok" }`

### `sessions.compact`

Compact a session's context window.

- **Scope**: Write
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "status": "ok", "compacted_tokens": 1234 }`

### `sessions.overrides.get`

Get per-session overrides.

- **Scope**: Read
- **Params**: `{ "session_id": "..." }`
- **Response**: `{ "overrides": { "model": "...", "thinking": "..." } }`

### `sessions.overrides.set`

Set per-session overrides.

- **Scope**: Write
- **Params**: `{ "session_id": "...", "overrides": { "model": "gpt-4o", "thinking": "high" } }`
- **Response**: `{ "status": "ok" }`

### `sessions.identity_links.get`

Get cross-platform identity links.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "links": { "chris": ["discord:123", "slack:u1"] } }`

### `sessions.identity_links.set`

Set full cross-platform identity links map.

- **Scope**: Write
- **Params**: `{ "links": { "chris": ["discord:123", "slack:u1"] } }`
- **Response**: `{ "status": "updated", "count": 1 }`

### `identity.link`

Incrementally add or merge identity links.

- **Scope**: Write
- **Params**: `{ "canonical": "chris", "ids": ["telegram:888"] }`
- **Response**: `{ "status": "linked", "summary": { ... } }`

### `sessions.dm_scope.get`

Get DM scope policy.

- **Scope**: Read
- **Params**: `{}`
- **Response**: `{ "policy": { "default": "main", "agents": {}, "channels": {} } }`

### `sessions.dm_scope.set`

Set DM scope policy.

- **Scope**: Write
- **Params**: `{ "policy": { "default": "per_peer", "agents": { "support": "per_peer" } } }`
- **Response**: `{ "status": "updated", "policy": { ... } }`

### `sessions.dm_scope.migrate`

Re-key existing sessions to a target DM scope mode.

- **Scope**: Write
- **Params**: `{ "scope": "per_channel_peer", "dry_run": true }`
- **Response**: `{ "status": "dry_run", "moved": 10, "merged": 1, "skipped": 2 }`

---

## Channel methods

### `send`

Send a message to a chat platform channel.

- **Scope**: Chat
- **Params**: `{ "channel": "discord:12345", "text": "Hello" }`
- **Response**: `{ "status": "ok" }`

### `wake`

Wake the agent (trigger a system event).

- **Scope**: Write
- **Params**: `{ "reason": "scheduled_task" }`
- **Response**: `{ "status": "ok" }`

### `channels.list`

List configured channels and their status.

- **Scope**: Write
- **Response**: `{ "channels": [{ "id": "discord", "status": "configured" }, ...] }`

### `channels.status`

Get detailed channel status.

- **Scope**: Write
- **Params**:
  - `channel` / `platform` (optional): return one channel only
  - `probe` (optional, bool): refresh probe status
- **Response**: includes per-channel health metrics:
  - `lastMessageTime`, `lastEventTime`
  - `reconnectAttemptCount`
  - `probeStatus`
  - `uptimeMs`
  - `errorRate`

### `channels.login`

Login to a channel bridge.

- **Scope**: Write
- **Params**: `{ "channel": "discord", "credentials": {...} }`
- **Response**: `{ "status": "ok" }`

### `channels.logout`

Logout from a channel bridge.

- **Scope**: Write
- **Params**: `{ "channel": "discord" }`
- **Response**: `{ "status": "ok" }`

---

## Config methods

### `config.get`

Get the current configuration.

- **Scope**: Admin
- **Response**: `{ "config": {...} }`

### `config.set`

Set a configuration value.

- **Scope**: Admin
- **Params**: `{ "key": "gateway.port", "value": 8080 }`
- **Response**: `{ "status": "ok" }`

### `config.patch`

Merge-patch the configuration.

- **Scope**: Admin
- **Params**: `{ "config": {...}, "note": "reason" }`
- **Response**: `{ "status": "ok", "config_path": "..." }`

### `config.apply`

Replace the configuration entirely.

- **Scope**: Admin
- **Params**: `{ "config": {...}, "note": "reason" }`
- **Response**: `{ "status": "ok", "config_path": "..." }`

### `config.schema`

Get the configuration JSON schema.

- **Scope**: Admin
- **Response**: `{ "schema": {...} }`

---

## Cron methods

### `cron.list`

List all scheduled jobs.

- **Scope**: Read
- **Response**: `{ "jobs": [...] }`

### `cron.status`

Get cron service status.

- **Scope**: Read
- **Response**: `{ "running": true, "job_count": 5 }`

### `cron.add`

Add a new scheduled job.

- **Scope**: Write
- **Params**:

| Field      | Type   | Required | Description                          |
|------------|--------|----------|--------------------------------------|
| `name`     | string | Yes      | Job name                             |
| `schedule` | object | Yes      | Schedule type (at/every/cron)        |
| `payload`  | object | Yes      | Payload (systemEvent or agentTurn)   |
| `channel`  | string | No       | Delivery channel                     |

- **Response**: `{ "job_id": "...", "status": "created" }`

### `cron.update`

Update an existing job.

- **Scope**: Write
- **Params**: `{ "job_id": "...", "name": "...", "schedule": {...} }`
- **Response**: `{ "status": "ok" }`

### `cron.remove`

Remove a scheduled job.

- **Scope**: Write
- **Params**: `{ "job_id": "..." }`
- **Response**: `{ "status": "ok" }`

### `cron.run`

Manually trigger a job.

- **Scope**: Write
- **Params**: `{ "job_id": "..." }`
- **Response**: `{ "status": "ok", "run_id": "..." }`

### `cron.runs`

List run history for a job.

- **Scope**: Read
- **Params**: `{ "job_id": "...", "limit": 20 }`
- **Response**: `{ "runs": [...] }`

---

## Node methods

### `node.list`

List known nodes.

- **Scope**: Read
- **Response**: `{ "nodes": [...] }`

### `node.describe`

Get details about a specific node.

- **Scope**: Read
- **Params**: `{ "node_id": "..." }`
- **Response**: `{ "node": {...} }`

### `node.capabilities.list`

List gateway-supported node capabilities and their pairing/approval metadata.

- **Scope**: Read
- **Response**: `{ "capabilities": [{ "id": "camera.snap", "method": "system.camera", "requires_pairing": true, "requires_approval": true, ... }] }`

### `node.invoke`

Invoke a capability on a node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "method": "...", "params": {...} }`
- **Response**: `{ "request_id": "...", "status": "pending" }`

### `node.invoke.result`

Get the result of a node invocation.

- **Scope**: Read
- **Params**: `{ "request_id": "..." }`
- **Response**: `{ "status": "completed", "result": {...} }`

### `node.event`

Send an event to a node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "event": "...", "data": {...} }`
- **Response**: `{ "status": "ok" }`

### `node.rename`

Rename a node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "name": "..." }`
- **Response**: `{ "status": "ok" }`

### `node.camera.snap`

Invoke still-image capture on a paired node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "device": "optional" }`
- **Response**: Same shape as `node.invoke`

### `node.camera.clip`

Invoke short camera clip capture on a paired node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "duration_ms": 3000 }`
- **Response**: Same shape as `node.invoke`

### `node.screen.record`

Invoke screen recording on a paired node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "duration_ms": 5000, "display": 0 }`
- **Response**: Same shape as `node.invoke`

### `node.location.get`

Invoke location retrieval on a paired node.

- **Scope**: Write
- **Params**: `{ "node_id": "..." }`
- **Response**: Same shape as `node.invoke`

### `node.notify`

Send a device notification via a paired node.

- **Scope**: Write
- **Params**: `{ "node_id": "...", "title": "...", "body": "..." }`
- **Response**: Same shape as `node.invoke`

---

## Device pairing methods

### `node.pair.request`

Create a new pairing request.

- **Scope**: Pairing
- **Params**: `{ "node_id": "...", "device_name": "..." }`
- **Response**: `{ "request_id": "...", "code": "..." }`

### `node.pair.list`

List pending pairing requests.

- **Scope**: Pairing
- **Response**: `{ "requests": [...] }`

### `node.pair.approve`

Approve a pairing request.

- **Scope**: Pairing
- **Params**: `{ "request_id": "..." }`
- **Response**: `{ "status": "approved", "token": "..." }`

### `node.pair.reject`

Reject a pairing request.

- **Scope**: Pairing
- **Params**: `{ "request_id": "..." }`
- **Response**: `{ "status": "rejected" }`

### `node.pair.verify`

Verify a pairing code.

- **Scope**: Pairing
- **Params**: `{ "code": "..." }`
- **Response**: `{ "valid": true, "node_id": "..." }`

### `device.pair.list`

List paired devices.

- **Scope**: Pairing
- **Response**: `{ "devices": [...] }`

### `device.pair.approve` / `device.pair.reject`

Approve or reject a device pairing.

- **Scope**: Pairing
- **Params**: `{ "device_id": "..." }`

### `device.token.rotate`

Rotate a device's authentication token.

- **Scope**: Pairing
- **Params**: `{ "device_id": "..." }`
- **Response**: `{ "token": "new-token" }`

### `device.token.revoke`

Revoke a device's token.

- **Scope**: Pairing
- **Params**: `{ "device_id": "..." }`
- **Response**: `{ "status": "revoked" }`

---

## TTS methods

### `tts.status`

Get text-to-speech service status.

- **Scope**: Read
- **Response**: `{ "enabled": true, "provider": "...", "voice": "..." }`

### `tts.providers`

List available TTS providers.

- **Scope**: Read
- **Response**: `{ "providers": [...] }`

### `tts.enable`

Enable TTS.

- **Scope**: Write
- **Params**: `{ "provider": "...", "voice": "..." }`
- **Response**: `{ "status": "ok" }`

### `tts.disable`

Disable TTS.

- **Scope**: Write
- **Response**: `{ "status": "ok" }`

### `tts.convert`

Convert text to speech.

- **Scope**: Write
- **Params**: `{ "text": "Hello world" }`
- **Response**: `{ "audio_url": "...", "format": "mp3" }`

### `tts.setProvider`

Set the TTS provider and voice.

- **Scope**: Write
- **Params**: `{ "provider": "...", "voice": "..." }`
- **Response**: `{ "status": "ok" }`

---

## Skills methods

### `skills.status`

Get skills system status.

- **Scope**: Read
- **Response**: `{ "installed_count": 5, "available_count": 20 }`

### `skills.bins`

List skill binary paths.

- **Scope**: Read
- **Response**: `{ "bins": [...] }`

### `skills.install`

Install a skill.

- **Scope**: Write
- **Params**: `{ "name": "...", "url": "..." }`
- **Response**: `{ "status": "ok" }`

### `skills.update`

Update a skill.

- **Scope**: Write
- **Params**: `{ "name": "..." }`
- **Response**: `{ "status": "ok" }`

---

## Exec approval methods

### `exec.approvals.get`

Get the current approval policy.

- **Scope**: Approvals
- **Response**: `{ "policy": "..." }`

### `exec.approvals.set`

Set the approval policy.

- **Scope**: Approvals
- **Params**: `{ "policy": "..." }`
- **Response**: `{ "status": "ok" }`

### `exec.approvals.node.get`

Get approval policy for a specific node.

- **Scope**: Approvals
- **Params**: `{ "node_id": "..." }`
- **Response**: `{ "policy": "..." }`

### `exec.approvals.node.set`

Set approval policy for a specific node.

- **Scope**: Approvals
- **Params**: `{ "node_id": "...", "policy": "..." }`
- **Response**: `{ "status": "ok" }`

### `exec.approval.request`

Create an execution approval request.

- **Scope**: Approvals
- **Params**: `{ "command": "...", "session_id": "..." }`
- **Response**: `{ "request_id": "...", "status": "pending" }`

### `exec.approval.resolve`

Resolve an approval request.

- **Scope**: Approvals
- **Params**: `{ "request_id": "...", "approved": true }`
- **Response**: `{ "status": "ok" }`

---

## Usage methods

### `usage.status`

Get token usage statistics.

- **Scope**: Read
- **Response**: `{ "total_tokens": 12345, "total_sessions": 10 }`

### `usage.cost`

Get estimated cost breakdown.

- **Scope**: Read
- **Params**: `{ "period": "day" }`
- **Response**: `{ "total_cost_usd": 1.23, "breakdown": [...] }`

---

## Logs methods

### `logs.tail`

Get recent log entries.

- **Scope**: Read
- **Params**: `{ "lines": 50, "level": "info" }`
- **Response**: `{ "logs": [...] }`

---

## Models methods

### `models.list`

List available models.

- **Scope**: Read
- **Response**: `{ "models": [...] }`

### `models.add`

Add a custom model definition.

- **Scope**: Write
- **Params**: `{ "id": "...", "provider": "...", "config": {...} }`
- **Response**: `{ "status": "ok" }`

### `models.update`

Update a model definition.

- **Scope**: Write
- **Params**: `{ "id": "...", "config": {...} }`
- **Response**: `{ "status": "ok" }`

### `models.delete`

Delete a custom model.

- **Scope**: Write
- **Params**: `{ "id": "..." }`
- **Response**: `{ "status": "ok" }`

### `models.setdefault`

Set the default model.

- **Scope**: Write
- **Params**: `{ "model": "gpt-4o" }`
- **Response**: `{ "status": "ok" }`

---

## Tools methods

### `tools.invoke`

Invoke a tool directly.

- **Scope**: Write
- **Params**: `{ "tool": "...", "input": {...} }`
- **Response**: `{ "output": {...} }`

---

## Memory methods

### `memory.list`

List memory entries across all layers.

- **Scope**: Read
- **Params**: `{ "layer": "global" }` (optional filter)
- **Response**: `{ "entries": [...] }`

### `memory.get`

Get a specific memory entry.

- **Scope**: Read
- **Params**: `{ "layer": "global", "slug": "coding-conventions" }`
- **Response**: `{ "entry": { "slug": "...", "layer": "...", "body": "...", "frontmatter": {...} } }`

### `memory.create`

Create a new memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "project", "slug": "api-notes", "body": "...", "frontmatter": {...} }`
- **Response**: `{ "status": "created", "slug": "api-notes" }`

### `memory.update`

Update an existing memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "global", "slug": "coding-conventions", "body": "...", "frontmatter": {...} }`
- **Response**: `{ "status": "ok" }`

### `memory.delete`

Delete a memory entry.

- **Scope**: Write
- **Params**: `{ "layer": "project", "slug": "old-notes" }`
- **Response**: `{ "status": "ok" }`

### `memory.search`

Search memory entries by text or tags.

- **Scope**: Read
- **Params**: `{ "query": "rust patterns", "layer": "global" }`
- **Response**: `{ "results": [...] }`

### `memory.promote`

Promote a session-layer entry to a persistent layer.

- **Scope**: Write
- **Params**: `{ "slug": "temp-finding", "target_layer": "project" }`
- **Response**: `{ "status": "ok" }`

### `memory.layers`

List configured memory layer directories.

- **Scope**: Read
- **Response**: `{ "layers": [{ "name": "global", "path": "..." }, ...] }`

---

## System methods

### `last-heartbeat`

Get the timestamp of the last heartbeat.

- **Scope**: Read
- **Response**: `{ "last_heartbeat_ms": 1700000000000 }`

### `set-heartbeats`

Configure heartbeat settings.

- **Scope**: Write
- **Params**: `{ "interval_ms": 30000 }`
- **Response**: `{ "status": "ok" }`

### `system-presence`

Report system presence status.

- **Scope**: Read
- **Params**: `{ "status": "active" }`
- **Response**: `{ "status": "ok" }`

### `system-event`

Send a system event.

- **Scope**: Write
- **Params**: `{ "event": "...", "data": {...} }`
- **Response**: `{ "status": "ok" }`

---

## Wizard methods

### `wizard.start`

Start an interactive setup wizard.

- **Scope**: Write
- **Params**: `{ "wizard_type": "..." }`
- **Response**: `{ "wizard_id": "...", "step": {...} }`

### `wizard.next`

Advance to the next wizard step.

- **Scope**: Write
- **Params**: `{ "wizard_id": "...", "answer": {...} }`
- **Response**: `{ "step": {...} }`

### `wizard.cancel`

Cancel an active wizard.

- **Scope**: Write
- **Params**: `{ "wizard_id": "..." }`
- **Response**: `{ "status": "cancelled" }`

### `wizard.status`

Get wizard status.

- **Scope**: Read
- **Response**: `{ "active_wizards": [...] }`

---

## Misc methods

### `browser.request`

Make a browser request.

- **Scope**: Read
- **Params**: `{ "url": "...", "method": "GET" }`
- **Response**: `{ "status": 200, "body": "..." }`

### `browser.extension.relay.start`

Bootstrap the in-page extension relay bridge on the selected tab/profile.

- **Scope**: Write
- **Params**: `{ "profile": "default", "channel": "default" }`
- **Response**: `{ "status": "ok", "channel": "...", "target_id": "..." }`

### `browser.extension.relay.status`

Read relay lifecycle state for the selected tab/profile.

- **Scope**: Read
- **Params**: `{ "profile": "default" }`
- **Response**: `{ "relay": { "started": true, "channel": "...", "queued": 0 } }`

### `browser.extension.relay.poll`

Drain queued relay messages from the selected tab/profile.

- **Scope**: Read
- **Params**: `{ "profile": "default" }`
- **Response**: `{ "messages": [...] }`

### `browser.extension.relay.send`

Dispatch an `savfox-relay` custom event into the selected tab/profile.

- **Scope**: Write
- **Params**: `{ "profile": "default", "channel": "default", "event_type": "message", "payload": {...} }`
- **Response**: `{ "status": "ok" }`

### `browser.extension.relay.stop`

Stop relay state and clear queued relay messages for the selected tab/profile.

- **Scope**: Write
- **Params**: `{ "profile": "default" }`
- **Response**: `{ "relay": { "stopped": true, ... } }`

### `browser.content_script.inject`

Evaluate a content-script string in the selected tab/profile.

- **Scope**: Write
- **Params**: `{ "profile": "default", "script": "return document.title;" }`
- **Response**: `{ "status": "ok", "result": ... }`

### `browser.page.extract`

Extract page data (text/headings/links/meta/html) from the selected tab/profile.

- **Scope**: Read
- **Params**: `{ "profile": "default", "interactive_only": false, "max_text_chars": 8000 }`
- **Response**: `{ "data": { "url": "...", "title": "...", ... } }`
- `interactive_only=true` returns an interaction-focused view (`interactive_elements`) for UI/automation flows.

### `talk.mode`

Set talk mode settings.

- **Scope**: Write
- **Params**: `{ "enabled": true, "language": "en" }`
- **Response**: `{ "status": "ok" }`

### `voicewake.get`

Get voice wake word settings.

- **Scope**: Read
- **Response**: `{ "enabled": false, "wake_word": "savfox" }`

### `voicewake.set`

Configure voice wake word.

- **Scope**: Write
- **Params**: `{ "enabled": true, "wake_word": "savfox" }`
- **Response**: `{ "status": "ok" }`

### `update.run`

Trigger a gateway update check.

- **Scope**: Write
- **Response**: `{ "status": "ok", "update_available": false }`
