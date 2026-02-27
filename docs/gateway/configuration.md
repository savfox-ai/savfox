# Gateway Configuration Reference

The Savfox gateway server is configured through a combination of CLI flags,
the `[gateway]` section in `config.toml`, and environment variables for bridge
credentials.

## Configuration sources (precedence order)

1. **CLI flags** (highest) -- `savfox gateway --port 8080 --token abc`
2. **config.toml `[gateway]` section** -- persistent file configuration
3. **Environment variables** -- for bridge credentials and secrets
4. **Defaults** (lowest) -- built-in defaults

## CLI flags

```
savfox gateway [OPTIONS] [SUBCOMMAND]
```

| Flag          | Type     | Default        | Description                              |
|---------------|----------|----------------|------------------------------------------|
| `--host`      | IP addr  | `127.0.0.1`    | Host address to bind to                  |
| `--port`      | u16      | `18881`        | Port to listen on                        |
| `--token`     | String   | (auto-generated)| Bearer token for authentication         |
| `--tls-cert`  | Path     | None           | TLS certificate path (PEM)               |
| `--tls-key`   | Path     | None           | TLS private key path (PEM)               |

Use `--host 0.0.0.0` to listen on all interfaces.

## config.toml reference

The `[gateway]` section in `~/.savfox/config.toml`:

```toml
[gateway]
host = "127.0.0.1"
port = 18881
# token = "your-static-token"  # Auto-generated if omitted
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"

[gateway.bridges.discord]
enabled = true
bot_token = "your-discord-bot-token"
# application_id = "123456789"
# application_public_key = "hex-encoded-ed25519-public-key"

[gateway.bridges.telegram]
enabled = true
bot_token = "123456:ABCdefGhIjKlMnOpQrStUvWxYz"
# webhook_secret_token = "optional-secret"

[gateway.bridges.slack]
enabled = true
bot_token = "xoxb-your-slack-bot-token"
signing_secret = "your-signing-secret"

[gateway.bridges.msteams]
enabled = true
app_id = "your-microsoft-app-id"
app_password = "your-microsoft-app-password"
# tenant_id = "optional-tenant-id"

[gateway.bridges.webhook]
enabled = true
# callback_url = "https://example.com/webhook"
# secret = "shared-hmac-secret"

[gateway.bridges.whatsapp]
enabled = true
phone_number_id = "your-phone-number-id"
access_token = "your-whatsapp-access-token"
# verify_token = "webhook-verify-token"
# app_secret = "webhook-signature-secret"

[gateway.bridges.signal]
enabled = true
# phone_number = "+1234567890"
# rpc_url = "http://localhost:8080/api/v1/rpc"

[gateway.bridges.imessage]
enabled = true
api_url = "http://localhost:1234"
password = "bluebubbles-server-password"
# poll_interval_secs = 5

[gateway.bridges.zalo]
enabled = true
app_id = "your-zalo-app-id"
app_secret = "your-zalo-app-secret"
access_token = "your-zalo-access-token"
# webhook_verify_token = "optional-verify-token"
```

## Field reference

### `[gateway]`

| Field      | Type            | Default       | Description                          |
|------------|-----------------|---------------|--------------------------------------|
| `host`     | `IpAddr`        | `127.0.0.1`   | Bind address                         |
| `port`     | `u16`           | `18881`       | Listen port                          |
| `token`    | `Option<String>`| `None` (auto) | Static bearer token                  |
| `tls_cert` | `Option<String>`| `None`        | PEM certificate path                 |
| `tls_key`  | `Option<String>`| `None`        | PEM private key path                 |
| `bridges`  | `BridgesConfig` | `{}`          | Chat platform bridge configurations  |

### `[gateway.bridges.discord]`

| Field                    | Type            | Required | Description                    |
|--------------------------|-----------------|----------|--------------------------------|
| `enabled`                | `bool`          | Yes      | Enable the Discord bridge      |
| `bot_token`              | `String`        | Yes      | Discord bot token              |
| `application_id`         | `Option<String>`| No       | Discord application ID         |
| `application_public_key` | `Option<String>`| No       | Ed25519 public key for signature verification |

### `[gateway.bridges.telegram]`

| Field                  | Type            | Required | Description                      |
|------------------------|-----------------|----------|----------------------------------|
| `enabled`              | `bool`          | Yes      | Enable the Telegram bridge       |
| `bot_token`            | `String`        | Yes      | Telegram Bot API token           |
| `webhook_secret_token` | `Option<String>`| No       | Secret for webhook verification  |

### `[gateway.bridges.slack]`

| Field            | Type     | Required | Description                          |
|------------------|----------|----------|--------------------------------------|
| `enabled`        | `bool`   | Yes      | Enable the Slack bridge              |
| `bot_token`      | `String` | Yes      | Slack bot OAuth token (`xoxb-...`)   |
| `signing_secret` | `String` | Yes      | Slack app signing secret             |

### `[gateway.bridges.msteams]`

| Field          | Type            | Required | Description                        |
|----------------|-----------------|----------|------------------------------------|
| `enabled`      | `bool`          | Yes      | Enable the MS Teams bridge         |
| `app_id`       | `String`        | Yes      | Microsoft Bot registration App ID  |
| `app_password` | `String`        | Yes      | Microsoft App Password (secret)    |
| `tenant_id`    | `Option<String>`| No       | Tenant ID for single-tenant bots   |

### `[gateway.bridges.webhook]`

| Field          | Type            | Required | Description                          |
|----------------|-----------------|----------|--------------------------------------|
| `enabled`      | `bool`          | Yes      | Enable the generic webhook bridge    |
| `callback_url` | `Option<String>`| No       | URL for outbound events              |
| `secret`       | `Option<String>`| No       | HMAC-SHA256 shared secret            |

### `[gateway.bridges.whatsapp]`

| Field             | Type            | Required | Description                       |
|-------------------|-----------------|----------|-----------------------------------|
| `enabled`         | `bool`          | Yes      | Enable the WhatsApp bridge        |
| `phone_number_id` | `String`       | Yes      | WhatsApp Business phone number ID |
| `access_token`    | `String`        | Yes      | WhatsApp Business access token    |
| `verify_token`    | `Option<String>`| No       | Webhook verification token        |
| `app_secret`      | `Option<String>`| No       | App secret for signature verification |

### `[gateway.bridges.signal]`

| Field          | Type            | Required | Description                        |
|----------------|-----------------|----------|------------------------------------|
| `enabled`      | `bool`          | Yes      | Enable the Signal bridge           |
| `phone_number` | `Option<String>`| No       | Signal account phone number        |
| `rpc_url`      | `Option<String>`| No       | signal-cli JSON-RPC URL            |

### `[gateway.bridges.imessage]`

| Field                | Type            | Required | Description                    |
|----------------------|-----------------|----------|--------------------------------|
| `enabled`            | `bool`          | Yes      | Enable the iMessage bridge     |
| `api_url`            | `String`        | Yes      | BlueBubbles server URL         |
| `password`           | `String`        | Yes      | BlueBubbles server password    |
| `poll_interval_secs` | `Option<u64>`   | No       | Polling interval (default: 5s) |

### `[gateway.bridges.zalo]`

| Field                  | Type            | Required | Description                     |
|------------------------|-----------------|----------|---------------------------------|
| `enabled`              | `bool`          | Yes      | Enable the Zalo OA bridge       |
| `app_id`               | `String`        | Yes      | Zalo OA App ID                  |
| `app_secret`           | `String`        | Yes      | App secret for signature checks |
| `access_token`         | `String`        | Yes      | OA access token                 |
| `webhook_verify_token` | `Option<String>`| No       | Webhook handshake token         |

## Environment variables

Bridge credentials can also be set via environment variables. These are used as
fallbacks when config.toml values are not present:

| Variable                      | Bridge      | Description                    |
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
| `IRC_BRIDGE_URL`             | IRC         | Bridge HTTP URL (default: `http://127.0.0.1:6667`) |
| `ZALO_OA_ACCESS_TOKEN`       | Zalo        | OA access token                |

## Runtime configuration updates

Configuration can be modified at runtime without restarting the gateway:

### Via REST API

```bash
# Merge-patch (only modifies specified fields)
curl -X POST http://localhost:18881/api/config/patch \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "config": {
      "gateway": {
        "bridges": {
          "discord": {
            "bot_token": "new-token"
          }
        }
      }
    },
    "note": "Updated Discord token"
  }'

# Full replace
curl -X POST http://localhost:18881/api/config/apply \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "config": { ... },
    "note": "Full config replacement"
  }'
```

### Via WS-RPC

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "config.patch",
  "params": {
    "config": {
      "gateway": {
        "bridges": {
          "telegram": {
            "bot_token": "new-token"
          }
        }
      }
    }
  }
}
```

Runtime updates to bridge credentials are hot-reloaded without requiring a
gateway restart. The config.toml file is updated on disk for persistence.

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

Both fields must be set together. Setting only one will be ignored.

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
