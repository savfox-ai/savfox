---
name: memory
description: Persistent memory storage for conversations, preferences, and context. Use when you need to remember information across conversations, store user preferences, maintain context about projects, or recall past interactions and decisions.
metadata:
  short-description: Store and recall information persistently
---

# Memory System

Store and retrieve information persistently across conversations.

## Storage Operations

### Save Information

```
Remember that <fact>
Store <information> for later
Save this preference: <preference>
Note that <observation>
```

### Categories

Organize memories into categories:
- `preferences` - User preferences and settings
- `projects` - Project-specific information
- `contacts` - People and their details
- `facts` - General facts to remember
- `decisions` - Important decisions made
- `context` - Current context and state

## Retrieval Operations

### Recall Information

```
What do you know about <topic>?
Recall information about <subject>
Show me preferences for <category>
What have I told you about <topic>?
```

### List Memories

```
Show all memories
List preferences
What projects do we have?
Show recent memories
```

## Memory Management

### Update

```
Update <memory> with <new information>
Change preference <name> to <value>
```

### Delete

```
Forget <information>
Remove memory about <topic>
Clear all memories in <category>
```

## Use Cases

1. **User Preferences**: Remember preferred languages, formats, styles
2. **Project Context**: Keep track of ongoing projects and decisions
3. **Personal Details**: Remember names, dates, important events
4. **Learning**: Build knowledge over time from conversations

## Best Practices

1. Ask before storing sensitive information
2. Summarize long memories for efficient storage
3. Use meaningful categories for organization
4. Regularly review and clean up outdated memories
5. Cross-reference related memories when useful
