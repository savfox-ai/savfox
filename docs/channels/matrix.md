# Matrix Channel Setup

Connect Savfox to the Matrix protocol so users can interact with the agent in
Matrix rooms.

## Prerequisites

- A Matrix account on any homeserver (e.g., matrix.org, Element, Synapse).
- An access token for the bot account (recommended) OR password.
- A running Savfox gateway server.

## Step 1: Create a Matrix bot account

Register a new account on your Matrix homeserver for the bot. You can use
Element, or register via the API:

```bash
curl -X POST "https://matrix.org/_matrix/client/v3/register" \
  -H "Content-Type: application/json" \
  -d '{
    "username": "savfox-bot",
    "password": "secure-password",
    "auth": {"type": "m.login.dummy"}
  }'
```

## Step 2: Obtain credentials

### Option A: Access Token (Recommended)

Log in to get an access token:

```bash
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

The response contains an `access_token` field. Copy this value. With an access
token, the user ID is fetched automatically via `/whoami`.

### Option B: Password

Alternatively, provide the bot's user ID and password. Savfox will log in and
store the access token automatically.

## Step 3: Configure the gateway

### Via Environment Variables

```bash
export MATRIX_HOMESERVER="https://matrix.org"
export MATRIX_ACCESS_TOKEN="syt_your_access_token_here"
# OR use password auth:
# export MATRIX_USER_ID="@savfox-bot:matrix.org"
# export MATRIX_PASSWORD="secure-password"
```

### Via Configuration

Add to your `config.toml`:

```toml
[channels.matrix]
enabled = true
homeserver = "https://matrix.org"
accessToken = "syt_***"
# Optional:
# userId = "@savfox-bot:matrix.org"
# password = "secure-password"
# deviceName = "Savfox Gateway"
# encryption = false
```

## Step 4: DM Policy Configuration

Control who can send direct messages to the bot:

| Policy | Description |
|--------|-------------|
| `pairing` | Unknown senders get a pairing code (default) |
| `allowlist` | Only users in `dmAllowFrom` can DM |
| `open` | Anyone can DM (requires `"*"` in `dmAllowFrom`) |
| `disabled` | No DMs allowed |

Example:

```toml
[channels.matrix.dm]
policy = "allowlist"
allowFrom = ["@user:server.org", "@admin:example.com"]
```

## Step 5: Room/Group Configuration

Control which rooms the bot responds to:

| Policy | Description |
|--------|-------------|
| `allowlist` | Only rooms in `groups` (mention-gated by default) |
| `open` | Any room can trigger the bot (mention-gated) |
| `disabled` | No rooms allowed |

Example:

```toml
[channels.matrix]
groupPolicy = "allowlist"

[channels.matrix.groups]
"!roomId:server.org" = { allow = true }
"#alias:server.org" = { allow = true, requireMention = false }
```

## Step 6: Invite the bot to a room

In your Matrix client, invite the bot user to the room where you want it to
respond. The bot will auto-join by default (configurable via `autoJoin`).

## Encryption (E2EE)

End-to-end encryption is supported. Enable with `encryption = true`:

```toml
[channels.matrix]
encryption = true
```

When E2EE is enabled:
- Encrypted rooms are decrypted automatically
- On first connection, the bot requests device verification
- Verify the device in another Matrix client (Element, etc.) to enable key sharing

## Multi-account Support

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

## Configuration Reference

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | false | Enable/disable channel |
| `homeserver` | string | - | Homeserver URL |
| `userId` | string | - | Matrix user ID (optional with token) |
| `accessToken` | string | - | Access token for auth |
| `password` | string | - | Password (alternative to token) |
| `deviceName` | string | - | Device display name |
| `encryption` | bool | false | Enable E2EE |
| `dm.policy` | string | "pairing" | DM access policy |
| `dm.allowFrom` | array | [] | Allowed DM senders |
| `groupPolicy` | string | "allowlist" | Room access policy |
| `groups` | object | {} | Room allowlist config |
| `autoJoin` | string | "always" | Invite handling: always/allowlist/off |

## Troubleshooting

- **"M_UNKNOWN_TOKEN"**: The access token has expired. Log in again for a new one.
- **Bot doesn't respond**: Verify the bot has joined the room and can send messages.
- **DMs ignored**: Check `dm.policy` - sender may need approval.
- **Rooms ignored**: Check `groupPolicy` and `groups` allowlist.
- **Encrypted rooms fail**: Enable `encryption = true` and verify the device.
- **Homeserver unreachable**: Check the `homeserver` URL.

## Usage

Once configured, send messages to the bot:
- **DM**: Start a direct message with the bot
- **Room**: Mention the bot or use the configured trigger prefix

Messages are sent using the Matrix Client-Server API:

```
PUT /_matrix/client/v3/rooms/{room_id}/send/m.room.message/{txn_id}
```
