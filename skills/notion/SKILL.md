---
name: notion
description: Create, read, update, and query Notion pages and databases.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📝"
    requires:
      env:
        - NOTION_API_KEY
    install: []
---

# Notion Skill

You can interact with Notion workspaces using the Notion API v1.

## Authentication

Requires `NOTION_API_KEY` environment variable (Internal Integration Token).

## Pages

Create a page:
```bash
curl -X POST https://api.notion.com/v1/pages \
  -H "Authorization: Bearer $NOTION_API_KEY" \
  -H "Notion-Version: 2022-06-28" \
  -H "Content-Type: application/json" \
  -d '{
    "parent": {"database_id": "DATABASE_ID"},
    "properties": {
      "Name": {"title": [{"text": {"content": "My Page"}}]}
    },
    "children": [
      {"object": "block", "type": "paragraph", "paragraph": {"rich_text": [{"text": {"content": "Hello!"}}]}}
    ]
  }'
```

Get a page:
```bash
curl -s "https://api.notion.com/v1/pages/{page_id}" \
  -H "Authorization: Bearer $NOTION_API_KEY" \
  -H "Notion-Version: 2022-06-28" | jq '.properties'
```

## Databases

Query a database:
```bash
curl -X POST "https://api.notion.com/v1/databases/{database_id}/query" \
  -H "Authorization: Bearer $NOTION_API_KEY" \
  -H "Notion-Version: 2022-06-28" \
  -H "Content-Type: application/json" \
  -d '{"filter": {"property": "Status", "select": {"equals": "Done"}}}'
```

## Search

Search across workspace:
```bash
curl -X POST https://api.notion.com/v1/search \
  -H "Authorization: Bearer $NOTION_API_KEY" \
  -H "Notion-Version: 2022-06-28" \
  -H "Content-Type: application/json" \
  -d '{"query": "meeting notes", "filter": {"property": "object", "value": "page"}}'
```

## Guidelines

- Always use `Notion-Version: 2022-06-28` header
- Rich text is always an array of text objects
- Database properties use typed objects (title, rich_text, select, multi_select, etc.)
- Page content is composed of block children
- Use pagination with `start_cursor` for large results
