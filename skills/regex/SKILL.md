---
name: regex
description: Find and transform text using regular expressions with grep and sed.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔍"
    requires:
      bins: []
      env: []
    install: []
---

# Regex Skill

Find and transform text using regular expressions.

## Search with grep

Basic search:
```bash
grep -r "pattern" .
```

Case insensitive:
```bash
grep -ri "error" logs/
```

Extended regex:
```bash
grep -E "error|warning|fatal" logfile.txt
```

Show line numbers and context:
```bash
grep -n -C 3 "TODO" src/**/*.rs
```

Files only:
```bash
grep -rl "deprecated" src/
```

Invert match (lines NOT matching):
```bash
grep -v "DEBUG" logfile.txt
```

## Search with ripgrep (rg)

```bash
rg "fn\s+\w+\(" --type rust
rg "TODO|FIXME" -g "*.{rs,ts,py}"
rg "import.*from" --type ts -l
```

## Common Patterns

| Pattern | Matches |
|---------|---------|
| `\d+` | One or more digits |
| `\b\w+\b` | Word boundaries |
| `[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}` | Email addresses |
| `https?://[^\s]+` | URLs |
| `\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b` | IPv4 addresses |
| `[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}` | UUIDs |

## Transform with sed

Replace first occurrence per line:
```bash
sed 's/old/new/' file.txt
```

Replace all occurrences:
```bash
sed 's/old/new/g' file.txt
```

In-place edit:
```bash
sed -i 's/old/new/g' file.txt
```

Delete matching lines:
```bash
sed '/pattern/d' file.txt
```

## Guidelines

- Use `-E` (extended regex) for `+`, `?`, `|`, `()` without escaping
- Use ripgrep (`rg`) over grep for large codebases — it's faster
- Test regex on sample data before running on production files
- Use `sed -i.bak` to create backups when editing in-place
