# Discord Channel

The Discord channel connects Savfox to Discord using a gateway stream for
normal inbound messages and an interactions webhook for slash commands and
buttons.

Default inbound mode is `stream`.

## Prerequisites

- A running Savfox gateway (`savfox gateway`)
- A Discord account with permission to add bots to your server

## Step 1: Create a Discord Application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** and give it a name (e.g., "Savvy Fox").
3. Navigate to the **Bot** tab in the left sidebar.
4. Click **Add Bot** and confirm.

## Step 2: Configure Bot Permissions

On the Bot settings page:

1. Enable **Message Content Intent** if you want Savfox to read normal guild
   messages in stream mode.
2. Under OAuth2 > URL Generator, select the `bot` scope.
3. Select these bot permissions:
   - Send Messages
   - Read Message History
   - Use Slash Commands (optional)
4. Copy the generated invite URL and open it in your browser to add the bot to your server.

## Step 3: Get the Bot Token

1. On the Bot settings page, click **Reset Token** (or **Copy** if visible).
2. Save this token securely. You will need it for the Savfox configuration.

**Never share your bot token publicly.** If compromised, reset it immediately in the Developer Portal.

## Step 4: Configure Savfox

Add the Discord channel to your `~/.savfox/config.toml`:

```toml
[gateway.channels.discord]
enabled = true
bot_token = "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX.XXXXXX.XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
mode = "stream" # default; use "webhook" to disable gateway streaming
# application_id = "123456789012345678"
# application_public_key = "hex-encoded-ed25519-public-key"
# guild_id = "optional-guild-id"
```

Alternatively, set the token via an environment variable to avoid storing it in the config file:

```toml
[gateway.channels.discord]
enabled = true
bot_token = "$DISCORD_BOT_TOKEN"
```

Then export the variable before starting the gateway:

```bash
export DISCORD_BOT_TOKEN="MTIzNDU2..."
savfox gateway
```

## Step 5: Start the Gateway

```bash
savfox gateway
```

The gateway exposes the Discord interactions endpoint and also starts the
Discord Gateway stream by default.

## Usage

- **DMs**: Plain text DMs start a Savfox turn.
- **Guilds**: Mention the bot or reply to one of the bot's messages.
- **Slash command**: Use `/savfox <prompt>` to send a prompt through the
  interactions webhook.
- **Interaction callbacks**: Buttons and related follow-up interactions use the
  same webhook flow.

If you switch to `mode = "webhook"`, Savfox keeps the interactions webhook but
does not open the Discord Gateway stream for normal DMs or plain messages.

## Webhook Verification

Discord sends an `x-signature-ed25519` header and `x-signature-timestamp` with each webhook request. The gateway verifies these automatically using the bot's public key. No additional configuration is needed.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Bot appears offline | Check that `enabled = true`, `mode = "stream"`, and the `bot_token` is correct |
| Bot does not respond to normal guild messages | Enable Message Content Intent and make sure the gateway started the Discord stream |
| Bot does not respond to DMs | Make sure the channel is using stream mode and the gateway stream is running |
| "Invalid token" error in logs | Reset the bot token and update your config |
| Slash commands do not work | Make sure the Interactions Endpoint URL points to `/webhooks/discord` and `application_public_key` is configured if you want signature verification |
| Bot cannot send messages | Verify the bot has Send Messages permission in the channel |

## Session Behavior

Each Discord user gets a separate session by default. Group channels share a session scoped to the channel. Sessions persist across gateway restarts via the file-based session store.

To reset a user's session, use the gateway management API:

```bash
savfox gateway channels
```
