# Mattermost Channel

Connect Savfox to Mattermost, an open-source team collaboration platform.

## Prerequisites

- Mattermost server (self-hosted or cloud)
- Bot account or personal access token

## Configuration

```toml
[[channels]]
type = "mattermost"
enabled = true

[channels.mattermost]
server_url = "https://mattermost.example.com"
token = "${MATTERMOST_TOKEN}"
team_name = "my-team"
channels = ["town-square"]
```

## Setup

### Create a Bot Account

1. Go to **System Console** → **Integrations** → **Bot Accounts**
2. Create a new bot account
3. Copy the access token

### Or Use Personal Access Token

1. Go to **Account Settings** → **Security** → **Personal Access Tokens**
2. Create a new token

## Features

| Feature | Support |
|---------|---------|
| Text messages | Full |
| Markdown | Full |
| File attachments | Receive |
| Threads | Full |
| Reactions | Receive |
| Direct messages | Full |
| Channel messages | Full |

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `server_url` | Required | Mattermost server URL |
| `token` | Required | Bot or personal access token |
| `team_name` | Required | Team to operate in |
| `channels` | `[]` | Channels to monitor |
| `respond_to_dm` | `true` | Respond to direct messages |
