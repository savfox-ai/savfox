# Discord Bridge

The Discord bridge connects Savfox to a Discord server, allowing users to chat with the AI agent directly from Discord channels and DMs.

## Prerequisites

- A running Savfox gateway (`savfox gateway`)
- A Discord account with permission to add bots to your server

## Step 1: Create a Discord Application

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** and give it a name (e.g., "Savfox Agent").
3. Navigate to the **Bot** tab in the left sidebar.
4. Click **Add Bot** and confirm.

## Step 2: Configure Bot Permissions

On the Bot settings page:

1. Enable **Message Content Intent** under Privileged Gateway Intents. This is required for the bot to read message text.
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

Add the Discord bridge to your `~/.savfox/config.toml`:

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX.XXXXXX.XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
```

Alternatively, set the token via an environment variable to avoid storing it in the config file:

```toml
[gateway.bridges.discord]
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

The gateway will connect to Discord and the bot will appear as online in your server.

## Usage

- **DMs**: Send a direct message to the bot. It creates a private session for each user.
- **Channels**: Mention the bot (`@Savfox Agent`) or reply to one of its messages. The bot tracks separate sessions per channel.
- **Threads**: The bot supports Discord threads. Conversations within a thread share a single session.

## Webhook Verification

Discord sends an `x-signature-ed25519` header and `x-signature-timestamp` with each webhook request. The gateway verifies these automatically using the bot's public key. No additional configuration is needed.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Bot appears offline | Check that `enabled = true` and the `bot_token` is correct |
| Bot does not respond to messages | Enable the **Message Content Intent** in the Developer Portal |
| "Invalid token" error in logs | Reset the bot token and update your config |
| Bot cannot send messages | Verify the bot has Send Messages permission in the channel |

## Session Behavior

Each Discord user gets a separate session by default. Group channels share a session scoped to the channel. Sessions persist across gateway restarts via the file-based session store.

To reset a user's session, use the gateway management API:

```bash
savfox gateway channels
```
