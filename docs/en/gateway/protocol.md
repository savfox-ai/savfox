# WebSocket Protocol

The gateway WebSocket protocol uses typed control envelopes for authentication and
event streaming. Business requests use JSON-RPC 2.0 after authentication.
Connect to `ws://localhost:18881/ws` (or `wss://` with TLS).

## Message Types

Typed control messages are JSON objects with a `type` field. Authenticated bare
JSON-RPC requests instead carry a root `jsonrpc` field and need no `type` wrapper.
Root-object field order is insignificant: `jsonrpc` may follow a large or non-ASCII
`params` value. Nested keys or quoted text named `jsonrpc` do not select this route.
The inbound frame limit remains 1 MiB; normal RPC parsing, authorization, and
method validation still apply after classification.

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
For production clients, set `token` to `HMAC-SHA256(nonce, token)` encoded as
lowercase hex. Raw bearer tokens are still accepted for backwards
compatibility, but they should be avoided by browser and remote clients.

```json
{
  "type": "connect",
  "token": "hmac-sha256-nonce-signature-hex",
  "client_info": { "name": "my-app", "version": "1.0.0" },
  "min_protocol": 1,
  "max_protocol": 1,
  "role": "operator",
  "scopes": ["chat", "config"]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `token` | string | Yes | HMAC signature over the challenge nonce, or a legacy raw bearer token |
| `client_info` | object | No | Client name and version |
| `min_protocol` | number | No | Minimum protocol version supported |
| `max_protocol` | number | No | Maximum protocol version supported |
| `role` | string | No | `"operator"` or `"node"` |
| `scopes` | array | No | Requested permission scopes |

Legacy clients may pass the token as a query parameter:
`ws://localhost:18881/ws?token=my-secret-token`. This skips the
`Connect` / `ConnectChallenge` handshake and can expose the token in browser
history, access logs, proxies, and telemetry. Do not use query-string tokens for
production browser or remote clients.

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

### JSON-RPC Request (Client to Server)

Send requests directly with `jsonrpc: "2.0"`:

```json
{"params":{"message":"Hello, agent"},"method":"chat.send","id":"req-1","jsonrpc":"2.0"}
```

Requests receive JSON-RPC responses with `jsonrpc`, `id`, and `result` or `error`.
The `jsonrpc` value must be exactly `"2.0"`.

### JSON-RPC Response (Server to Client)

The result of a Request.

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
  "result": { "message": "Hello! How can I help?" }
}
```

Error response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
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

### Event Subscriptions

Use JSON-RPC for event subscription requests:

```json
{ "jsonrpc": "2.0", "id": 2, "method": "events.subscribe", "params": { "events": ["agent.*"] } }
{ "jsonrpc": "2.0", "id": 3, "method": "events.unsubscribe", "params": { "events": ["agent.*"] } }
```

Subscriptions are currently advisory; clients must also filter received event
names. Request recent server logs with `logs.tail`:

```json
{ "jsonrpc": "2.0", "id": 4, "method": "logs.tail", "params": {} }
```

### Execution Approvals

Respond to an execution approval request:

```json
{ "jsonrpc": "2.0", "id": 5, "method": "exec.approval.resolve", "params": { "request_id": "approval-123", "nonce": "single-use-nonce", "decision": "approve-once" } }
```

The authenticated client must have approval-resolution permission and satisfy
the approval's owner and single-use nonce checks.

### Removed Request Envelopes

The old `type: "request"`, `type: "response"`, and `type: "approvalResponse"`
envelopes are not supported. They receive a control error without dispatching a
method or resolving an approval. There is no fallback to app-server protocol
methods. Use the gateway's JSON-RPC methods for configuration, account login,
sessions and approvals. Authentication, event and ping/pong controls retain their
typed envelopes.

### Error

Generic error envelope:

```json
{ "type": "error", "code": 401, "message": "invalid token" }
```

## JSON-RPC Methods

After authentication, use JSON-RPC 2.0 requests to call the gateway methods:

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
5. Client sends JSON-RPC 2.0 requests; server replies with JSON-RPC results/errors and pushes typed `Event` messages.
6. Either side may close the connection at any time.

## Protocol Version

The current protocol version is `1`. Version negotiation occurs during the `Connect` / `Connected` handshake via `min_protocol` and `max_protocol` fields.

All business requests pass through the JSON-RPC dispatcher's method-scope and
session-ownership checks. The gateway does not expose the app-server request
dispatcher over WebSocket.
