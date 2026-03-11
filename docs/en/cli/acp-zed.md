# ACP Bridge for Zed

Savfox can expose a stdio ACP bridge via:

```bash
savfox acp --gateway-url http://127.0.0.1:18881 --token "$SAVFOX_TOKEN"
```

## Zed configuration

Add an ACP server entry in your Zed settings:

```json
{
  "agent_servers": {
    "savfox": {
      "command": "savfox",
      "args": [
        "acp",
        "--gateway-url",
        "http://127.0.0.1:18881",
        "--token",
        "${SAVFOX_TOKEN}"
      ]
    }
  }
}
```

## ACP methods supported by `savfox acp`

- `prompt` -> gateway `chat.send`
- `cancel` -> gateway `chat.abort`

Bridge events are forwarded as JSON lines with methods:

- `stream`
- `complete`
- `error`
- `tool_call`
- `tool_result`
