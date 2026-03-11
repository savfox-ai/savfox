---
name: obsidian
description: Read, create, and modify Obsidian vault notes with frontmatter and wiki-links.
version: "1.0.0"
metadata:
  savfox:
    emoji: "💎"
    requires:
      bins: []
    install: []
---

# Obsidian Skill

You can interact with Obsidian vaults by directly reading and writing markdown files on the filesystem. Obsidian vaults are simply directories of `.md` files.

## Finding the Vault

Obsidian vaults are typically at:
- macOS: `~/Documents/Obsidian/` or `~/Library/Mobile Documents/iCloud~md~obsidian/Documents/`
- Linux: `~/Documents/Obsidian/` or `~/Obsidian/`
- Windows: `C:\Users\<user>\Documents\Obsidian\`

Check `.obsidian/` subdirectory to confirm it's a vault.

## Reading Notes

Simply read the markdown file. Notes support YAML frontmatter:

```markdown
---
tags: [meeting, project-x]
date: 2026-02-13
---

# Meeting Notes
...
```

## Creating Notes

Write a new `.md` file in the vault directory. Use proper frontmatter:
```bash
cat > "vault/Daily Notes/2026-02-13.md" << 'EOF'
---
tags: [daily]
date: 2026-02-13
---

# 2026-02-13

## Tasks
- [ ] Review PR
- [ ] Write documentation
EOF
```

## Wiki-Links

Obsidian uses `[[wiki-links]]` for inter-note links:
- `[[Note Name]]` — link to a note
- `[[Note Name#Heading]]` — link to a heading
- `[[Note Name|Display Text]]` — link with display text
- `![[Image.png]]` — embed an image

## Tags

Tags use `#tag` syntax in body or `tags:` in frontmatter.

## Searching

Find notes matching a pattern:
```bash
grep -rl "search term" vault/ --include="*.md"
```

Find notes by tag:
```bash
grep -rl "#project-x" vault/ --include="*.md"
```

## Guidelines

- Preserve existing frontmatter when editing notes
- Use wiki-links `[[]]` instead of standard markdown links for internal references
- Respect the vault's folder structure
- Daily notes typically go in `Daily Notes/` folder
- Templates are in `.obsidian/templates/` or `Templates/`
