# Microsoft Teams Channel Setup

Connect Savfox to Microsoft Teams so users can interact with the agent in Teams
channels and chats.

## Prerequisites

- A Microsoft 365 account with permission to register bots.
- Azure Active Directory access for app registration.
- A running Savfox gateway server accessible from the internet.

## Step 1: Register a bot in Azure

1. Go to the [Azure Portal](https://portal.azure.com/).
2. Navigate to **Azure Active Directory > App registrations > New registration**.
3. Set the name (e.g., "Savfox Bot") and register.
4. Note the **Application (client) ID**.
5. Under **Certificates & secrets**, create a new client secret. Copy the
   secret value.

## Step 2: Create a Bot Channel Registration

1. In the Azure Portal, search for **Bot Services**.
2. Create a new **Azure Bot** resource.
3. Set the **Messaging endpoint** to:
   ```
   https://your-gateway-host:18881/webhooks/msteams
   ```
4. Link it to the app registration from Step 1.
5. Enable the **Microsoft Teams** channel in the bot's channel settings.

## Step 3: Configure the gateway

```toml
[gateway.bridges.msteams]
enabled = true
app_id = "YOUR_APPLICATION_CLIENT_ID"
app_password = "YOUR_CLIENT_SECRET_VALUE"
# tenant_id = "YOUR_TENANT_ID"  # For single-tenant bots only
```

The `tenant_id` field is optional. Set it only if your bot is restricted to a
single Azure AD tenant.

## Step 4: Install the bot in Teams

### Using Teams App Studio or Developer Portal

1. Open the [Teams Developer Portal](https://dev.teams.microsoft.com/).
2. Create a new app and configure the bot with your App ID.
3. Upload the app to your organization or install it directly.

### Using a manifest

Create an `manifest.json`:

```json
{
  "$schema": "https://developer.microsoft.com/json-schemas/teams/v1.16/MicrosoftTeams.schema.json",
  "manifestVersion": "1.16",
  "id": "YOUR_APP_ID",
  "version": "1.0.0",
  "name": { "short": "Savfox", "full": "Savfox AI Agent" },
  "description": { "short": "AI Agent", "full": "Savfox AI coding agent" },
  "bots": [
    {
      "botId": "YOUR_APP_ID",
      "scopes": ["personal", "team", "groupchat"]
    }
  ]
}
```

Package as a `.zip` and upload to Teams.

## Usage

- **Direct message**: Open a 1:1 chat with the bot in Teams.
- **Channel mention**: @mention the bot in a Teams channel.
- **Adaptive Cards**: The bot sends replies as text messages via the Bot
  Framework.

## Security

The gateway validates incoming Bot Framework requests using the Microsoft App ID
and password. The Bot Framework service authenticates itself using a JWT Bearer
token, which the gateway can verify against the Azure AD endpoint.

## Channel addressing

Use `msteams:<conversation_id>` or `teams:<conversation_id>`:

```json
{
  "channel": "msteams:19:meeting_abc@thread.v2",
  "text": "Hello from Savfox!"
}
```

## Troubleshooting

- **"Unauthorized" errors**: Verify the `app_id` and `app_password` match
  your Azure registration.
- **Bot not appearing in Teams**: Ensure the Teams channel is enabled in the
  Azure Bot resource and the manifest is installed correctly.
- **Messages not received**: Check that the messaging endpoint URL is correct
  and the gateway is reachable from Azure.
