# Gateway Architecture

The Savfox gateway server is a persistent, multi-protocol hub that exposes the
Savfox AI engine over WebSocket, REST, and chat-platform webhooks. It runs as
a long-lived process and manages sessions, channels, and background services on
behalf of connected clients.

## High-level component map

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
           Channel Runtime         Web UI (SPA)
           (Discord, Telegram,    rust-embed WASM
            Slack, Matrix, ...)   at /web/dist/
```

## Salvo HTTP framework

The gateway uses Salvo v0.89 for HTTP routing, WebSocket upgrade, and static
file serving. TLS is supported via PEM certificate and key paths in
`config.toml`.

## GatewayChannel

`GatewayChannel` is the central message router (`crates/gateway-server/src/channel.rs`).
It owns:

- **ThreadManager** -- creates, resumes, and controls agent threads.
- **Config** -- the resolved Savfox configuration.
- **ConfigService** -- live config read/write operations.
- **AuthManager** -- provider API key management.
- **GatewaySessionManager** -- tracks connected WebSocket clients.
- **RuntimeBridgeSecrets** -- hot-reloadable platform credentials (Discord,
  Telegram, Slack, webhook tokens).
- **HTTP client** -- for outbound platform API calls.

## Authentication

`GatewayAuth` (`auth.rs`) manages bearer tokens. Each token carries a label and
one or more `TokenScope` values.

| TokenScope         | Description                                  |
|--------------------|----------------------------------------------|
| `Operator`         | Full access (implies all sub-scopes)         |
| `Viewer`           | Read-only (implies `OperatorRead`)           |
| `Chat`             | Chat-channel-only operations                  |
| `OperatorRead`     | Read sessions, config, logs, usage           |
| `OperatorWrite`    | Start threads, send messages, modify config  |
| `OperatorAdmin`    | Token management, plugin control             |
| `OperatorApprovals`| Manage execution approvals                   |
| `OperatorPairing`  | Pair/manage devices and nodes                |

Authentication supports both raw bearer tokens and HMAC-SHA256
challenge-response for replay protection.

## WebSocket lifecycle

1. **Upgrade** -- Client connects to `GET /ws`. Optionally passes `?token=`.
2. **Challenge** -- Server sends `ConnectChallenge` (nonce + timestamp).
3. **Authenticate** -- Client replies with `Connect`. Token may be raw or
   HMAC-SHA256 signed.
4. **Connected** -- Server sends `Connected` with session ID and protocol
   version.
5. **Message loop** -- Bidirectional JSON frames (JSON-RPC requests/responses
   and server-push `Event` messages).
6. **Disconnection** -- Session is removed and subscriptions are cleaned up.

## JSON-RPC dispatch

Messages with a `"jsonrpc"` field are dispatched to the WS-RPC handler
(`ws_rpc.rs`), which routes 130+ methods organized by domain. Each method is
mapped to a required permission scope via `required_scope()` in `auth.rs`.

## REST API endpoints

| Endpoint                    | Method | Description                        |
|-----------------------------|--------|------------------------------------|
| `/health`                   | GET    | Health check                       |
| `/api/status`               | GET    | Connected client count             |
| `/api/token/validate`       | POST   | Validate a bearer token            |
| `/api/sessions`             | GET    | List active sessions               |
| `/api/agent`                | POST   | Invoke agent with text prompt      |
| `/v1/models`                | GET    | OpenAI-compatible models list      |
| `/v1/chat/completions`      | POST   | OpenAI-compatible chat completions |
| `/v1/responses`             | POST   | OpenResponses API                  |
| `/ws`                       | GET    | WebSocket upgrade                  |

## Background services

- **CronService** -- timer-based job scheduling with cron expressions, intervals,
  and one-shot timestamps. Payloads can inject system events or trigger agent
  turns. Failed jobs use exponential backoff.
- **CompactionService** -- manages context window size by summarizing older
  messages when the token count approaches a configurable threshold.
- **Session pruning** -- background timer (every 5 minutes) prunes stale entries
  from the persistent session store.

## Web UI

The gateway embeds a Dioxus 0.7 WASM single-page application via `rust-embed`.
Assets are served from `web/dist/`, with SPA fallback routing for unmatched GET
requests.

## Deployment options

```bash
# Foreground
savfox gateway --port 18881 --token <token>

# Daemon
savfox gateway start --port 18881
savfox gateway stop
savfox gateway restart

# System service (systemd or launchd)
savfox gateway install --name savfox-gateway
```
