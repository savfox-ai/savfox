# Slack Channel

The Slack channel connects Savfox to a Slack workspace, letting users interact with the AI agent from Slack channels, threads, and DMs.

## Prerequisites

- A running Savfox gateway (`savfox gateway`)
- Admin access to a Slack workspace (or permission to install apps)

## Step 1: Create a Slack App

1. Go to the [Slack API Apps page](https://api.slack.com/apps).
2. Click **Create New App** > **From scratch**.
3. Name the app (e.g., "Savfox Agent") and select your workspace.

## Step 2: Configure OAuth Scopes

Navigate to **OAuth & Permissions** in the sidebar and add these **Bot Token Scopes**:

| Scope | Purpose |
|-------|---------|
| `chat:write` | Send messages |
| `channels:history` | Read channel messages |
| `groups:history` | Read private channel messages |
| `im:history` | Read DM messages |
| `im:write` | Send DMs |
| `app_mentions:read` | Respond to @mentions |

## Step 3: Enable Event Subscriptions

1. Go to **Event Subscriptions** and toggle it on.
2. Set the **Request URL** to your gateway's Slack webhook endpoint:
   ```
   https://your-domain.com/channels/slack/webhook
   ```
3. Slack will send a challenge request to verify the URL. The gateway handles this automatically.
4. Under **Subscribe to bot events**, add:
   - `message.channels`
   - `message.groups`
   - `message.im`
   - `app_mention`

## Step 4: Install the App

1. Go to **OAuth & Permissions**.
2. Click **Install to Workspace** and authorize.
3. Copy the **Bot User OAuth Token** (starts with `xoxb-`).

## Step 5: Get the Signing Secret

1. Go to **Basic Information**.
2. Under **App Credentials**, copy the **Signing Secret**.

The signing secret is used to verify that incoming webhook requests genuinely come from Slack.

## Step 6: Configure Savfox

Add the Slack channel to `~/.savfox/config.toml`:

```toml
[gateway.channels.slack]
enabled = true
bot_token = "xoxb-1234567890-1234567890123-abcdefghijklmnopqrstuvwx"
signing_secret = "abc123def456ghi789jkl012mno345pq"
```

## Step 7: Start the Gateway

```bash
savfox gateway
```

## Usage

- **Channels**: Invite the bot to a channel, then mention it with `@Savfox Agent your question`.
- **DMs**: Send a direct message to the bot.
- **Threads**: Reply in a thread to continue a conversation. The bot maintains session context per thread.

## Webhook Security

Slack signs every webhook request with the signing secret. The gateway verifies the `x-slack-signature` and `x-slack-request-timestamp` headers automatically. Requests older than 5 minutes are rejected to prevent replay attacks.

Failure responses:

- `401 invalid_signature` -- HMAC mismatch
- `401 missing_signature` -- Required headers are absent
- `401 stale_signature` -- Timestamp outside the 5-minute replay window

## Troubleshooting

| Problem | Solution |
|---------|----------|
| URL verification fails | Ensure the gateway is publicly reachable and the path is correct |
| Bot does not respond in channels | Invite the bot to the channel; check that `app_mentions:read` is enabled |
| "invalid_auth" error | Verify the `bot_token` starts with `xoxb-` |
| Signature verification fails | Confirm `signing_secret` matches the value in Slack's Basic Information |

## Session Behavior

In DMs, each Slack user gets an individual session. In channels, the bot scopes sessions per thread. If a message is not in a thread, it belongs to the channel-level session.
