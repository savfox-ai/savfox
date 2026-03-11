# Microsoft Teams Channel

Connect Savfox to Microsoft Teams via the Bot Framework.

## Prerequisites

- Microsoft Azure account
- Bot Framework registration
- Teams admin access (to install the bot)

## Configuration

```toml
[[channels]]
type = "msteams"
enabled = true

[channels.msteams]
app_id = "${TEAMS_APP_ID}"
app_secret = "${TEAMS_APP_SECRET}"
tenant_id = "${TEAMS_TENANT_ID}"
webhook_path = "/hooks/msteams"
```

## Setup Steps

### 1. Register a Bot in Azure

1. Go to [Azure Portal](https://portal.azure.com/)
2. Create a new **Bot Channels Registration**
3. Note the **App ID** and generate an **App Secret**

### 2. Configure the Messaging Endpoint

Set the messaging endpoint to:
```
https://your-gateway.example.com/hooks/msteams
```

### 3. Enable the Teams Channel

In the Azure Bot settings, enable the Microsoft Teams channel.

### 4. Install in Teams

Create a Teams app package and install it in your organization.

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Adaptive Cards | Send |
| @mentions | Receive |
| 1:1 chat | Full |
| Group chat | Full |
| Channel messages | When @mentioned |
| File attachments | Receive |

## Message Types

The bridge supports both plain text and Adaptive Cards:

```json
{
  "type": "AdaptiveCard",
  "body": [
    {"type": "TextBlock", "text": "Hello from Savfox!", "weight": "bolder"}
  ]
}
```

## Troubleshooting

- **401 Unauthorized**: Verify App ID and Secret
- **Bot not responding in channels**: Ensure the bot is @mentioned
- **Webhook not receiving**: Check Azure Bot messaging endpoint URL
