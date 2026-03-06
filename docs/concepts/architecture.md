# Gateway Architecture

The Savfox gateway server is a persistent, multi-protocol hub that exposes the
Savfox AI engine over WebSocket, REST, and chat-platform webhooks. It runs as a
long-lived process (foreground or daemon) and manages sessions, bridges, and
background services on behalf of connected clients.

## High-level components

```
                     +--------------------+
  WebSocket (/ws) -->|                    |--> ThreadManager (Savfox core)
  REST (/api/*)   -->|   Gateway Server   |--> SessionStore (JSON persistence)
  Webhooks (/wh*) -->|     (Salvo)        |--> CronService (scheduled jobs)
  OpenAI API (/v1)->|                    |--> Auth (token + scope enforcement)
                     +--------------------+
                             |
                  +----------+----------+
                  |                     |
           Bridge Runtime         Web UI (SPA)
           (Discord, Telegram,    rust-embed WASM
            Slack, Matrix, ...)   at /web/dist/
```

### Salvo HTTP framework

The gateway uses [Salvo](https://salvo.rs/) v0.89 for HTTP routing, WebSocket
upgrade, and static file serving. TLS is supported via PEM certificate and key
paths.

### GatewayBridge

`GatewayBridge` (`crates/gateway-server/src/bridge.rs`) is the central message
router. It owns:

- An `Arc<ThreadManager>` for creating, resuming, and controlling agent threads.
- An `Arc<Config>` for the resolved Savfox configuration.
- A `ConfigService` for live config read/write operations.
- An `AuthManager` for provider API key management.
- A `GatewaySessionManager` that tracks connected WebSocket clients.
- Platform-specific HTTP helpers for sending messages to Discord, Telegram,
  Slack, Matrix, Mattermost, Google Chat, LINE, Feishu, IRC, Zalo, and generic
  webhooks.

### GatewayAuth

`GatewayAuth` (`crates/gateway-server/src/auth.rs`) manages bearer tokens. Each
token carries a label and a list of `TokenScope` values:

| TokenScope         | Description                                  |
|--------------------|----------------------------------------------|
| `Operator`         | Full access (implies all sub-scopes)         |
| `Viewer`           | Read-only (implies `OperatorRead`)           |
| `Chat`             | Chat-bridge-only operations                  |
| `OperatorRead`     | Read sessions, config, logs, usage           |
| `OperatorWrite`    | Start threads, send messages, modify config  |
| `OperatorAdmin`    | Token management, plugin control             |
| `OperatorApprovals`| Manage execution approvals                   |
| `OperatorPairing`  | Pair/manage devices and nodes                |

Token validation supports both raw bearer tokens and HMAC-SHA256
challenge-response authentication for replay protection.

## WebSocket lifecycle

1. **Upgrade** -- Client connects to `GET /ws`. Optionally passes `?token=...`.
2. **Challenge** -- If no query token, the server sends a `ConnectChallenge`
   message containing a random nonce and timestamp.
3. **Authenticate** -- Client replies with a `Connect` message. The `token`
   field may be a raw bearer token (backwards compatible) or an
   `HMAC-SHA256(nonce, token)` signature.
4. **Connected** -- Server sends a `Connected` acknowledgement with the session
   ID, server version, and negotiated protocol version.
5. **Message loop** -- Bidirectional JSON messages flow:
   - **Client to server**: `Request` (JSON-RPC), `Subscribe`, `Unsubscribe`,
     `SubscribeLogs`, `ApprovalResponse`, `Ping`.
   - **Server to client**: `Response`, `Event`, `Error`, `Pong`.
6. **Disconnection** -- On close, the session is removed and subscriptions are
   cleaned up.

### JSON-RPC dispatch

Messages with a `"jsonrpc"` field are dispatched to the WS-RPC handler
(`ws_rpc.rs`), which routes 96+ methods organized by domain. All other messages
are dispatched as `GatewayMessage` variants (the legacy protocol envelope).

## Bridge lifecycle

A "bridge" connects a chat platform to the gateway. Each bridge:

1. Receives inbound webhooks at `/webhooks/<platform>`.
2. Parses the platform-specific payload into a `ChannelAction`:
   - `StartThread` -- create a new agent thread with the user's prompt.
   - `SendToThread` -- route a message to an existing thread.
   - `Approve` -- respond to an exec approval request.
   - `Ignore` -- no action needed (e.g., a verification ping).
3. Invokes the agent via `GatewayBridge::invoke_agent_text()`.
4. Sends the agent's reply back to the originating channel via the platform API.

Bridge credentials can be configured via `config.toml` under `[gateway.bridges]`
or set at runtime through the `config.patch` RPC method.

## Session management

### In-memory sessions (GatewaySessionManager)

`GatewaySessionManager` tracks WebSocket clients. Each `ClientSession` has:

- A unique session ID (UUID v7).
- Token information (scopes).
- A per-client outgoing message channel (mpsc, capacity 256).
- Thread subscriptions (which thread events to forward).
- Log subscriptions (for real-time log streaming).

### Persistent sessions (SessionStore)

`SessionStore` (`crates/gateway-server/src/session_store.rs`) persists session
metadata as JSON at `{savfox_home}/sessions/sessions.json`. Features:

- **TTL cache**: In-memory cache with 45-second TTL, refreshed on file
  modification time change.
- **Pruning**: Entries older than 30 days or exceeding 500 total are removed
  automatically every 5 minutes.
- **Rotation**: When the store file exceeds 10 MB, numbered backups are created
  (up to 3).
- **Session keys**: Built from agent ID, channel, group ID, thread ID, and peer
  ID using the `DmScope` enum (`Main`, `PerPeer`, `PerChannelPeer`).
- **Reset policies**: `Never`, `Daily { hour }`, or `Idle { timeout_secs }`.

### Session overrides

Each session can carry per-session overrides for:

- `model` -- override the default model.
- `thinking` -- thinking budget (`off`, `low`, `medium`, `high`).
- `verbose` -- verbosity level (`off`, `on`, `full`).
- `reasoning` -- reasoning mode (`off`, `on`, `stream`).

## Background services

### Cron service

`CronService` (`crates/gateway-server/src/cron_service.rs`) runs scheduled jobs
with JSON persistence. Schedule types include fixed time (`at`), interval
(`every`), and cron expressions (`cron`). Payloads can be `systemEvent` or
`agentTurn`. Failed jobs use exponential backoff, and run history is stored as
JSONL.

### Context compaction

`CompactionService` (`crates/gateway-server/src/compaction.rs`) manages context
window size. When the token count approaches a configurable threshold, older
messages are summarized while pinned messages and tool results are preserved.
Modes: `Auto`, `Manual`, `Disabled`.

### Session pruning

A background timer runs every 5 minutes to prune stale session entries from the
persistent store.

## REST API endpoints

| Endpoint                    | Method | Description                        |
|-----------------------------|--------|------------------------------------|
| `/health`                   | GET    | Health check                       |
| `/api/status`               | GET    | Connected client count             |
| `/api/logs`                 | GET    | In-memory gateway logs             |
| `/api/config`               | GET    | Sanitized gateway configuration    |
| `/api/config/patch`         | POST   | Merge-patch configuration          |
| `/api/config/apply`         | POST   | Replace configuration              |
| `/api/token/validate`       | POST   | Validate a bearer token            |
| `/api/message`              | POST   | Send message to a chat channel     |
| `/api/sessions`             | GET    | List active sessions               |
| `/api/channels`             | GET    | List configured channel bridges    |
| `/api/nodes`                | GET    | List known nodes                   |
| `/api/devices`              | GET    | List paired devices                |
| `/api/devices/pair`         | POST   | Create device pairing request      |
| `/api/agent`                | POST   | Invoke agent with text prompt      |
| `/api/agent/wait`           | POST   | Wait for agent run completion      |
| `/api/restart`              | POST   | Request gateway restart            |
| `/v1/models`                | GET    | OpenAI-compatible models list      |
| `/v1/chat/completions`      | POST   | OpenAI-compatible chat completions |
| `/v1/responses`             | POST   | OpenResponses API                  |
| `/tools/invoke`             | POST   | Invoke a tool directly             |
| `/ws`                       | GET    | WebSocket upgrade                  |

## Web UI

The gateway embeds a Dioxus 0.7 WASM single-page application via `rust-embed`.
Static assets are served from `web/dist/`, and all unmatched GET requests fall
through to the SPA handler for client-side routing.

## Deployment

### Foreground

```bash
savfox gateway --port 18881 --token <token>
```

### Daemon

```bash
savfox gateway start --port 18881
savfox gateway stop
savfox gateway restart --port 18881
```

### System service

```bash
savfox gateway install --name savfox-gateway
savfox gateway uninstall --name savfox-gateway
```

This installs a systemd unit (Linux) or launchd plist (macOS).
