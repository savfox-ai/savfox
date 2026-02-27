# Twitch Channel

Connect Savfox to Twitch chat via IRC/TMI.

## Prerequisites

- A Twitch account for the bot
- OAuth token from [Twitch Token Generator](https://twitchtokengenerator.com/)

## Configuration

```toml
[[channels]]
type = "twitch"
enabled = true

[channels.twitch]
username = "savfox_bot"
oauth_token = "${TWITCH_OAUTH_TOKEN}"
channels = ["#yourchannel"]
command_prefix = "!"
```

## OAuth Token

Generate an OAuth token with these scopes:
- `chat:read` — Read chat messages
- `chat:edit` — Send chat messages

The token format is: `oauth:xxxxxxxxxxxxxxxxxxxxxxxxxx`

## Features

| Feature | Support |
|---------|---------|
| Chat messages | Full |
| Whispers | Receive |
| Commands (!cmd) | Full |
| Emotes | Text form |
| Multiple channels | Full |
| Auto-reconnect | Full |

## Rate Limits

Twitch has strict rate limits for chat messages:
- Regular users: 20 messages per 30 seconds
- Moderators: 100 messages per 30 seconds
- Verified bots: Higher limits

The bridge respects these limits automatically.

## Commands

The bot responds to messages prefixed with the configured command prefix:
```
!ask What is the weather today?
!help Show available commands
```
