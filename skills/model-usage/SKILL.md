---
name: model-usage
description: Track and report LLM model usage statistics, token consumption, and costs.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📊"
    requires:
      bins: []
    install: []
---

# Model Usage Skill

Track and report LLM model usage across sessions.

## Checking Usage

Query the gateway's usage endpoint:
```bash
curl -s http://localhost:18881/api/status \
  -H "Authorization: Bearer $SAVFOX_TOKEN" | jq '.usage'
```

## Usage via WS-RPC

Send a WebSocket RPC call to get detailed usage:
```json
{"method": "usage.summary", "params": {"period": "today"}, "id": 1}
```

Available periods: `today`, `week`, `month`, `all`

## Token Counting

Approximate token count for text:
- English: ~4 characters per token
- Code: ~3 characters per token
- CJK: ~1-2 characters per token

## Cost Estimation

Approximate costs per 1M tokens (varies by model):
- GPT-4o: $2.50 input / $10.00 output
- GPT-4o-mini: $0.15 input / $0.60 output
- Claude 3.5 Sonnet: $3.00 input / $15.00 output
- Claude Haiku: $0.25 input / $1.25 output

## Guidelines

- Monitor usage regularly to avoid unexpected costs
- Set up alerts for unusual spikes in token consumption
- Use cheaper models for simple tasks (routing, classification)
- Reserve expensive models for complex reasoning tasks
