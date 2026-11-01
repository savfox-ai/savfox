# Gateway Configuration Reference

The Savfox gateway server is configured through a combination of CLI flags,
the `[gateway]` section in `config.toml`, and environment variables for channel
credentials.

## Configuration sources (precedence order)

1. **CLI flags** (highest) -- `savfox gateway --port 8080 --token abc`
2. **config.toml `[gateway]` section** -- persistent file configuration
3. **Environment variables** -- for channel credentials and secrets
4. **Defaults** (lowest) -- built-in defaults

## CLI flags

```
savfox gateway [OPTIONS] [SUBCOMMAND]
```

| Flag          | Type     | Default        | Description                              |
|---------------|----------|----------------|------------------------------------------|
| `--host`      | IP addr  | `127.0.0.1`    | Host address to bind to                  |
| `--port`      | u16      | `18881`        | Port to listen on                        |
| `--token`     | String   | (persisted/generated) | Bearer token for authentication    |
| `--tls-cert`  | Path     | None           | TLS certificate path (PEM)               |
| `--tls-key`   | Path     | None           | TLS private key path (PEM)               |

Use `--host 0.0.0.0` to listen on all interfaces.

## System configuration and management pages

System configuration (`/config`) starts with a configuration overview, followed by
Gateway and security, Interface and notifications, Execution and tools, Sessions and context, Runtime environment,
and Advanced configuration. Search matches field names, translated labels and descriptions.
The schema comes from the runtime's `ConfigToml` and `GatewayConfig` types, not a
separate list of hypothetical settings. Unsupported old fields are excluded and
removed on the next save; they are not migrated to a second configuration location.

Models/accounts, agents, channels, skills and scheduled jobs have dedicated management
pages and service-owned stores. Channel credentials belong in the Channels page and
`channels/*.json`; the old `[gateway.channels]` and `[gateway.bridges]` inputs are removed.
Speech synthesis and talk/wake settings share `/speech`; nodes, live WebSocket clients
and pairing share `/devices`. Browser options are grouped within System configuration:
theme, language and desktop notifications under Interface and notifications, the WebSocket
address under Gateway and security, and the default model for new chats under Sessions
and context. There is no separate preferences page or `/settings` route.
Server logging controls are on the Logs page. Advanced configuration links to manual RPC
and diagnostics instead of placing another item in the primary navigation.

```toml
[gateway]
host = "127.0.0.1"
port = 18881
# token is persisted/generated when omitted
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"
trust_x_forwarded_for = false

[gateway.rate_limit]
max_requests = 100
window_secs = 60
max_connections_per_ip = 10

[gateway.response_footer]
enabled = true
max_length = 240
```

Explicit CLI listener/credential arguments take precedence over file values. Omitted
arguments use file values, then defaults. Configure both TLS paths together.

## Environment variables

Channel credentials can also be set via environment variables. These are used as
fallbacks when the saved channel credentials are not present:

| Variable                      | Channel      | Description                    |
|-------------------------------|-------------|--------------------------------|
| `DISCORD_BOT_TOKEN`          | Discord     | Bot token                      |
| `TELEGRAM_BOT_TOKEN`         | Telegram    | Bot API token                  |
| `SLACK_BOT_TOKEN`            | Slack       | Bot OAuth token                |
| `MATRIX_HOMESERVER`          | Matrix      | Homeserver URL (default: `https://matrix.org`) |
| `MATRIX_ACCESS_TOKEN`        | Matrix      | Access token                   |
| `MATTERMOST_URL`             | Mattermost  | Server URL (default: `http://localhost:8065`)  |
| `MATTERMOST_TOKEN`           | Mattermost  | Access token                   |
| `GOOGLECHAT_WEBHOOK_URL`     | Google Chat | Webhook URL                    |
| `TEAMS_WEBHOOK_URL`          | MS Teams    | Webhook URL                    |
| `LINE_CHANNEL_TOKEN`         | LINE        | Channel access token           |
| `FEISHU_APP_ACCESS_TOKEN`    | Feishu/Lark | App access token               |
| `IRC_BRIDGE_URL`             | IRC         | Channel HTTP URL (default: `http://127.0.0.1:6667`) |
| `ZALO_OA_ACCESS_TOKEN`       | Zalo        | OA access token                |

## Saving and activation

Optional settings distinguish inherited defaults from explicit enabled/disabled values. An absent option is not displayed as disabled.

The editor uses one Save configuration action for server and browser drafts.
Browser options are explicitly marked as affecting only this browser and apply when saved.
Saving only browser options does not write server configuration or require a gateway restart.
Discard, reload, search and the change review cover both scopes. Browser options are excluded
from the advanced server JSON document and its export.

The editor maintains one server draft. JSON edits update that draft directly; invalid JSON
blocks saving. Discard restores the loaded document. Reload asks before discarding
unsaved changes. The advanced JSON editor also serves as the import surface: paste
a complete document, review the changes, and save once. Removing a field in this
document removes it from the saved configuration.

`config.get` returns `config`, `revision` and `removed_fields`. Save with:

```json
{"jsonrpc":"2.0","id":1,"method":"config.set","params":{"config":{"gateway":{"port":18881}},"revision":"revision-from-config.get"}}
```

Writes validate supported fields and types, reject stale revisions, preserve the
previous file in `config.toml.bak`, and atomically replace `config.toml` once.
The response contains `status`, `config`, `revision` and `restart_required`.
Experimental feature flags and low-frequency core settings remain available in the advanced document instead of expanding into hundreds of default controls.

A successful write returns `saved` and `restart_required: true`; an unchanged
document returns `unchanged` and does not rewrite the file. Restart the gateway to
activate system configuration. Neither `config.apply` nor `config.reload` pretends
to hot-reconfigure the running server. Service-specific actions, such as changing
the current logging level, use their own APIs.

`config.patch` takes `{ "patch": { ... }, "revision": "..." }` and uses JSON merge
patch semantics (`null` deletes). HTTP `POST /api/config/patch` takes `config` as its
patch body; HTTP `POST /api/config/apply` takes a full `config` document. Both share
the same validation, backup, revision and atomic-write path as WebSocket saves.

Export downloads the saved, redacted system configuration through `config.export`.
It does not export unsaved edits or make a complete backup of accounts, channels,
speech settings, jobs, sessions or secrets.

`system.connections` is a read-only list of live clients with `session_id`, `name`,
`connected_at` and `is_current`. `system.disconnect` requires administrator scope and
`{ "session_id": "..." }`; it notifies and closes that WebSocket. The browser stops
automatic reconnect after an explicit operator disconnect; reload to connect again.

## TLS configuration

To enable HTTPS, provide both a certificate and private key in PEM format:

```toml
[gateway]
tls_cert = "/etc/ssl/certs/gateway.pem"
tls_key = "/etc/ssl/private/gateway-key.pem"
```

Or via CLI:

```bash
savfox gateway --tls-cert /path/to/cert.pem --tls-key /path/to/key.pem
```

Both fields must be set together. A configuration containing only one is rejected.

## Session store configuration

The session store uses the following defaults:

| Parameter       | Default     | Description                         |
|-----------------|-------------|-------------------------------------|
| Max age         | 30 days     | Entries older than this are pruned  |
| Max entries     | 500         | Maximum number of sessions stored   |
| Max file size   | 10 MB       | File size before rotation           |
| Max backups     | 3           | Number of rotated backup files      |
| Cache TTL       | 45 seconds  | In-memory cache validity period     |

The session store file is located at `{savfox_home}/sessions/sessions.json`.

## Local Claude terminal agent

The experimental Rust `claude_stream` mode has separate model/settings configuration, explicit version gating, credential-bound approvals and durable session resume. See [Local Claude Code](claude-code.md) for configuration, permissions, cancellation, compatibility limits and migration from one-shot mode.

### Public webhook authentication

Every `/webhooks/<platform>` callback requires an explicitly enabled saved channel. Unconfigured or disabled channels return 503. Verification credentials must be nonempty. Missing credentials, missing signatures, forged signatures, or altered signed bodies return 401 rather than starting an agent. A gateway bearer token does not substitute for platform webhook authentication.

Existing platform verification formats remain in use (Discord public key, Slack signing secret, Telegram secret token, LINE/WhatsApp signing keys, Zalo `x-zalooa-signature`, and the configured verification tokens for other integrations). Feishu HTTP callbacks require a verification token. WhatsApp's setup GET also requires the configured verify token. Channel polling, long connections and the authenticated Matrix appservice routes retain their transport authentication.

For the local IRC, Signal, DingTalk and legacy `/webhooks/matrix` HTTP bridges, store a distinct `webhook_secret` in each saved channel's `config` object. Alternatively set `IRC_WEBHOOK_SECRET`, `SIGNAL_WEBHOOK_SECRET`, `DINGTALK_WEBHOOK_SECRET` or `MATRIX_WEBHOOK_SECRET` while keeping the channel explicitly enabled. The bridge must send `x-hub-signature-256: sha256=<hex HMAC-SHA256(secret, exact request body)>`. Native Matrix appservice transactions continue to use their existing homeserver token routes.

The MS Teams HTTP bridge requires a body HMAC in `x-ms-signature` using the configured app password, in addition to its Authorization header. An unverified Bot Framework bearer JWT alone is rejected. Deployments forwarding native Teams callbacks must validate them in the bridge and supply this HMAC; direct Bot Framework JWT validation remains unsupported here.

Migration: enable the intended channel, configure its verifier and update any local forwarding bridge to sign the unchanged body before exposing the callback. Previous unsigned callbacks are deliberately rejected, including on loopback. Outbound-only channels do not need an inbound verifier unless their callback is used.
