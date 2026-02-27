# Slack Channel Setup

Connect Savfox to Slack so team members can interact with the agent from any
Slack channel or direct message.

## Prerequisites

- A Slack workspace where you have permission to install apps.
- A running Savfox gateway server accessible from the internet.

## Step 1: Create a Slack app

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App**.
2. Choose **From scratch** and select your workspace.
3. Note the **Signing Secret** from the Basic Information page.

## Step 2: Configure bot scopes

Navigate to **OAuth & Permissions** and add the following Bot Token Scopes:

- `chat:write` -- send messages
- `chat:write.public` -- send to channels the bot hasn't joined
- `commands` -- register slash commands
- `app_mentions:read` -- respond to @mentions
- `channels:history` -- read message history (optional)

## Step 3: Install the app

Click **Install to Workspace** and authorize the app. Copy the **Bot User
OAuth Token** (`xoxb-...`).

## Step 4: Configure the gateway

```toml
[gateway.bridges.slack]
enabled = true
bot_token = "xoxb-your-slack-bot-token"
signing_secret = "your-signing-secret"
```

Or set the environment variable:

```bash
export SLACK_BOT_TOKEN="xoxb-your-slack-bot-token"
```

## Step 5: Set up the request URL

1. In the Slack app settings, go to **Event Subscriptions**.
2. Enable events and set the Request URL to:
   ```
   https://your-gateway-host:18881/webhooks/slack
   ```
3. Slack will send a verification challenge. The gateway handles this
   automatically.
4. Subscribe to bot events: `message.im`, `app_mention`.

## Step 6: Create a slash command (optional)

1. Go to **Slash Commands** in the app settings.
2. Create a new command:
   - Command: `/savfox`
   - Request URL: `https://your-gateway-host:18881/webhooks/slack`
   - Description: "Send a prompt to Savfox"

## Usage

- **Slash command**: `/savfox <prompt>`
- **Mention**: `@Savfox <prompt>` in any channel the bot is in.
- **Direct message**: Send any message to the bot directly.

## Security

The gateway verifies every incoming Slack request using HMAC-SHA256 signature
verification:

1. The `X-Slack-Signature` header is compared against a computed signature.
2. The `X-Slack-Request-Timestamp` header is checked for freshness (within 5
   minutes).
3. Requests with invalid signatures or stale timestamps are rejected.

## Channel addressing

Use `slack:<channel_id>` when sending messages:

```json
{
  "channel": "slack:C01234567",
  "text": "Hello from Savfox!"
}
```

## Troubleshooting

- **"url_verification" fails**: Ensure the gateway is reachable at the URL and
  the signing secret is correct.
- **Bot doesn't respond in channels**: The bot must be invited to the channel
  or have `chat:write.public` scope.
- **Signature mismatch**: Verify the signing secret matches the one shown in
  the Slack app's Basic Information page.
