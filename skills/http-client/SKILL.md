---
name: http-client
description: Make HTTP requests using curl for API testing and debugging.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🌐"
    requires:
      bins:
        - curl
      env: []
    install: []
---

# HTTP Client Skill

Make HTTP requests for API testing and debugging.

## GET Request

```bash
curl -s https://api.example.com/users | jq .
```

With headers:
```bash
curl -s -H "Authorization: Bearer $TOKEN" https://api.example.com/users
```

## POST Request

JSON body:
```bash
curl -s -X POST https://api.example.com/users \
  -H "Content-Type: application/json" \
  -d '{"name": "Alice", "email": "alice@example.com"}'
```

## PUT Request

```bash
curl -s -X PUT https://api.example.com/users/1 \
  -H "Content-Type: application/json" \
  -d '{"name": "Updated Name"}'
```

## DELETE Request

```bash
curl -s -X DELETE https://api.example.com/users/1 \
  -H "Authorization: Bearer $TOKEN"
```

## Show Response Headers

```bash
curl -sI https://api.example.com/health
```

Full response with headers:
```bash
curl -sv https://api.example.com/health 2>&1
```

## Download File

```bash
curl -L -o output.zip https://example.com/file.zip
```

## Form Data

```bash
curl -X POST https://api.example.com/upload \
  -F "file=@document.pdf" \
  -F "name=My Document"
```

## Guidelines

- Use `-s` (silent) to suppress progress bars
- Use `| jq .` to pretty-print JSON responses
- Use `-L` to follow redirects
- Use `-v` for verbose output (debugging)
- Use `-w '\n'` to add trailing newline
- Store tokens in env vars, not in commands
