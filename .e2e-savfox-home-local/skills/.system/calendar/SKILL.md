---
name: calendar
description: Manage calendar events, schedules, and reminders. Use when the user wants to create events, check their schedule, find available times, set reminders, or manage appointments across their calendars.
metadata:
  short-description: Manage calendar events and schedules
---

# Calendar Management

Manage events and schedules across calendar services.

## Authentication

Requires OAuth access to calendar providers (Google Calendar, Outlook, etc.).

## Event Operations

### Create Events

```
Schedule a meeting on <date> at <time>
Add event "<title>" from <start> to <end>
Create an all-day event on <date>
Block out time for <activity> on <date>
```

### Query Events

```
What's on my calendar today?
Show my schedule for <date>
What do I have next week?
When is my next meeting with <person>?
```

### Modify Events

```
Reschedule <event> to <new time>
Move <event> to <new date>
Cancel <event>
Update <event> description to <text>
```

## Scheduling

### Find Available Times

```
When am I free on <date>?
Find a 1-hour slot this week
When can I meet with <person>?
What's my next available time?
```

### Conflict Detection

```
Do I have any conflicts on <date>?
Check if <time> is available
Show overlapping events
```

## Reminders

```
Remind me about <event> 15 minutes before
Set a reminder for <date/time>
Add a daily reminder for <activity>
```

## Recurring Events

```
Create a weekly meeting on <day>
Add a daily standup at <time>
Schedule monthly review on <day>
```

## Best Practices

1. Always confirm event details before creating
2. Include relevant attendees and locations
3. Set appropriate reminders for important events
4. Check for conflicts before scheduling
5. Handle timezone conversions appropriately
