---
name: discord
description: Manage Discord servers, channels, messages, and bot interactions.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🎮"
    requires:
      env:
        - DISCORD_BOT_TOKEN
    install: []
---

# Discord Skill

You can interact with Discord servers using the Discord REST API.

## Authentication

Requires `DISCORD_BOT_TOKEN` environment variable.

## Sending Messages

Send a message to a channel:
```bash
curl -X POST "https://discord.com/api/v10/channels/{channel_id}/messages" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"content": "Hello from Savfox!"}'
```

Send an embed:
```bash
curl -X POST "https://discord.com/api/v10/channels/{channel_id}/messages" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "embeds": [{
      "title": "Status Report",
      "description": "Everything is working!",
      "color": 3066993
    }]
  }'
```

## Server Operations

List guilds the bot is in:
```bash
curl -s "https://discord.com/api/v10/users/@me/guilds" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" | jq '.[] | {id, name}'
```

List channels in a guild:
```bash
curl -s "https://discord.com/api/v10/guilds/{guild_id}/channels" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" | jq '.[] | {id, name, type}'
```

## Member Management

List members:
```bash
curl -s "https://discord.com/api/v10/guilds/{guild_id}/members?limit=100" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN" | jq '.[] | {user: .user.username, nick: .nick}'
```

## Reactions

Add a reaction:
```bash
curl -X PUT "https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}/reactions/👍/@me" \
  -H "Authorization: Bot $DISCORD_BOT_TOKEN"
```

## Guidelines

- Use embeds for structured information (max 10 embeds per message)
- Message content limit: 2000 characters
- Rate limits: 5 requests per second per route, global limit 50/s
- Use thread channels for long conversations
