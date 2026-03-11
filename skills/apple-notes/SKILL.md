---
name: apple-notes
description: Create, read, and search Apple Notes on macOS using AppleScript.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📝"
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

# Apple Notes Skill

Manage Apple Notes using AppleScript on macOS.

## List Notes

List all notes with their titles:
```bash
osascript -e 'tell application "Notes" to get name of every note'
```

List notes in a specific folder:
```bash
osascript -e 'tell application "Notes" to get name of every note in folder "Work"'
```

## Read a Note

Read note content by title:
```bash
osascript -e 'tell application "Notes" to get plaintext of note "My Note Title"'
```

## Create a Note

Create a new note:
```bash
osascript -e 'tell application "Notes" to make new note at folder "Notes" with properties {name:"Title", body:"<h1>Title</h1><p>Body text here</p>"}'
```

## Search Notes

Search notes by content:
```bash
osascript -e 'tell application "Notes"
  set matchingNotes to {}
  repeat with n in every note
    if plaintext of n contains "search term" then
      set end of matchingNotes to name of n
    end if
  end repeat
  return matchingNotes
end tell'
```

## List Folders

```bash
osascript -e 'tell application "Notes" to get name of every folder'
```

## Delete a Note

```bash
osascript -e 'tell application "Notes" to delete note "Note Title"'
```

## Guidelines

- This skill is **macOS only** — requires the Notes app
- Note body uses HTML format (not plain text) for creation
- AppleScript may prompt for permissions on first use
- Large note collections may be slow to enumerate
- Notes are synced via iCloud — changes propagate to all devices
- Avoid running in quick succession to prevent rate limiting by Notes.app
