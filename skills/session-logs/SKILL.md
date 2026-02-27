---
name: session-logs
description: Export, search, and analyze conversation session logs.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📜"
    requires:
      bins: []
    install: []
---

# Session Logs Skill

Export and analyze conversation session logs from the gateway.

## List Sessions

```bash
curl -s http://localhost:18881/api/sessions \
  -H "Authorization: Bearer $SAVFOX_TOKEN" | jq '.[] | {id, created_at, message_count}'
```

## Export a Session

Export full session transcript:
```bash
curl -s "http://localhost:18881/api/sessions/{session_id}/history" \
  -H "Authorization: Bearer $SAVFOX_TOKEN" | jq '.messages'
```

## Search Sessions

Search across all sessions:
```json
{"method": "sessions.search", "params": {"query": "search term", "limit": 20}, "id": 1}
```

## Export Formats

- JSON: Full structured data with metadata
- Markdown: Human-readable transcript
- CSV: Tabular data (timestamp, role, content)

## Analysis

Count messages by role:
```bash
curl -s "http://localhost:18881/api/sessions/{id}/history" \
  -H "Authorization: Bearer $SAVFOX_TOKEN" \
  | jq '[.messages[] | .role] | group_by(.) | map({role: .[0], count: length})'
```

## Guidelines

- Session logs may contain sensitive information — handle with care
- Large sessions may need pagination
- Old sessions are automatically pruned based on retention policy
- Use session search for finding specific conversations
