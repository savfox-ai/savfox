# WhatsApp Channel Setup

Connect Savfox to WhatsApp Business so users can chat with the agent through
WhatsApp.

## Prerequisites

- A Meta Business account with WhatsApp Business API access.
- A WhatsApp Business phone number.
- A running Savfox gateway server accessible from the internet.

## Step 1: Set up WhatsApp Business API

1. Go to [developers.facebook.com](https://developers.facebook.com/) and create
   an app with the **Business** type.
2. Add the **WhatsApp** product to your app.
3. In the WhatsApp section, note your:
   - **Phone Number ID**
   - **Permanent Access Token** (generate from the API Setup page)
   - **App Secret** (from Basic Settings)

## Step 2: Configure the gateway

```toml
[gateway.bridges.whatsapp]
enabled = true
phone_number_id = "YOUR_PHONE_NUMBER_ID"
access_token = "YOUR_PERMANENT_ACCESS_TOKEN"
verify_token = "your-custom-verify-token"
app_secret = "YOUR_APP_SECRET"
```

## Step 3: Configure the webhook

1. In the Meta developer dashboard, go to **WhatsApp > Configuration**.
2. Set the **Callback URL** to:
   ```
   https://your-gateway-host:18881/webhooks/whatsapp
   ```
3. Set the **Verify Token** to the same value as `verify_token` in your config.
4. Subscribe to the `messages` webhook field.

The gateway handles the webhook verification GET request automatically by
checking the `hub.verify_token` query parameter.

## Step 4: Test the connection

Send a message to your WhatsApp Business number. The gateway should receive the
webhook, invoke the agent, and send back a reply.

## Security

The gateway verifies incoming webhook signatures using HMAC-SHA256 with your
`app_secret`. The `X-Hub-Signature-256` header is checked against the computed
hash of the request body. Set the `app_secret` field to enable this protection.

## Message format

The WhatsApp Cloud API is used for outbound messages. The gateway sends text
messages via:

```
POST https://graph.facebook.com/v17.0/{phone_number_id}/messages
```

## Limitations

- WhatsApp requires a 24-hour messaging window for user-initiated conversations.
- Template messages are required for business-initiated conversations outside
  the 24-hour window.
- Media attachments are not currently supported by the bridge.

## Troubleshooting

- **Webhook verification fails**: Ensure the `verify_token` in config matches
  the token entered in the Meta dashboard.
- **Messages not delivered**: Check that the access token is valid and the phone
  number is verified.
- **Signature validation errors**: Verify the `app_secret` matches the one in
  your Meta app's Basic Settings.
