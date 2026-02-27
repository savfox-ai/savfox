# WhatsApp Channel

Connect Savfox to WhatsApp using the WhatsApp Business API or third-party bridges.

## Prerequisites

- WhatsApp Business API access (via Meta Business Platform)
- A verified business phone number
- A webhook endpoint accessible from the internet

## Configuration

```toml
[[channels]]
type = "whatsapp"
enabled = true

[channels.whatsapp]
phone_number_id = "1234567890"
access_token = "EAABx..."
verify_token = "your-verify-token"
webhook_path = "/hooks/whatsapp"
```

## Setup Steps

### 1. Create a Meta App

1. Go to [Meta for Developers](https://developers.facebook.com/)
2. Create a new app → Select "Business" type
3. Add the "WhatsApp" product

### 2. Configure Webhook

Set the webhook URL to your gateway's public URL:
```
https://your-gateway.example.com/hooks/whatsapp
```

Subscribe to these webhook fields:
- `messages`
- `messaging_postbacks`

### 3. Set Verify Token

The verify token is used during webhook verification. Set it in your config and in the Meta dashboard.

### 4. Get Access Token

From the Meta dashboard, generate a permanent access token for the WhatsApp Business API.

## Message Types

| Type | Support |
|------|---------|
| Text messages | Full |
| Images | Receive + Send |
| Documents | Receive |
| Voice messages | Receive |
| Location | Receive |
| Stickers | Receive |

## Rate Limits

WhatsApp Business API has conversation-based pricing and rate limits:
- 1,000 conversations/month on free tier
- Higher tiers require Meta Business verification

## Security

- Always validate webhook signatures using the app secret
- Store access tokens securely (never in version control)
- Use HTTPS for webhook endpoints

## Troubleshooting

- **Webhook not receiving**: Ensure your gateway is publicly accessible
- **Message not sending**: Check access token validity and phone number ID
- **Rate limited**: Check conversation limits in Meta Business dashboard

## See Also

- [WhatsApp Business API Documentation](https://developers.facebook.com/docs/whatsapp/)
