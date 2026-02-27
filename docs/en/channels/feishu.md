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
webhook_path = "/hooks/feishu"
```

## Setup

1. Go to [Feishu Open Platform](https://open.feishu.cn/)
2. Create a custom app
3. Enable the **Bot** capability
4. Set the event subscription URL
5. Add required permissions: `im:message`, `im:message.create`

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Rich text | Send |
| Interactive cards | Send |
| Images | Receive + Send |
| Group messages | Full |
| 1:1 messages | Full |
| @mention trigger | Full |
