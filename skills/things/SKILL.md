---
name: things
description: Manage tasks in Things 3 (macOS) using AppleScript and URL schemes.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📋"
    requires:
      bins:
        - osascript
      env: []
    install:
      - id: manual
        kind: manual
        instructions: "Requires Things 3 from the Mac App Store."
        bins: [osascript]
        label: Things 3 (macOS)
---

# Things Skill

Manage tasks in Things 3 on macOS.

## List Tasks

List all inbox tasks:
```bash
osascript -e 'tell application "Things3" to get name of every to do of list "Inbox"'
```

List tasks in a project:
```bash
osascript -e 'tell application "Things3" to get name of every to do of project "My Project"'
```

List today's tasks:
```bash
osascript -e 'tell application "Things3" to get name of every to do of list "Today"'
```

## Create a Task

Simple task in Inbox:
```bash
osascript -e 'tell application "Things3" to make new to do with properties {name:"New task"}'
```

Task with notes and due date:
```bash
osascript -e 'tell application "Things3" to make new to do with properties {name:"Review PR", notes:"Check the auth changes", due date:date "2026-02-15"}'
```

## Using Things URL Scheme

Create a task via URL scheme (works reliably):
```bash
open "things:///add?title=My%20Task&notes=Some%20notes&when=today"
```

Create with tags and list:
```bash
open "things:///add?title=Deploy%20app&tags=work,urgent&list=My%20Project&when=2026-02-15"
```

## Complete a Task

```bash
osascript -e 'tell application "Things3" to set status of (first to do whose name is "Task Name") to completed'
```

## List Projects

```bash
osascript -e 'tell application "Things3" to get name of every project'
```

## List Areas

```bash
osascript -e 'tell application "Things3" to get name of every area'
```

## Guidelines

- Requires **Things 3** installed from Mac App Store
- macOS only — Things uses its own sync (Things Cloud)
- URL scheme is the most reliable method for creating tasks
- AppleScript access may need to be granted in System Preferences
- Task properties: name, notes, due date, tags, status
- Lists: Inbox, Today, Upcoming, Anytime, Someday, Logbook
