---
name: trello
description: Manage Trello boards, lists, and cards for project management.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📋"
    requires:
      env:
        - TRELLO_API_KEY
        - TRELLO_TOKEN
    install: []
---

# Trello Skill

Manage Trello boards, lists, and cards using the Trello REST API.

## Authentication

Requires both `TRELLO_API_KEY` and `TRELLO_TOKEN` environment variables.

## Boards

List boards:
```bash
curl -s "https://api.trello.com/1/members/me/boards?key=$TRELLO_API_KEY&token=$TRELLO_TOKEN" \
  | jq '.[] | {id, name}'
```

## Lists

Get lists on a board:
```bash
curl -s "https://api.trello.com/1/boards/{boardId}/lists?key=$TRELLO_API_KEY&token=$TRELLO_TOKEN" \
  | jq '.[] | {id, name}'
```

## Cards

Create a card:
```bash
curl -X POST "https://api.trello.com/1/cards?key=$TRELLO_API_KEY&token=$TRELLO_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"idList": "LIST_ID", "name": "New Task", "desc": "Task description"}'
```

Move a card to another list:
```bash
curl -X PUT "https://api.trello.com/1/cards/{cardId}?key=$TRELLO_API_KEY&token=$TRELLO_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"idList": "NEW_LIST_ID"}'
```

Get cards in a list:
```bash
curl -s "https://api.trello.com/1/lists/{listId}/cards?key=$TRELLO_API_KEY&token=$TRELLO_TOKEN" \
  | jq '.[] | {id, name, desc: .desc[:50]}'
```

## Guidelines

- Board IDs can be found in the board URL (e.g., trello.com/b/{boardId}/...)
- Use labels and due dates for better organization
- Card descriptions support Markdown
