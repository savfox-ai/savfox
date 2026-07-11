# Gateway Server

The gateway server provides remote HTTP/WebSocket access to Savfox, enabling web clients, chat platform integrations, and multi-device usage.

## Starting the Gateway

```bash
savfox gateway
savfox gateway --port 8080
savfox gateway --port 8080 --token my-secret-token
savfox gateway --host 0.0.0.0 --port 443 --tls-cert cert.pem --tls-key key.pem
```

**Server options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--host <ADDR>` | `127.0.0.1` | Bind address |
| `--port <PORT>` | `18881` | Listen port |
| `--token <TOKEN>` | Auto-generated | Bearer token for authentication |
| `--tls-cert <PATH>` | — | TLS certificate for HTTPS/WSS |
| `--tls-key <PATH>` | — | TLS private key |

If `--token` is omitted, the gateway reuses the valid token stored at
`<savfox-home>/gateway/token`. On the first run, or if that file is invalid, it
generates and persists a new token. Startup output shows only its fingerprint
and the token file path.

## Authentication

All API and WebSocket requests require a bearer token:

```
Authorization: Bearer <token>
```

Tokens support three scopes:
- **Operator** — Full access to all APIs and agent execution
- **Viewer** — Read-only access to status, sessions, and logs
- **Chat** — Send and receive chat messages only

Validate a token:

```bash
curl -X POST http://localhost:18881/api/token/validate \
  -H "Authorization: Bearer <token>"
```

## REST API Endpoints

| Path | Method | Description |
|------|--------|-------------|
| `/health` | GET | Health check |
| `/api/status` | GET | Server status and info |
| `/api/config` | GET | Current configuration |
| `/api/config/patch` | POST | Patch configuration |
| `/api/config/apply` | POST | Apply configuration changes |
| `/api/message` | POST | Send a message to the agent |
| `/api/sessions` | GET | List sessions |
| `/api/sessions/<id>/history` | GET | Session conversation history |
| `/api/restart` | POST | Restart the server |
| `/api/agent` | POST | Invoke the agent |
| `/api/agent/wait` | POST | Invoke and wait for completion |
| `/api/exec/approval/*` | GET/POST | Execution approval management |
| `/v1/chat/completions` | POST | OpenAI-compatible chat API |
| `/v1/responses` | POST | OpenResponses API |
| `/tools/invoke` | POST | Tool invocation endpoint |

## WebSocket JSON-RPC

Connect to the WebSocket endpoint for real-time bidirectional communication:

```
ws://localhost:18881/ws
```

The protocol uses JSON-RPC 2.0. Example request:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "status",
  "params": {}
}
```

Key method groups:
- **Agent** — `agent`, `agent.identity`, `agent.wait`
- **Chat** — `chat.send`, `chat.history`, `chat.abort`
- **Sessions** — `sessions.list`, `sessions.preview`, `sessions.patch`, `sessions.reset`, `sessions.delete`
- **Config** — `config.get`, `config.set`, `config.apply`, `config.patch`
- **Cron** — `cron.list`, `cron.add`, `cron.update`, `cron.remove`, `cron.run`, `cron.runs`
- **Models** — `models.list`
- **System** — `connect`, `health`, `status`, `wake`, `send`

## Management Subcommands

### Status

```bash
savfox gateway status
```

### Logs

```bash
savfox gateway logs              # recent logs
savfox gateway logs --follow     # stream logs in real-time
savfox gateway logs --lines 100  # last 100 lines
```

### Models

```bash
savfox gateway models
```

### Approvals

```bash
savfox gateway approvals list
savfox gateway approvals approve <ID>
savfox gateway approvals deny <ID> --reason "Not safe"
```

### Devices

```bash
savfox gateway devices list
savfox gateway devices pair --name "My Phone"
savfox gateway devices revoke <ID>
```

### Channels

Manage chat channel channel integrations:

```bash
savfox gateway channels
```

### Nodes

Manage connected nodes in a multi-node setup:

```bash
savfox gateway nodes
```

## Chat Platform Bridges

The gateway can channel conversations to external chat platforms. Configure channels in your `config.toml`:

```toml
[gateway.channels.discord]
enabled = true
bot_token = "your-discord-bot-token"

[gateway.channels.telegram]
enabled = true
bot_token = "your-telegram-bot-token"
webhook_secret_token = "optional-telegram-secret-token"

[gateway.channels.slack]
enabled = true
bot_token = "xoxb-your-slack-bot-token"
signing_secret = "your-signing-secret"

[gateway.channels.webhook]
enabled = true
secret = "shared-webhook-hmac-secret"
```

**Supported platforms:**

| Platform | Config Key | Required Fields |
|----------|-----------|-----------------|
| Discord | `channels.discord` | `bot_token` |
| Telegram | `channels.telegram` | `bot_token` |
| Slack | `channels.slack` | `bot_token`, `signing_secret` |
| Matrix | `channels.matrix` | Server URL, credentials |
| Mattermost | `channels.mattermost` | Server URL, token |
| Google Chat | `channels.google_chat` | Service account |
| Line | `channels.line` | Channel token |
| Feishu | `channels.feishu` | App credentials |
| IRC | `channels.irc` | Server, channel, nick |
| Webhook | `channels.webhook` | URL/`secret` (recommended) |

Each channel routes messages bidirectionally between the chat platform and the Savfox agent.

### Webhook Security

When security config exists, the gateway enforces request verification:

- Discord: `x-signature-ed25519` + `x-signature-timestamp`
- Slack: `x-slack-signature` + `x-slack-request-timestamp` (5-minute replay window)
- Telegram: `x-telegram-bot-api-secret-token`
- Generic webhook: `x-signature` or `x-hub-signature-256` (HMAC-SHA256)

Common failure responses:

- `401 invalid_signature` — signature/token mismatch
- `401 missing_signature` — required signature headers are absent
- `401 stale_signature` — Slack timestamp outside the replay window

## Background Services

The gateway runs several background services:

- **Session pruning** — Removes expired sessions every 5 minutes
- **Cron scheduler** — Executes scheduled tasks (checks every 60 seconds)

### Cron Service

Schedule recurring tasks via the WebSocket API:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "cron.add",
  "params": {
    "name": "daily-report",
    "schedule": { "type": "cron", "expr": "0 9 * * *" },
    "payload": { "type": "agentTurn", "prompt": "Generate the daily status report" }
  }
}
```

Schedule types:
- `at` — One-shot execution at a specific time
- `every` — Repeat at a fixed interval
- `cron` — Standard cron expression

## Rate Limiting

The gateway includes a token-bucket rate limiter to prevent abuse. Rate limits are applied per-token and can be configured as needed.
