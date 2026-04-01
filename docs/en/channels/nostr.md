# Nostr Channel

Connect Savfox to the Nostr decentralized social protocol.

## Prerequisites

- A Nostr private key (hex or nsec format)
- One or more relay URLs

## Configuration

```toml
[[channels]]
type = "nostr"
enabled = true

[channels.nostr]
private_key = "${NOSTR_PRIVATE_KEY}"
relays = [
  "wss://relay.damus.io",
  "wss://nos.lol",
  "wss://relay.nostr.band"
]
```

## How It Works

The Nostr channel connects to configured relays via WebSocket:

- **Kind 1** (Text Notes): Public posts mentioning the bot's pubkey
- **Kind 4** (Encrypted DMs): Private messages to the bot

The channel subscribes to events that mention the bot and responds inline.

## Features

| Feature | Support |
|---------|---------|
| Public mentions | Full |
| Encrypted DMs (NIP-04) | Full |
| Relay multiplexing | Full |
| Event signing | NIP-01 |
| Profile metadata | Read |

## Security

- Private keys should be stored securely (use env vars)
- NIP-04 encryption provides basic DM privacy
- Consider using a dedicated keypair for the bot
- The bot's pubkey is derived from the private key
