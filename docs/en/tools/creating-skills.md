# Creating Custom Skills

Skills are markdown-based capability definitions that teach Savfox agents how to use external tools and APIs.

## Skill File Format

Each skill is a `SKILL.md` file with YAML frontmatter and markdown instructions:

```markdown
---
name: my-skill
description: A brief description of what this skill does.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔧"
    requires:
      bins:
        - my-tool
      env:
        - MY_API_KEY
    install:
      - id: brew
        kind: brew
        formula: my-tool
        bins: [my-tool]
        label: Homebrew
---

# My Skill

Instructions for the agent on how to use this skill...
```

## Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Unique skill identifier |
| `description` | Yes | Human-readable description |
| `version` | Yes | Semantic version |
| `metadata.savfox.emoji` | No | Display emoji |
| `metadata.savfox.requires.bins` | No | Required executables on PATH |
| `metadata.savfox.requires.env` | No | Required environment variables |
| `metadata.savfox.install` | No | Installation methods |

## Installation Methods

Supported `kind` values:
- `brew` — Homebrew (macOS/Linux)
- `apt` — APT (Debian/Ubuntu)
- `cargo` — Cargo (Rust)
- `npm` — npm (Node.js)
- `winget` — Winget (Windows)
- `scoop` — Scoop (Windows)
- `choco` — Chocolatey (Windows)
- `manual` — Manual installation with instructions

## Writing Good Instructions

1. **Be specific** — Include exact command syntax and expected output
2. **Show examples** — Use code blocks with real-world examples
3. **Document options** — List available flags, formats, and parameters
4. **Include guidelines** — Rate limits, best practices, error handling
5. **Keep it concise** — Agents work best with focused instructions

## Skill Locations

Skills are loaded from these directories (in priority order):
1. `<workspace>/.savfox/skills/` — Project-specific skills
2. `~/.savfox/skills/` — User skills
3. Built-in skills directory

## Testing a Skill

Verify your skill works by asking an agent to use it:

```bash
savfox exec "Use the my-skill skill to do X"
```

The agent will read the SKILL.md and follow the instructions.
