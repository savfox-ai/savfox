# Skills Management

Skills teach Savfox agents how to use external tools and APIs.

## List Skills

```bash
savfox skills list
```

Shows all discovered skills with their status, emoji, and required dependencies.

## Skill Locations

Skills are loaded from these directories (in priority order):

1. `<workspace>/.savfox/skills/` — Project-specific skills
2. `~/.savfox/skills/` — User-level skills
3. Built-in skills directory

## Check Skill Dependencies

```bash
savfox skills check <skill-name>
```

Verifies that required binaries and environment variables are available.

## Using a Skill

Skills are automatically available to the agent. Simply reference them:

```bash
savfox exec "Use the github skill to list my open PRs"
```

The agent reads the skill's SKILL.md file and follows its instructions.

## Built-in Skills

| Skill | Description |
|-------|-------------|
| `github` | GitHub CLI operations (issues, PRs, repos) |
| `slack` | Slack Web API integration |
| `discord` | Discord REST API |
| `weather` | Weather lookup via wttr.in |
| `summarize` | Content summarization |
| `healthcheck` | Service health monitoring |
| `tmux` | Terminal session management |
| `coding-agent` | Delegate coding tasks to sub-agents |
| `notion` | Notion API page/database operations |
| `obsidian` | Obsidian vault management |
| `openai-image` | DALL-E image generation |
| `whisper` | OpenAI Whisper transcription |
| `spotify` | Spotify playback control |
| `trello` | Trello board management |
| `1password` | 1Password CLI integration |
| `video-frames` | Video frame extraction with ffmpeg |
| `mcp-porter` | MCP server bridging |
| `apple-notes` | Apple Notes (macOS) |
| `apple-reminders` | Apple Reminders (macOS) |
| `things` | Things 3 task management (macOS) |

## Creating Custom Skills

See [Creating Custom Skills](../tools/creating-skills.md) for the full guide.
