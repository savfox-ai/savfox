---
name: jq
description: Process and transform JSON data with jq.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔧"
    requires:
      bins:
        - jq
      env: []
    install:
      - id: brew
        kind: brew
        formula: jq
        bins: [jq]
        label: Homebrew
      - id: apt
        kind: apt
        package: jq
        bins: [jq]
        label: APT
      - id: choco
        kind: choco
        package: jq
        bins: [jq]
        label: Chocolatey
---

# jq Skill

Process and transform JSON data.

## Basic Selection

Get a field:
```bash
echo '{"name":"Alice","age":30}' | jq '.name'
```

Get nested field:
```bash
echo '{"user":{"name":"Alice"}}' | jq '.user.name'
```

## Arrays

Get array element:
```bash
echo '[1,2,3]' | jq '.[0]'
```

Map over array:
```bash
echo '[{"name":"a"},{"name":"b"}]' | jq '.[].name'
```

Filter array:
```bash
echo '[{"age":20},{"age":30},{"age":25}]' | jq '[.[] | select(.age > 22)]'
```

## Transform

Create new object:
```bash
echo '{"first":"Alice","last":"Smith"}' | jq '{full_name: (.first + " " + .last)}'
```

Group by:
```bash
echo '[{"type":"a","v":1},{"type":"b","v":2},{"type":"a","v":3}]' | jq 'group_by(.type) | map({type: .[0].type, count: length})'
```

## File Operations

Read from file:
```bash
jq '.key' data.json
```

Pretty print:
```bash
jq '.' ugly.json
```

Compact output:
```bash
jq -c '.' data.json
```

Raw strings (no quotes):
```bash
jq -r '.name' data.json
```

## Guidelines

- Use `-r` for raw string output (no JSON quotes)
- Use `-c` for compact single-line output
- Use `--slurp` to read multiple JSON objects as array
- Use `--arg name value` to pass shell variables
- Pipe `curl -s` output directly into jq for API responses
