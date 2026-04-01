# Google Chat Channel

Connect Savfox to Google Chat (Google Workspace).

## Prerequisites

- Google Workspace account
- Google Cloud project with Chat API enabled
- Service account credentials

## Configuration

```toml
[[channels]]
type = "googlechat"
enabled = true

[channels.googlechat]
credentials_file = "/path/to/service-account.json"
webhook_path = "/hooks/googlechat"
```

## Setup

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Enable the **Google Chat API**
3. Create a service account and download credentials
4. Configure the Chat app in the [Chat API configuration](https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat)
5. Set the bot URL to your gateway

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Cards (Card v2) | Send |
| Threads | Full |
| Spaces | Full |
| Direct messages | Full |
| @mention trigger | Full |
| Slash commands | Configurable |
