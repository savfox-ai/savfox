---
name: apple-reminders
description: Manage Apple Reminders on macOS using AppleScript.
version: "1.0.0"
metadata:
  savfox:
    emoji: "✅"
    requires:
      bins:
        - osascript
      env: []
    install:
      - id: manual
        kind: manual
        instructions: "osascript is built into macOS. This skill is macOS-only."
        bins: [osascript]
        label: Built-in (macOS)
---

# Apple Reminders Skill

Manage Apple Reminders using AppleScript on macOS.

## List Reminders

List all incomplete reminders in a list:
```bash
osascript -e 'tell application "Reminders" to get name of every reminder in list "Reminders" whose completed is false'
```

## Create a Reminder

Create a simple reminder:
```bash
osascript -e 'tell application "Reminders" to make new reminder in list "Reminders" with properties {name:"Buy groceries"}'
```

Create a reminder with due date:
```bash
osascript -e 'tell application "Reminders" to make new reminder in list "Reminders" with properties {name:"Submit report", due date:date "2026-02-15 09:00:00"}'
```

Create a reminder with notes:
```bash
osascript -e 'tell application "Reminders" to make new reminder in list "Reminders" with properties {name:"Call dentist", body:"Schedule cleaning appointment"}'
```

## Complete a Reminder

```bash
osascript -e 'tell application "Reminders" to set completed of (first reminder in list "Reminders" whose name is "Buy groceries") to true'
```

## List All Lists

```bash
osascript -e 'tell application "Reminders" to get name of every list'
```

## Create a New List

```bash
osascript -e 'tell application "Reminders" to make new list with properties {name:"Shopping"}'
```

## Delete a Reminder

```bash
osascript -e 'tell application "Reminders" to delete (first reminder in list "Reminders" whose name is "Old Task")'
```

## Guidelines

- This skill is **macOS only** — requires the Reminders app
- Reminders sync via iCloud to iOS/iPadOS devices
- Date format may vary by locale — use ISO format when possible
- First run may trigger a permissions dialog
- Use list names exactly as they appear (case-sensitive)
