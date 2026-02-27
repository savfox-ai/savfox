# Interactive Mode

When you run `savfox` without a subcommand, it launches a ratatui-based terminal user interface for interactive sessions with the AI agent.

## Launching

```bash
savfox                    # start a new session
savfox resume             # pick a previous session to resume
savfox resume --last      # resume the most recent session
savfox fork               # fork from a previous session
```

## Interface Overview

The TUI provides:

- **Chat composer** — Type messages and send them to the agent
- **Markdown rendering** — Agent responses are rendered with syntax highlighting
- **Diff display** — Proposed file changes shown as unified diffs
- **Approval modals** — Accept or reject commands and file edits
- **Status indicators** — Loading animations and progress feedback

## Session Management

### Resume

Continue a previous conversation where you left off:

```bash
savfox resume             # shows an interactive session picker
savfox resume --last      # resume the most recent session
savfox resume <SESSION_ID>
```

### Fork

Branch from a previous session to explore alternative approaches:

```bash
savfox fork               # shows a session picker, then forks
```

### Archive

Sessions are stored locally and can be archived or listed through the TUI.

## Approval Workflow

When the agent wants to execute a command or modify a file, Savfox presents an approval modal:

- **Command execution** — Review the command before it runs
- **File changes** — Review the diff before it's applied

You can approve or decline each action. The approval policy can be configured globally:

```bash
savfox --ask-for-approval unless-trusted   # default: ask unless command is trusted
savfox --ask-for-approval never            # auto-approve everything (use with sandbox)
savfox --ask-for-approval on-failure       # only ask when something fails
```

## Model and Personality Selection

From the TUI, you can:

- Switch between available LLM models
- Select different agent personalities / collaboration modes
- Configure via overlays accessible from the interface

Or set the model from the command line:

```bash
savfox -m gpt-4o
savfox --oss           # use local models (Ollama, LM Studio)
```

## Slash Commands

The interactive mode supports slash commands for quick actions. Type `/` in the chat composer to see available commands.

## Keyboard Shortcuts

The TUI uses standard terminal keybindings. Navigation and input follow typical terminal conventions.

## Tips

- Use `Ctrl+C` to interrupt a running agent turn
- Sessions persist automatically — you can always resume later
- Attach images to your message with `-i`:
  ```bash
  savfox -i screenshot.png
  ```
