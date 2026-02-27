---
name: 1password
description: Securely access 1Password vault items, credentials, and secrets.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔐"
    requires:
      bins:
        - op
    install:
      - id: brew
        kind: brew
        formula: 1password-cli
        bins: [op]
        label: Homebrew (macOS/Linux)
      - id: winget
        kind: winget
        package: AgileBits.1Password.CLI
        bins: [op]
        label: Winget (Windows)
---

# 1Password Skill

Access 1Password vaults using the `op` CLI.

## Authentication

Sign in first: `op signin` or use service accounts via `OP_SERVICE_ACCOUNT_TOKEN`.

## Read Items

Get a password:
```bash
op item get "My Login" --fields password
```

Get full item details:
```bash
op item get "My Login" --format json | jq '{title, username: .fields[] | select(.label=="username") | .value}'
```

## List Items

```bash
op item list --format json | jq '.[] | {id, title, category}'
```

## Secrets References

Use `op://vault/item/field` syntax for secret references:
```bash
op read "op://Personal/AWS/access_key"
```

## Inject Secrets

Inject secrets into a config template:
```bash
op inject -i template.env -o .env
```

## Guidelines

- Never store 1Password credentials in plain text
- Use `op read` for single values, `op inject` for templates
- Prefer service account tokens for automation
- Always use `--format json` for machine-readable output
