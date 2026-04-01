---
name: clipboard
description: Read from and write to the system clipboard.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📋"
    requires:
      bins: []
      env: []
    install: []
---

# Clipboard Skill

Read from and write to the system clipboard.

## macOS

Copy to clipboard:
```bash
echo "text" | pbcopy
cat file.txt | pbcopy
```

Paste from clipboard:
```bash
pbpaste
pbpaste > output.txt
```

## Linux (X11)

Copy:
```bash
echo "text" | xclip -selection clipboard
echo "text" | xsel --clipboard --input
```

Paste:
```bash
xclip -selection clipboard -o
xsel --clipboard --output
```

## Linux (Wayland)

Copy:
```bash
echo "text" | wl-copy
```

Paste:
```bash
wl-paste
```

## Windows (PowerShell)

Copy:
```powershell
"text" | Set-Clipboard
Get-Content file.txt | Set-Clipboard
```

Paste:
```powershell
Get-Clipboard
```

## Cross-Platform Pattern

```bash
if command -v pbcopy &>/dev/null; then
    echo "text" | pbcopy        # macOS
elif command -v xclip &>/dev/null; then
    echo "text" | xclip -sel c  # Linux X11
elif command -v wl-copy &>/dev/null; then
    echo "text" | wl-copy       # Linux Wayland
fi
```

## Guidelines

- Use `pbcopy`/`pbpaste` on macOS
- Use `xclip` or `xsel` on Linux X11
- Use `wl-copy`/`wl-paste` on Wayland
- Pipe command output directly to clipboard
