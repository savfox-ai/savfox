---
name: notion
description: Interact with Notion workspaces to manage pages, databases, and content. Use when the user wants to create, edit, or query Notion pages, work with databases, manage tasks, or organize information in their Notion workspace.
metadata:
  short-description: Work with Notion pages and databases
---

# Notion Integration

Interact with Notion workspaces via the Notion API.

## Authentication

Set `NOTION_API_KEY` environment variable with an integration token.

## Page Operations

### Create Pages

```
Create a page titled "<title>"
Create a page in <database> with properties...
Add a new page under <parent page>
```

### Edit Pages

```
Update page <title> with content...
Append to page <title>
Add a heading/paragraph/bullet list to <page>
```

### Query Pages

```
Find page <title>
Search for pages containing "<query>"
Show children of <page>
```

## Database Operations

### Query Databases

```
List all databases
Query <database> where <property> equals <value>
Show entries from <database>
```

### Create Entries

```
Add entry to <database> with properties...
Create a new row in <database>
```

### Update Entries

```
Update entry <id> in <database>
Mark task <name> as complete
```

## Block Types

Supported content blocks:
- Paragraph
- Headings (H1, H2, H3)
- Bullet and numbered lists
- To-do checkboxes
- Code blocks
- Callouts
- Dividers
- Images

## Common Use Cases

1. **Task Management**: Create and update task databases
2. **Documentation**: Create and maintain documentation pages
3. **Notes**: Quick capture of notes and ideas
4. **Knowledge Base**: Organize and retrieve information

## Best Practices

1. Always confirm before deleting content
2. Use appropriate page icons and covers
3. Maintain consistent database schemas
4. Link related pages together
