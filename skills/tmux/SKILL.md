---
name: tmux
description: "Manage tmux sessions, windows, and panes for terminal multiplexing"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\U0001F4DF"
    requires:
      bins: ["tmux"]
    install:
      - id: brew
        kind: brew
        formula: tmux
        bins: [tmux]
        label: "Install tmux via Homebrew"
      - id: apt
        kind: apt
        package: tmux
        bins: [tmux]
        label: "Install tmux via apt"
---
# Tmux Skill

You can manage tmux sessions, windows, and panes to help users with terminal multiplexing tasks.

## Session Management

### List Sessions

```bash
tmux list-sessions
```

Returns a list of active sessions with their names, window counts, and dimensions.

### Create a New Session

```bash
tmux new-session -d -s <session-name>
```

The `-d` flag starts the session detached. Always use detached mode so you do not lose control of the current terminal.

### Attach to a Session

```bash
tmux attach-session -t <session-name>
```

### Kill a Session

```bash
tmux kill-session -t <session-name>
```

### Rename a Session

```bash
tmux rename-session -t <old-name> <new-name>
```

## Window Management

### Create a New Window

```bash
tmux new-window -t <session-name> -n <window-name>
```

### List Windows

```bash
tmux list-windows -t <session-name>
```

### Select a Window

```bash
tmux select-window -t <session-name>:<window-index>
```

### Rename a Window

```bash
tmux rename-window -t <session-name>:<window-index> <new-name>
```

### Close a Window

```bash
tmux kill-window -t <session-name>:<window-index>
```

## Pane Management

### Split Horizontally

```bash
tmux split-window -h -t <session-name>:<window-index>
```

### Split Vertically

```bash
tmux split-window -v -t <session-name>:<window-index>
```

### List Panes

```bash
tmux list-panes -t <session-name>:<window-index>
```

### Send Keys to a Pane

Run a command inside a specific pane without attaching:

```bash
tmux send-keys -t <session-name>:<window>.<pane> "<command>" Enter
```

### Capture Pane Output

Read what is currently displayed in a pane:

```bash
tmux capture-pane -t <session-name>:<window>.<pane> -p
```

## Common Workflows

### Dev Environment Setup

Create a session with multiple windows for a project:

```bash
tmux new-session -d -s dev -n editor
tmux send-keys -t dev:editor "vim ." Enter
tmux new-window -t dev -n server
tmux send-keys -t dev:server "npm run dev" Enter
tmux new-window -t dev -n logs
tmux send-keys -t dev:logs "tail -f /var/log/app.log" Enter
```

### Monitor Multiple Services

```bash
tmux new-session -d -s monitor -n services
tmux split-window -h -t monitor:services
tmux split-window -v -t monitor:services.0
tmux send-keys -t monitor:services.0 "watch docker ps" Enter
tmux send-keys -t monitor:services.1 "htop" Enter
tmux send-keys -t monitor:services.2 "tail -f /var/log/syslog" Enter
```

## Guidelines

1. Always create sessions in detached mode (`-d`) to avoid hijacking the user's terminal.
2. Use descriptive session and window names so they are easy to identify later.
3. When running long-lived commands, use `send-keys` to type the command into the pane.
4. Use `capture-pane -p` to read output from a pane without attaching to it.
5. Before creating a new session, check if one with that name already exists using `tmux has-session -t <name>` to avoid errors.
6. When the user asks to "clean up" tmux, list sessions first and confirm before killing them.
7. Tmux is not available on Windows natively. If on Windows, suggest using WSL or an alternative like Windows Terminal tabs.
