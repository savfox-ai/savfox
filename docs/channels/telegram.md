# Telegram Channel Setup

Connect Savfox to Telegram so users can chat with the agent through a Telegram
bot.

## Prerequisites

- A Telegram account.
- A running Savfox gateway server accessible from the internet (for webhooks).

## Step 1: Create a Telegram bot

1. Open Telegram and search for **@BotFather**.
2. Send `/newbot` and follow the prompts to name your bot.
3. BotFather will give you a bot token in the format `123456:ABCdef...`.
4. Copy this token.

## Step 2: Configure the gateway

Add the Telegram channel to `config.toml`:

```toml
[gateway.channels.telegram]
enabled = true
bot_token = "123456789:ABCdefGhIjKlMnOpQrStUvWxYz"
# webhook_secret_token = "optional-secret-for-verification"
```

Or set the environment variable:

```bash
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGhIjKlMnOpQrStUvWxYz"
```

## Step 3: Register the webhook

Tell Telegram where to send updates:

```bash
curl -X POST \
  "https://api.telegram.org/bot<YOUR_BOT_TOKEN>/setWebhook" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://your-gateway-host:18881/webhooks/telegram",
    "secret_token": "optional-secret-for-verification"
  }'
```

If you set `webhook_secret_token` in your config, include the same value in the
`secret_token` field so the gateway can verify incoming requests.

## Step 4: Verify the webhook

Check that the webhook is active:

```bash
curl "https://api.telegram.org/bot<YOUR_BOT_TOKEN>/getWebhookInfo"
```

The response should show your URL and `pending_update_count`.

## Usage

Users interact with the bot using the `/savfox` command:

```
/savfox Explain the difference between async and threads in Rust
```

The bot parses `/savfox <prompt>` messages, invokes the agent, and replies with
the response. Messages without the `/savfox` prefix in group chats are ignored.
In private chats, any message triggers the agent.

## Security

If `webhook_secret_token` is configured, the gateway checks the
`X-Telegram-Bot-Api-Secret-Token` header against the configured value. Requests
with a mismatched or missing secret are rejected.

## Channel addressing

Use `telegram:<chat_id>` when sending messages programmatically:

```json
{
  "channel": "telegram:987654321",
  "text": "Hello from Savfox!"
}
```

The gateway sends messages via the Telegram Bot API with HTML parse mode enabled.

## Troubleshooting

- **Bot doesn't respond**: Verify the webhook URL is correct and the gateway is
  reachable from the internet.
- **"Unauthorized"**: The bot token may have been regenerated. Get a new token
  from BotFather and update the configuration.
- **Webhook conflicts**: Only one webhook can be active per bot. If using
  polling elsewhere, disable it first.
