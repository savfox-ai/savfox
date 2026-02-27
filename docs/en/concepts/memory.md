# Memory System

Savfox uses a 4-layer Markdown memory system that provides persistent context across sessions. Memory entries are plain `.md` files with optional YAML frontmatter, making them easy to read and edit with any text editor.

## The Four Layers

| Layer | Location | Scope | Budget Share |
|-------|----------|-------|-------------|
| **Global** | `~/.savfox/memory/global/*.md` | User preferences, coding conventions | 10% |
| **Project** | `<git-root>/.savfox/memory/*.md` | Project-specific knowledge | 25% |
| **Agent** | `~/.savfox/memory/agents/<name>/*.md` | Agent personality, specialized knowledge | 25% |
| **Session** | In-memory only | Temporary working notes (not persisted) | 40% |

Layers are ordered by specificity. Session memories take priority over project memories, which take priority over global memories.

## File Format

Each memory file is a Markdown document with optional YAML frontmatter between `---` delimiters:

```markdown
---
tags: [rust, conventions]
priority: 8
pinned: false
author: user
created_at: 2025-01-15T10:30:00Z
updated_at: 2025-01-15T10:30:00Z
expires_at: null
---

# Rust Conventions

- Use `thiserror` for library error types.
- Use `anyhow` in binary crates.
- Prefer `impl Trait` over `dyn Trait` where possible.
```

### Frontmatter Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `tags` | list | `[]` | Searchable tags |
| `priority` | integer | `5` | 1 (lowest) to 10 (highest) |
| `pinned` | boolean | `false` | Always include regardless of budget |
| `author` | string | `"user"` | Who created this entry |
| `created_at` | datetime | now | Creation timestamp |
| `updated_at` | datetime | now | Last modification timestamp |
| `expires_at` | datetime | null | Auto-delete after this time |

## Budget System

The total memory budget is **16 KB** by default. Memory entries are assembled into a prompt section injected into the LLM context. The budget controls how much memory content is included.

### Assembly Order

1. **Pinned entries** are always included first, sorted by priority (highest first).
2. Remaining budget is divided across layers by their share percentages.
3. Within each layer, entries are sorted by priority (descending), then by `updated_at` (most recent first).
4. Entries are included until the layer's budget is exhausted.

If a pinned entry's content exceeds the total budget, it is truncated.

## Creating Memories

### Manually

Create a `.md` file in the appropriate layer directory:

```bash
# Global memory
echo "# My Preferences\n- Always use dark mode" > ~/.savfox/memory/global/preferences.md

# Project memory
echo "# API Patterns\n- All endpoints return JSON" > .savfox/memory/api-patterns.md
```

### Via the Agent

The agent can create and update memories using the `md_memory` tool:

```
Save a note that this project uses PostgreSQL 16 with pgvector.
```

The agent will call `md_memory` with the `create` action and write a file to the project memory layer.

### Via the Gateway API

Use the `memory.*` WebSocket RPC methods:

```json
{"jsonrpc": "2.0", "id": 1, "method": "memory.create", "params": {
  "layer": "project",
  "slug": "db-setup",
  "content": "# Database\nPostgreSQL 16 with pgvector extension.",
  "tags": ["database", "setup"]
}}
```

## The md_memory Tool

The agent has access to a built-in `md_memory` tool with these actions:

| Action | Description |
|--------|-------------|
| `list` | List all memories across layers |
| `get` | Read a specific memory by slug and layer |
| `create` | Create a new memory entry |
| `update` | Update an existing memory's content or frontmatter |
| `delete` | Remove a memory entry |
| `search` | Full-text search across all memories |
| `promote` | Move a memory to a higher layer (e.g., session to project) |

## Discovery

On startup, Savfox discovers memory files by scanning the layer directories. File names must be valid slugs (lowercase alphanumeric with hyphens). Files larger than the per-file limit are skipped.

## Prompt Injection

The assembled memory prompt is injected into the LLM context after the user instructions and before the environment context. It appears as a `# Memory Context` section with entries grouped by layer.

## Examples

```bash
# List all memories
ls ~/.savfox/memory/global/
ls .savfox/memory/

# Create a pinned global memory
cat > ~/.savfox/memory/global/code-style.md << 'EOF'
---
tags: [style]
priority: 9
pinned: true
---

# Code Style

- 4-space indentation
- No trailing whitespace
- Maximum line length: 100 characters
EOF
```
