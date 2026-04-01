# Telegram channel

The Telegram channel connects Savfox to a Telegram bot, allowing users to chat with the AI agent from any Telegram client.

## Prerequisites

- A running Savfox gateway (`savfox gateway`)
- A Telegram account

## Step 1: Create a Bot via BotFather

1. Open Telegram and search for **@BotFather**.
2. Send the command `/newbot`.
3. Follow the prompts:
   - Choose a display name (e.g., "Savvy Fox").
   - Choose a username ending in `bot` (e.g., `savfox_bot`).
4. BotFather will reply with your **bot token**. It looks like `123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ`.
5. Save this token securely.

## Step 2: Configure Bot Settings (Optional)

Send these commands to @BotFather to customize the bot:

- `/setdescription` -- Set a short description shown on the bot's profile.
- `/setabouttext` -- Set the "About" text.
- `/setuserpic` -- Upload a profile photo.
- `/setcommands` -- Define slash commands (e.g., `/reset` to clear session).

To allow the bot to read all messages in groups (not just commands and mentions):

1. Send `/setprivacy` to @BotFather.
2. Select your bot.
3. Choose **Disable** to turn off privacy mode.

## Step 3: Configure Savfox

Add the Telegram channel to your `~/.savfox/config.toml`:

```toml
[gateway.channels.telegram]
enabled = true
bot_token = "123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ"
```

For additional security, set a webhook secret token. Telegram sends this in the `x-telegram-bot-api-secret-token` header so the gateway can verify requests originate from Telegram:

```toml
[gateway.channels.telegram]
enabled = true
bot_token = "123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ"
webhook_secret_token = "my-random-secret-string"
```

## Step 4: Set the Webhook URL

If your gateway is publicly accessible, Telegram needs to know where to send updates. The gateway registers the webhook automatically on startup. If you need to set it manually:

```bash
curl -X POST "https://api.telegram.org/bot<BOT_TOKEN>/setWebhook" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://your-domain.com/channels/telegram/webhook"}'
```

For local development, use a tunnel service (e.g., ngrok, Cloudflare Tunnel) to expose your gateway.

## Step 5: Start the Gateway

```bash
savfox gateway
```

## Usage

- **Private chats**: Send any message to the bot directly.
- **Group chats**: Add the bot to a group. It responds when mentioned by name or when privacy mode is disabled.
- **Inline replies**: Reply to the bot's message to continue the conversation thread.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Bot does not respond | Verify `bot_token` is correct; check gateway logs |
| Bot ignores group messages | Disable privacy mode via @BotFather (`/setprivacy`) |
| Webhook errors | Ensure your gateway URL is publicly reachable over HTTPS |
| "401 missing_signature" | Set `webhook_secret_token` in config and re-register the webhook |

## Session Behavior

Each Telegram user gets a separate session in private chats. In group chats, the session is scoped to the group. Sessions survive gateway restarts.
