---
name: slack
description: Send messages and manage Slack workspace channels, users, and integrations.
version: "1.0.0"
metadata:
  savfox:
    emoji: "💬"
    requires:
      bins:
        - slack
      env:
        - SLACK_TOKEN
    install:
      - id: brew
        kind: brew
        formula: slack-cli
        bins: [slack]
        label: Homebrew (macOS/Linux)
      - id: npm
        kind: npm
        npm_package: "@slack/cli"
        bins: [slack]
        label: npm (cross-platform)
---

# Slack Skill

You can interact with Slack workspaces using the Slack CLI or Slack Web API.

## Authentication

Requires `SLACK_TOKEN` environment variable set to a Slack Bot or User token with appropriate scopes.

## Sending Messages

Send a message to a channel:
```bash
curl -X POST https://slack.com/api/chat.postMessage \
  -H "Authorization: Bearer $SLACK_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"channel": "C0123456", "text": "Hello from Savfox!"}'
```

## Channel Operations

List channels:
```bash
curl -s https://slack.com/api/conversations.list \
  -H "Authorization: Bearer $SLACK_TOKEN" | jq '.channels[] | {id, name}'
```

Get channel info:
```bash
curl -s "https://slack.com/api/conversations.info?channel=C0123456" \
  -H "Authorization: Bearer $SLACK_TOKEN" | jq '.channel'
```

## User Operations

List users:
```bash
curl -s https://slack.com/api/users.list \
  -H "Authorization: Bearer $SLACK_TOKEN" | jq '.members[] | {id, name, real_name}'
```

## File Uploads

Upload a file to a channel:
```bash
curl -F file=@document.pdf -F channels=C0123456 -F initial_comment="Here's the file" \
  -H "Authorization: Bearer $SLACK_TOKEN" \
  https://slack.com/api/files.upload
```

## Reminders

Set a reminder:
```bash
curl -X POST https://slack.com/api/reminders.add \
  -H "Authorization: Bearer $SLACK_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"text": "Review PR", "time": "in 30 minutes"}'
```

## Guidelines

- Always check channel permissions before posting
- Use thread_ts for threaded replies to avoid channel clutter
- Rate limit: respect Slack's API rate limits (1 request/second for most methods)
- For bulk operations, use pagination with cursor parameter
