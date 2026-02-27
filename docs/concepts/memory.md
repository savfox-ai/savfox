# Markdown Memory System

Savfox uses a 4-layer Markdown memory system that gives users and agents
persistent, human-editable knowledge stores. Memory files are plain `.md` files
with optional YAML frontmatter, organized into layers by scope.

## The four layers

| Layer     | Location                                    | Purpose                                    |
|-----------|---------------------------------------------|--------------------------------------------|
| **Global**  | `~/.savfox/memory/global/*.md`            | User preferences, coding conventions       |
| **Project** | `<git-root>/.savfox/memory/*.md`          | Project-specific knowledge                 |
| **Agent**   | `~/.savfox/memory/agents/<name>/*.md`     | Agent personality and knowledge            |
| **Session** | In-memory only (not persisted to disk)     | Temporary working notes for one session    |

Layer directories are resolved at runtime by `discovery.rs`. The Project layer
uses the nearest Git root, so `.savfox/memory/` files travel with the repository.

## File format

Each memory file is a standard Markdown file. An optional YAML frontmatter block
between `---` delimiters appears at the top:

```markdown
---
tags: [rust, patterns]
priority: 8
pinned: true
author: user
created_at: 2025-03-15T10:00:00Z
updated_at: 2025-03-20T14:30:00Z
expires_at: 2025-12-31T23:59:59Z
---

# Coding Conventions

Always use `snake_case` for variable names in Rust.
Prefer `thiserror` for library error types.
```

### Frontmatter fields

| Field        | Type              | Default   | Description                                     |
|--------------|-------------------|-----------|-------------------------------------------------|
| `tags`       | `Vec<String>`     | `[]`      | Searchable tags                                 |
| `priority`   | `u32`             | `5`       | Higher values are included first (1-10)         |
| `pinned`     | `bool`            | `false`   | Pinned entries are always included in the prompt |
| `author`     | `String`          | `"user"`  | Who created this entry (`user` or `agent`)      |
| `created_at` | `DateTime<Utc>`   | `None`    | Creation timestamp                              |
| `updated_at` | `DateTime<Utc>`   | `None`    | Last modification timestamp                     |
| `expires_at` | `DateTime<Utc>`   | `None`    | Auto-expire after this time (skipped on load)   |

### Slug rules

The file stem (without `.md`) serves as the entry's slug identifier. Valid slugs
contain only lowercase alphanumeric characters, hyphens, and underscores. They
must not start with a hyphen or underscore.

Valid: `coding-conventions`, `rust-patterns`, `my_notes_2025`
Invalid: `-leading`, `My Notes`, `path/sep`

Maximum file size: **64 KB** per `.md` file.

## Budget management

When memories are injected into the agent's context, a budget system controls
how much text is included. The default budget is **16,384 bytes** (16 KB).

### Inclusion order

1. **Pinned entries first** -- sorted by priority (descending). Pinned entries
   are always included as long as they fit in the total budget.

2. **Layer budget allocation** -- the remaining budget is divided across layers:

   | Layer   | Share |
   |---------|-------|
   | Session | 40%   |
   | Agent   | 25%   |
   | Project | 25%   |
   | Global  | 10%   |

3. **Within each layer** -- entries are sorted by priority (descending), then
   by `updated_at` (most recent first). Entries are included until the layer
   budget or total budget is exhausted.

### Prompt injection

The assembled memory prompt is injected into the agent's initial context in
`codex.rs` at `build_initial_context()`, after `UserInstructions` and before
`EnvironmentContext`. The output has this structure:

```
# Memory Context

## [global] coding-conventions
Always use snake_case for variable names...

## [project] api-design
REST endpoints follow /api/v1/ prefix...

## [agent] personality
You are a helpful assistant focused on Rust development...
```

## Discovery

The `discover_md_memories()` function scans all layer directories for `.md`
files. It:

1. Resolves directory paths for Global, Project, and Agent layers.
2. Reads each `.md` file under 64 KB.
3. Parses the YAML frontmatter and Markdown body.
4. Skips entries past their `expires_at` timestamp.
5. Returns a `Vec<MdMemoryEntry>` for budget assembly.

Session-layer entries are managed in-memory and are not discovered from disk.

## Agent tool: `md_memory`

The agent can manage memories at runtime using the `md_memory` tool, which
supports 7 actions:

| Action    | Description                                   |
|-----------|-----------------------------------------------|
| `list`    | List all memory entries across layers          |
| `get`     | Read a specific entry by layer and slug        |
| `create`  | Create a new memory file                       |
| `update`  | Update an existing memory file                 |
| `delete`  | Delete a memory file                           |
| `search`  | Search entries by tags or text                 |
| `promote` | Copy a session entry to a persistent layer     |

The tool handler lives at `crates/core/src/tools/handlers/md_memory.rs`.

## Gateway WS-RPC methods

The gateway exposes 8 memory-related RPC methods over WebSocket:

| Method            | Scope | Description                               |
|-------------------|-------|-------------------------------------------|
| `memory.list`     | Read  | List entries, optionally filtered by layer |
| `memory.get`      | Read  | Get a single entry by layer and slug       |
| `memory.create`   | Write | Create a new entry                         |
| `memory.update`   | Write | Update an existing entry                   |
| `memory.delete`   | Write | Delete an entry                            |
| `memory.search`   | Read  | Full-text and tag search                   |
| `memory.promote`  | Write | Promote session entry to persistent layer  |
| `memory.layers`   | Read  | List configured layer directories          |

## Coexistence with JSON memory

The Markdown memory system operates independently from the legacy JSON memory
system (`memory/entries.json`). They use different paths, different tools, and
different storage formats. Both can be active simultaneously.

## Examples

### Creating a global memory

```bash
# Create the directory
mkdir -p ~/.savfox/memory/global

# Write a memory file
cat > ~/.savfox/memory/global/coding-style.md << 'EOF'
---
tags: [style, rust]
priority: 7
pinned: false
author: user
---

# Coding Style Preferences

- Use `Result` instead of panicking.
- Prefer iterators over manual loops.
- Always add doc comments on public items.
EOF
```

### Creating a project memory

```bash
mkdir -p .savfox/memory

cat > .savfox/memory/api-design.md << 'EOF'
---
tags: [api, rest]
priority: 6
---

# API Design Notes

All REST endpoints live under `/api/v1/`.
Use JSON request/response bodies.
Authentication via Bearer token in the Authorization header.
EOF
```
