# Discord Channel Setup

Connect Savfox to Discord so users can interact with the agent via slash
commands and direct messages in any Discord server.

## Prerequisites

- A Discord account with permission to create applications.
- A running Savfox gateway server reachable from the internet.

## Step 1: Create a Discord application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** and give it a name (e.g., "Savfox").
3. Note the **Application ID** and **Public Key** from the General Information page.

## Step 2: Create a bot user

1. Navigate to the **Bot** section in your application settings.
2. Click **Add Bot**.
3. Under **Token**, click **Reset Token** and copy the bot token.
4. Enable the **Message Content Intent** under Privileged Gateway Intents.

## Step 3: Configure the gateway

Add the Discord bridge configuration to your `config.toml`:

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "YOUR_DISCORD_BOT_TOKEN"
application_id = "YOUR_APPLICATION_ID"
application_public_key = "YOUR_PUBLIC_KEY_HEX"
```

Alternatively, set the environment variable:

```bash
export DISCORD_BOT_TOKEN="YOUR_DISCORD_BOT_TOKEN"
```

## Step 4: Set up the interactions endpoint

1. In the Discord Developer Portal, go to **General Information**.
2. Set **Interactions Endpoint URL** to:
   ```
   https://your-gateway-host:18881/webhooks/discord
   ```
3. Discord will send a verification request. The gateway handles this
   automatically by responding to type-1 PING interactions.

## Step 5: Register slash commands

Register the `/savfox` slash command using the Discord API:

```bash
curl -X POST \
  "https://discord.com/api/v10/applications/YOUR_APP_ID/commands" \
  -H "Authorization: Bot YOUR_BOT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "savfox",
    "description": "Send a prompt to Savfox",
    "options": [
      {
        "name": "prompt",
        "description": "Your message to the agent",
        "type": 3,
        "required": true
      }
    ]
  }'
```

## Step 6: Invite the bot to your server

Generate an invite URL with the following scopes and permissions:

- Scopes: `bot`, `applications.commands`
- Permissions: `Send Messages`, `Read Message History`, `Embed Links`

```
https://discord.com/api/oauth2/authorize?client_id=YOUR_APP_ID&scope=bot+applications.commands&permissions=2147485696
```

## Usage

Once configured, users can interact with the agent:

- **Slash command**: `/savfox <prompt>` -- sends a prompt and receives the
  agent's response.
- **Direct message**: Send a DM to the bot to start a conversation.

## Security

The gateway verifies Discord interaction signatures using Ed25519 with the
`application_public_key`. Invalid signatures are rejected with HTTP 401.

## Troubleshooting

- **"Invalid token"**: Verify the bot token is correct and has not been reset.
- **Signature verification fails**: Ensure `application_public_key` matches the
  value shown in the Discord Developer Portal.
- **Bot doesn't respond**: Check the **Message Content Intent** is enabled and
  the bot has `Send Messages` permission. Use `discord:<channel_id>` format for
  programmatic messaging.
