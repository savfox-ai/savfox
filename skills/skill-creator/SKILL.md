---
name: skill-creator
description: Meta-skill for creating new Savfox skills from descriptions.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🧬"
    requires:
      bins: []
      env: []
    install: []
---

# Skill Creator

Create new Savfox skills from natural language descriptions.

## Creating a Skill

When asked to create a new skill, generate a `SKILL.md` file with:

1. **YAML Frontmatter** — metadata about the skill
2. **Markdown Body** — instructions for agents

### Template

```markdown
---
name: <skill-id>
description: <one-line description>
version: "1.0.0"
metadata:
  savfox:
    emoji: "<emoji>"
    requires:
      bins:
        - <required-binary>
      env:
        - <REQUIRED_ENV_VAR>
    install:
      - id: <installer-id>
        kind: <brew|apt|cargo|npm|winget|scoop|choco|manual>
        formula: <package-name>
        bins: [<binary-name>]
        label: <Display Name>
---

# <Skill Name>

<Brief description of what this skill does.>

## <Action 1>

<Description and example>
```bash
<exact command>
```

## <Action 2>

...

## Guidelines

- <Important notes about usage>
- <Rate limits or restrictions>
- <Best practices>
```

## Placement

Save skills to one of these locations:
- **Project-specific**: `<workspace>/.savfox/skills/<skill-id>/SKILL.md`
- **User-level**: `~/.savfox/skills/<skill-id>/SKILL.md`
- **Built-in**: `skills/<skill-id>/SKILL.md` (in the Savfox repo)

## Writing Guidelines

1. **Be specific** — Include exact command syntax with all flags
2. **Show real examples** — Use realistic data in examples
3. **Document all options** — List available parameters and formats
4. **Include error handling** — Common errors and how to fix them
5. **Keep it focused** — One skill per tool/API, not a general reference
6. **Use code blocks** — Always wrap commands in fenced code blocks
7. **Note requirements** — List all required binaries and env vars in frontmatter

## Supported Install Kinds

| Kind | Platform | Example |
|------|----------|---------|
| `brew` | macOS/Linux | `formula: jq` |
| `apt` | Debian/Ubuntu | `package: jq` |
| `cargo` | Any (Rust) | `crate: ripgrep` |
| `npm` | Any (Node.js) | `package: @modelcontextprotocol/server-github` |
| `winget` | Windows | `package: stedolan.jq` |
| `scoop` | Windows | `package: jq` |
| `choco` | Windows | `package: jq` |
| `manual` | Any | `instructions: "Download from https://..."` |

## Validation

After creating a skill, verify it works:
```bash
savfox exec "Use the <skill-id> skill to <action>"
```
