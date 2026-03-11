# Matrix Channel

Connect Savfox to Matrix, a federated open communication protocol.

## Prerequisites

- A Matrix homeserver account (e.g., on matrix.org or self-hosted Synapse)
- An access token for the bot account (recommended) OR password

## Setup Steps

### 1. Create a Bot Account

Register a new account on your homeserver for the bot:

```bash
# Linux/macOS
curl -X POST "https://matrix.org/_matrix/client/v3/register" \
  -H "Content-Type: application/json" \
  -d '{
    "username": "savfox-bot",
    "password": "secure-password",
    "auth": {"type": "m.login.dummy"}
  }'
```

```powershell
# Windows PowerShell
$body = @{
  username  = "savfox-bot"
  password  = "secure-password"
  auth      = @{ type = "m.login.dummy" }
} | ConvertTo-Json -Depth 5

Invoke-WebRequest `
  -Method POST `
  -Uri "https://matrix.org/_matrix/client/v3/register" `
  -ContentType "application/json" `
  -Body $body
```

### 2. Get Credentials

**Option A: Access Token (Recommended)**

```bash
# Linux/macOS
curl -X POST "https://matrix.org/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "m.login.password",
    "identifier": {
      "type": "m.id.user",
      "user": "savfox-bot"
    },
    "password": "secure-password"
  }'
```

```powershell
# Windows PowerShell
$body = @{
  type       = "m.login.password"
  identifier = @{
    type = "m.id.user"
    user = "savfox-bot"
  }
  password   = "secure-password"
} | ConvertTo-Json -Depth 5

Invoke-WebRequest `
  -Method POST `
  -Uri "https://matrix.org/_matrix/client/v3/login" `
  -ContentType "application/json" `
  -Body $body
```

Save the `access_token` from the response. With an access token, the user ID is fetched automatically via `/whoami`.

**Option B: Password**

Provide the bot's user ID and password. Savfox will log in and store the access token automatically.

### 3. Configure the Gateway

**Via Environment Variables:**

```bash
export MATRIX_HOMESERVER="https://matrix.org"
export MATRIX_ACCESS_TOKEN="syt_***"
# OR:
# export MATRIX_USER_ID="@savfox-bot:matrix.org"
# export MATRIX_PASSWORD="secure-password"
```

**Via Configuration:**

```toml
[channels.matrix]
enabled = true
homeserver = "https://matrix.org"
accessToken = "syt_***"
deviceName = "Savfox Gateway"
encryption = false
```

### 4. DM Policy Configuration

| Policy | Description |
|--------|-------------|
| `pairing` | Unknown senders get a pairing code (default) |
| `allowlist` | Only users in `dmAllowFrom` can DM |
| `open` | Anyone can DM |
| `disabled` | No DMs allowed |

```toml
[channels.matrix.dm]
policy = "allowlist"
allowFrom = ["@user:server.org", "@admin:example.com"]
```

### 5. Room Configuration

| Policy | Description |
|--------|-------------|
| `allowlist` | Only rooms in `groups` (mention-gated) |
| `open` | Any room can trigger (mention-gated) |
| `disabled` | No rooms allowed |

```toml
[channels.matrix]
groupPolicy = "allowlist"

[channels.matrix.groups]
"!roomId:server.org" = { allow = true }
"#alias:server.org" = { allow = true, requireMention = false }
```

### 6. Invite the Bot

Invite the bot user to rooms where you want it to respond. The bot auto-joins by default.

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Direct messages | Full |
| Rooms | Full |
| Threads | Basic |
| Media | Receive + Send |
| Reactions | Receive |
| End-to-end encryption | Supported |
| Room invites | Auto-join |

## Encryption (E2EE)

Enable with `encryption = true`:

```toml
[channels.matrix]
encryption = true
```

- Encrypted rooms are decrypted automatically
- On first connection, verify the device in another Matrix client
- Once verified, the bot can decrypt messages in encrypted rooms

## Multi-account

Configure multiple Matrix accounts:

```toml
[channels.matrix.accounts.assistant]
name = "Main assistant"
homeserver = "https://matrix.example.org"
accessToken = "syt_assistant_***"
encryption = true

[channels.matrix.accounts.alerts]
name = "Alerts bot"
homeserver = "https://matrix.example.org"
accessToken = "syt_alerts_***"

[channels.matrix.accounts.alerts.dm]
policy = "allowlist"
allowFrom = ["@admin:example.org"]
```

## Options Reference

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | false | Enable channel |
| `homeserver` | string | - | Homeserver URL |
| `userId` | string | - | User ID (optional with token) |
| `accessToken` | string | - | Access token |
| `password` | string | - | Password (alternative) |
| `deviceName` | string | - | Device display name |
| `encryption` | bool | false | Enable E2EE |
| `dm.policy` | string | "pairing" | DM policy |
| `dm.allowFrom` | array | [] | Allowed DM senders |
| `groupPolicy` | string | "allowlist" | Room policy |
| `groups` | object | {} | Room allowlist |
| `autoJoin` | string | "always" | Auto-join: always/allowlist/off |

## Troubleshooting

- **Bot not responding**: Verify access token and check if bot is in the room
- **M_UNKNOWN_TOKEN**: Access token expired - log in again
- **DMs ignored**: Check `dm.policy` - sender may need approval
- **Rooms ignored**: Check `groupPolicy` and `groups` allowlist
- **E2EE rooms fail**: Enable `encryption = true` and verify the device
- **Rate limited**: Matrix has rate limits — the bridge respects them automatically
