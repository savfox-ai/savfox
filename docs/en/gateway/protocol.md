# WebSocket Protocol

The gateway WebSocket protocol wraps JSON-RPC 2.0 messages in a typed envelope for authentication, event streaming, and subscription management. Connect to `ws://localhost:18881/ws` (or `wss://` with TLS).

## Message Types

All messages are JSON objects with a `type` field that determines the envelope format.

### ConnectChallenge (Server to Client)

Sent immediately after WebSocket upgrade. The client must authenticate before any other communication.

```json
{
  "type": "connectChallenge",
  "nonce": "a1b2c3d4e5f6",
  "ts": 1700000000000
}
```

| Field | Type | Description |
|-------|------|-------------|
| `nonce` | string | Random nonce for replay protection |
| `ts` | number | Server timestamp in milliseconds since epoch |

### Connect (Client to Server)

Client authentication message. Must be the first message sent by the client.

```json
{
  "type": "connect",
  "token": "my-secret-token",
  "client_info": { "name": "my-app", "version": "1.0.0" },
  "min_protocol": 1,
  "max_protocol": 1,
  "role": "operator",
  "scopes": ["chat", "config"]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | Yes | Authentication bearer token |
| `client_info` | object | No | Client name and version |
| `min_protocol` | number | No | Minimum protocol version supported |
| `max_protocol` | number | No | Maximum protocol version supported |
| `role` | string | No | `"operator"` or `"node"` |
| `scopes` | array | No | Requested permission scopes |

Alternatively, pass the token as a query parameter: `ws://localhost:18881/ws?token=my-secret-token`. This skips the `Connect` / `ConnectChallenge` handshake.

### Connected (Server to Client)

Sent after successful authentication.

```json
{
  "type": "connected",
  "session_id": "01234567-89ab-cdef-0123-456789abcdef",
  "server_version": "0.1.0",
  "protocol": 1,
  "policy": { "tick_interval_ms": 1000 }
}
```

### Request (Client to Server)

A JSON-RPC request wrapped in the gateway envelope.

```json
{
  "type": "request",
  "id": "req-1",
  "method": "chat.send",
  "params": { "message": "Hello, agent" }
}
```

### Response (Server to Client)

The result of a Request.

```json
{
  "type": "response",
  "id": "req-1",
  "ok": true,
  "payload": { "message": "Hello! How can I help?" }
}
```

Error response:

```json
{
  "type": "response",
  "id": "req-1",
  "ok": false,
  "error": { "code": -32601, "message": "method not found" }
}
```

### Event (Server to Client)

Server-push notification. Events are not tied to a specific request.

```json
{
  "type": "event",
  "event": "agent.message",
  "payload": { "text": "Working on it..." },
  "seq": 42
}
```

The `seq` field is an optional monotonic sequence number for ordering.

### Subscribe / Unsubscribe

Subscribe to events for a specific thread:

```json
{ "type": "subscribe", "thread_id": "thread-abc" }
{ "type": "unsubscribe", "thread_id": "thread-abc" }
```

### SubscribeLogs / UnsubscribeLogs

Stream server logs in real time:

```json
{ "type": "subscribeLogs", "level": "info" }
{ "type": "unsubscribeLogs" }
```

### ApprovalResponse

Respond to an execution approval request:

```json
{ "type": "approvalResponse", "request_id": "approval-123", "approved": true }
```

### Error

Generic error envelope:

```json
{ "type": "error", "code": 401, "message": "invalid token" }
```

## JSON-RPC Methods

After authentication, use `Request` messages to call any of the 90+ RPC methods:

| Group | Methods |
|-------|---------|
| Agent | `agent`, `agent.identity`, `agent.wait` |
| Chat | `chat.send`, `chat.history`, `chat.abort` |
| Sessions | `sessions.list`, `sessions.preview`, `sessions.patch`, `sessions.reset`, `sessions.delete` |
| Config | `config.get`, `config.set`, `config.apply`, `config.patch` |
| Cron | `cron.list`, `cron.add`, `cron.update`, `cron.remove`, `cron.run`, `cron.runs` |
| Models | `models.list` |
| Memory | `memory.list`, `memory.get`, `memory.create`, `memory.update`, `memory.delete`, `memory.search`, `memory.promote`, `memory.layers` |
| System | `connect`, `health`, `status`, `wake`, `send` |

## Connection Lifecycle

1. Client opens WebSocket to `/ws`.
2. Server sends `ConnectChallenge`.
3. Client sends `Connect` with token.
4. Server validates and sends `Connected` with session ID.
5. Client sends `Request` messages; server replies with `Response` and pushes `Event` messages.
6. Either side may close the connection at any time.

## Protocol Version

The current protocol version is `1`. Version negotiation occurs during the `Connect` / `Connected` handshake via `min_protocol` and `max_protocol` fields.
