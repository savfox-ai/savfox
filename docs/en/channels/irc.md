# IRC Channel

Connect Savfox to IRC (Internet Relay Chat) servers.

## Prerequisites

- An IRC server to connect to
- Optional: NickServ credentials for registered nicknames

## Configuration

```toml
[[channels]]
type = "irc"
enabled = true

[channels.irc]
server = "irc.libera.chat"
port = 6697
use_tls = true
nickname = "savfox-bot"
channels = ["#my-channel"]
nickserv_password = "${IRC_NICKSERV_PASSWORD}"
```

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `server` | Required | IRC server hostname |
| `port` | `6697` | Server port (6697 for TLS, 6667 for plain) |
| `use_tls` | `true` | Use TLS encryption |
| `nickname` | `"savfox"` | Bot nickname |
| `username` | Same as nick | IRC username |
| `realname` | `"Savfox Bot"` | GECOS/realname field |
| `channels` | `[]` | Channels to auto-join |
| `nickserv_password` | None | NickServ identification password |
| `command_prefix` | `"!"` | Prefix for bot commands |
| `respond_to_private` | `true` | Respond to private messages |

## Features

| Feature | Support |
|---------|---------|
| Channel messages | Full |
| Private messages | Full |
| CTCP ACTION (/me) | Receive |
| NickServ auth | SASL + IDENTIFY |
| TLS | Full |
| Multiple channels | Full |
| Flood protection | Built-in |

## Authentication

### SASL (Preferred)

SASL authentication happens during connection, before joining channels:
```toml
[channels.irc]
sasl_username = "savfox-bot"
sasl_password = "${IRC_SASL_PASSWORD}"
```

### NickServ

Falls back to NickServ IDENTIFY after connecting:
```toml
[channels.irc]
nickserv_password = "${IRC_NICKSERV_PASSWORD}"
```

## Troubleshooting

- **Cannot connect**: Check server, port, and TLS settings
- **Nickname in use**: Choose a different nickname or register it
- **Not receiving messages**: Ensure the bot has joined the channel
- **Disconnecting**: The channel auto-reconnects with exponential backoff
