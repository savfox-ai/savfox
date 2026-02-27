---
name: shell-commands
description: Execute shell commands with safety checks and approvals. Use when the user wants to run terminal commands, execute scripts, manage files via command line, or perform system operations. Always follow security guidelines.
metadata:
  short-description: Run shell commands safely
---

# Shell Commands

Execute shell commands with appropriate safety measures.

## Safety Guidelines

### Always Ask Approval For

1. Commands that modify files (`rm`, `mv`, `cp`)
2. Commands that install software (`apt`, `npm`, `pip`)
3. Commands that change system settings
4. Commands that access sensitive data
5. Commands with elevated privileges (`sudo`)

### Safe to Execute Without Approval

1. Read-only commands (`ls`, `cat`, `head`)
2. Information commands (`pwd`, `whoami`, `date`)
3. Non-destructive searches (`grep`, `find`)

## Command Categories

### File Operations

```bash
ls -la                    # List files
cat <file>                # Read file
mkdir -p <dir>            # Create directory
rm -rf <path>             # Remove (requires approval)
cp -r <src> <dest>        # Copy (requires approval)
mv <src> <dest>           # Move/rename (requires approval)
```

### Process Management

```bash
ps aux                    # List processes
kill <pid>                # Kill process (requires approval)
top                       # System monitor
htop                      # Interactive monitor
```

### Network Operations

```bash
curl <url>                # HTTP request
wget <url>                # Download file
ping <host>               # Test connectivity
netstat -an               # Network statistics
```

### Git Operations

```bash
git status                # Check status
git log --oneline         # View history
git diff                  # View changes
git add .                 # Stage changes
git commit -m "message"   # Commit (requires approval)
git push                  # Push (requires approval)
```

## Best Practices

1. Explain what a command will do before executing
2. Show command output to the user
3. Handle errors gracefully
4. Clean up temporary files
5. Never execute commands blindly from user input
6. Validate and sanitize all inputs

## Error Handling

1. Check command exit codes
2. Capture and display stderr
3. Provide helpful error messages
4. Suggest fixes for common errors
