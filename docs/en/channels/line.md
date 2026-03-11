# Line Channel

Connect Savfox to Line Messaging API.

## Prerequisites

- Line Developers account
- A Messaging API channel

## Configuration

```toml
[[channels]]
type = "line"
enabled = true

[channels.line]
channel_secret = "${LINE_CHANNEL_SECRET}"
channel_access_token = "${LINE_CHANNEL_ACCESS_TOKEN}"
webhook_path = "/hooks/line"
```

## Setup

1. Go to [Line Developers Console](https://developers.line.biz/)
2. Create a new **Messaging API** channel
3. Note the **Channel Secret** and **Channel Access Token**
4. Set the webhook URL to your gateway's public URL

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Images | Receive + Send |
| Stickers | Receive |
| Location | Receive |
| Rich messages | Send (Flex Message) |
| Group messages | Full |
| 1:1 messages | Full |
