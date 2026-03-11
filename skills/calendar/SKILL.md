---
name: calendar
description: Query and manage calendar events via CalDAV or platform-specific APIs.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📅"
    requires:
      bins: []
      env: []
    install: []
---

# Calendar Skill

Query and manage calendar events.

## Apple Calendar (macOS)

List today's events:
```bash
osascript -e 'tell application "Calendar"
  set today to current date
  set tomorrow to today + 1 * days
  set eventList to {}
  repeat with cal in calendars
    repeat with ev in (every event of cal whose start date >= today and start date < tomorrow)
      set end of eventList to (summary of ev) & " at " & (start date of ev as string)
    end repeat
  end repeat
  return eventList
end tell'
```

Create event:
```bash
osascript -e 'tell application "Calendar"
  tell calendar "Home"
    make new event with properties {summary:"Meeting", start date:date "2026-02-15 10:00:00", end date:date "2026-02-15 11:00:00"}
  end tell
end tell'
```

## Google Calendar (via gcalcli)

List events:
```bash
gcalcli list
gcalcli agenda
gcalcli agenda "2026-02-14" "2026-02-21"
```

Add event:
```bash
gcalcli add --title "Meeting" --when "2026-02-15 10:00" --duration 60
```

## Guidelines

- Apple Calendar requires macOS with Calendar.app
- For Google Calendar, install `gcalcli` (`pip install gcalcli`)
- For CalDAV servers, use `curl` with WebDAV REPORT requests
