# savfox-skill-registry

Skill registry crate for Savfox — discovers, searches, installs, and manages skills.

## Registry Model

The registry follows a **git-based** model with a 3-tier hierarchy:

```
realms/
├── _/                    # Default realm
│   └── flocks/
│       ├── core/
│       │   ├── flock.json
│       │   └── skills.json
│       └── dev-tools/
│           ├── flock.json
│           └── skills.json
└── savfox/               # Named realm
    └── flocks/
        └── ...
```

- **Realms** — top-level groupings (e.g., organization namespaces)
- **Flocks** — collections of related skills within a realm
- **Skills** — individual skill entries listed in `skills.json`

## Directory Layout

After installation, the skills directory is organized as follows:

```
{savfox_home}/skills/
├── .system/              # Embedded system skills (calendar, github, etc.)
│   ├── .system.marker
│   ├── github/SKILL.md
│   └── ...
├── .registry/            # Git-cloned registry repos
│   └── savfox/           # Default registry (id from config)
│       ├── realms/
│       │   ├── _/
│       │   └── savfox/
│       └── ...
├── .custom/              # User-installed skills (from URL or zip)
│   ├── my-skill/SKILL.md
│   └── ...
└── {other dirs}          # Legacy installed skills (still scanned)
```

## Configuration

Add a `[registry]` section to `config.toml`:

```toml
[registry]
id = "savfox"                                        # Registry identifier
git = "https://github.com/savhub-ai/registry.git"   # Git URL to clone
```

Both fields have sensible defaults and are optional.

## Installation Sources

Skills can be installed from three sources:

1. **Registry** — browse and install from the git-based registry
2. **Git URL** — install directly from any git repository
3. **Zip upload** — upload a zip file containing one or more skills

Git URL and zip installs are placed in the `.custom/` directory.

## Search

The `search(query)` method walks the registry's `realms/*/flocks/*/skills.json` files and matches the query against skill names, summaries, keywords, and categories using case-insensitive substring matching.
