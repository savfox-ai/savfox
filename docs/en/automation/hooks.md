# Hook System

Hooks allow you to intercept and modify messages at various points in the processing pipeline.

## Hook Types

### Webhook Hooks
HTTP endpoints that receive event notifications:

```toml
[[hooks]]
type = "webhook"
event = "message.received"
url = "https://your-server.com/hook"
secret = "hmac-secret"
```

### Wake Hooks
Trigger an agent turn from external events:

```bash
curl -X POST http://localhost:18881/hooks/wake \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message": "Check for new emails"}'
```

### Inbound Hooks
Transform or route incoming messages before they reach the agent:

```bash
curl -X POST http://localhost:18881/hooks/inbound \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"source": "custom-webhook", "payload": {"text": "Process this"}}'
```

## Auto-Reply Rules

Auto-reply rules trigger automated responses based on message patterns:

```json
{
  "id": "greeting",
  "name": "Greeting Response",
  "triggers": [
    {"type": "keywords", "keywords": ["hello", "hi", "hey"]}
  ],
  "response": {
    "type": "text",
    "template": "Hello {{user}}! How can I help you today?"
  }
}
```

### Trigger Types

| Type | Description |
|------|-------------|
| `mention` | Bot is @mentioned |
| `keywords` | Message contains any keyword (case-insensitive) |
| `regex` | Message matches regex pattern |
| `prefix` | Message starts with prefix (e.g., "!") |
| `always` | Fire on every message in channel |

### Template Variables

- `{{user}}` — User ID or name
- `{{channel}}` — Channel identifier
- `{{message}}` — Original message text
- `{{timestamp}}` — Current ISO timestamp

## Cron Hooks

Schedule recurring agent tasks:

```json
{
  "method": "cron.create",
  "params": {
    "name": "daily-summary",
    "schedule": {"type": "cron", "expression": "0 9 * * *"},
    "payload": {"type": "agentTurn", "prompt": "Summarize yesterday's activity"}
  }
}
```

See [Cron documentation](cron.md) for full scheduling options.
