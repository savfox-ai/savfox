# REST API Reference

The gateway exposes a REST API for health checks, configuration, session management, agent invocation, and more. All endpoints (except `/health`) require a bearer token.

## Authentication

Include the token in the `Authorization` header:

```
Authorization: Bearer <token>
```

## Endpoints

### Health

```
GET /health
```

Returns `200 OK` with no authentication required. Use this for load balancer health checks.

```bash
curl http://localhost:18881/health
```

### Status

```
GET /api/status
```

Returns server status including version, uptime, active sessions, and connected clients.

```bash
curl http://localhost:18881/api/status -H "Authorization: Bearer <token>"
```

### Configuration

```
GET /api/config
```

Returns the current gateway configuration (sensitive fields redacted).

```
POST /api/config/patch
```

Apply a partial configuration update. Body is a JSON object with the fields to change:

```bash
curl -X POST http://localhost:18881/api/config/patch \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"gateway": {"port": 9090}}'
```

```
POST /api/config/apply
```

Apply configuration changes that require a restart (e.g., port or TLS changes). The server reloads affected subsystems.

### Token Validation

```
POST /api/token/validate
```

Validate an authentication token and return its scopes:

```bash
curl -X POST http://localhost:18881/api/token/validate \
  -H "Authorization: Bearer <token>"
```

Response:

```json
{ "valid": true, "scopes": ["operator"] }
```

### Messages

```
POST /api/message
```

Send a message to the agent. The response is the agent's reply:

```bash
curl -X POST http://localhost:18881/api/message \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"message": "What files are in the src directory?"}'
```

### Sessions

```
GET /api/sessions
```

List all sessions with metadata (ID, key, timestamps, token usage).

```
GET /api/sessions/<session_id>/history
```

Return the full conversation history for a session.

```
POST /api/sessions/<session_id>
```

Update session metadata (model, label, etc.).

```
POST /api/sessions/<session_id>/reset
```

Reset a session's conversation history.

```
DELETE /api/sessions/<session_id>
```

Delete a session.

```
POST /api/sessions/<session_id>/compact
```

Trigger context compaction for a session.

### Agent Invocation

```
POST /api/agent
```

Start an agent run asynchronously. Returns a `run_id`:

```bash
curl -X POST http://localhost:18881/api/agent \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "List all TODO comments"}'
```

```
POST /api/agent/wait
```

Start an agent run and wait for completion. The response includes the agent's full output.

### Execution Approvals

```
GET /api/exec/approvals
```

List pending approval requests.

```
POST /api/exec/approval/request
```

Submit an approval request (used internally by the agent).

```
POST /api/exec/approval/resolve
```

Approve or deny a pending request:

```bash
curl -X POST http://localhost:18881/api/exec/approval/resolve \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"request_id": "abc123", "approved": true}'
```

### Channels and Nodes

```
GET /api/channels
```

List configured chat bridge channels and their status.

```
GET /api/nodes
```

List connected nodes in a multi-node setup.

### Devices

```
GET /api/devices
```

List paired devices.

```
POST /api/devices/pair
```

Generate a pairing token for a new device.

```
POST /api/devices/<device_id>/revoke
```

Revoke access for a paired device.

### OpenAI-Compatible API

```
POST /v1/chat/completions
```

OpenAI-compatible chat completions endpoint. Allows the gateway to act as a drop-in replacement for the OpenAI API:

```bash
curl -X POST http://localhost:18881/v1/chat/completions \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4o", "messages": [{"role": "user", "content": "Hello"}]}'
```

```
POST /v1/responses
```

OpenResponses API endpoint for streaming responses.

### Tools

```
POST /tools/invoke
```

Invoke a tool directly (shell, file search, etc.).

### Logs

```
GET /api/logs
```

Retrieve recent server log entries.

### Server Management

```
POST /api/restart
```

Restart the gateway server.
