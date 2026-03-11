# Discord Channel Setup

Connect Savfox to Discord so users can interact with the agent via the Discord
Gateway stream and the interactions webhook.

Default behavior: Savfox now uses **stream mode** for inbound Discord
messages. Normal DMs work, and guild messages work when users mention the bot
or reply to one of the bot's messages. The webhook endpoint is still used for
slash commands and interaction callbacks.

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
4. Enable **Message Content Intent** if you want Savfox to read normal guild
   messages in stream mode.

## Step 3: Configure the gateway

Add the Discord channel configuration to your `config.toml`:

```toml
[gateway.channels.discord]
enabled = true
bot_token = "YOUR_DISCORD_BOT_TOKEN"
mode = "stream" # default; use "webhook" to disable the Discord Gateway stream
application_id = "YOUR_APPLICATION_ID"
application_public_key = "YOUR_PUBLIC_KEY_HEX"
# guild_id = "OPTIONAL_GUILD_ID"
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

- **DMs**: Any normal DM text starts a Savfox turn.
- **Guild messages**: Mention the bot or reply to one of the bot's messages.
- **Slash command**: `/savfox <prompt>` -- sends a prompt and receives the
  agent's response through the interactions webhook.
- **Interaction buttons**: Follow-up approvals and related Discord interaction
  callbacks are supported through the same webhook flow.

If you set `mode = "webhook"`, Savfox keeps slash commands and interaction
callbacks but does not open the Discord Gateway stream for normal DMs or plain
guild messages.

## Security

The gateway verifies Discord interaction signatures using Ed25519 with the
`application_public_key`. Invalid signatures are rejected with HTTP 401.

## Troubleshooting

- **"Invalid token"**: Verify the bot token is correct and has not been reset.
- **Signature verification fails**: Ensure `application_public_key` matches the
  value shown in the Discord Developer Portal.
- **Bot doesn't respond to normal guild messages**: Make sure `mode = "stream"`
  is enabled, the gateway is running, and **Message Content Intent** is enabled
  in the Discord Developer Portal.
- **Bot doesn't respond to DMs**: Make sure the Discord channel is in stream
  mode and the gateway started the Discord stream successfully.
- **Slash commands don't work**: Ensure the **Interactions Endpoint URL** is
  set to `/webhooks/discord`.
- **Bot can't reply after a slash command**: Verify the bot has `Send Messages`
  permission. Use `discord:<channel_id>` format for programmatic messaging.
