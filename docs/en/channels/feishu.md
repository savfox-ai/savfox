# Feishu / Lark Channel

Connect Savfox to Feishu (飞书) or Lark, ByteDance's enterprise collaboration platform.

## Prerequisites

- Feishu/Lark developer account
- A custom app with Bot capability

## Configuration

```toml
[[channels]]
type = "feishu"
enabled = true

[channels.feishu]
app_id = "${FEISHU_APP_ID}"
app_secret = "${FEISHU_APP_SECRET}"
verification_token = "${FEISHU_VERIFICATION_TOKEN}"
encrypt_key = "${FEISHU_ENCRYPT_KEY}"
event_mode = "webhook" # or "stream"
webhook_path = "/webhooks/feishu"
receive_id_type = "chat_id"
```

## Setup

1. Go to [Feishu Open Platform](https://open.feishu.cn/)
2. Create a custom app
3. Enable the **Bot** capability
4. Choose **Webhook** or **Stream** event delivery mode
5. If using webhook mode, set the request URL to `/webhooks/feishu`
6. If using stream mode, keep `app_id` + `app_secret` configured so Savfox can open the long-lived stream connection at startup
7. Add required permissions: `im:message`, `im:message.create`

## Features

| Feature | Support |
|---------|---------|
| Webhook challenge / signature / decrypt | Full |
| Stream event subscription | Full |
| Text message trigger | `/savfox ...` |
| Group messages | Full |
| 1:1 messages | Full |
| Rich text / cards / images | Basic send only |
