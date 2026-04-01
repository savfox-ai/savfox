---
name: github
description: GitHub integration for repository management, pull requests, issues, and code review. Use when the user wants to interact with GitHub repositories, create or review pull requests, manage issues, search code, or perform git operations via the GitHub API.
metadata:
  short-description: Work with GitHub repos, PRs, and issues
---

# GitHub Integration

Interact with GitHub repositories using the GitHub API.

## Authentication

Set the `GITHUB_TOKEN` environment variable with a personal access token.

## Common Operations

### Repository Operations

```bash
# List repositories
gh repo list <owner>

# Create a repository
gh repo create <name> --public|--private

# Clone a repository
gh repo clone <owner>/<repo>
```

### Pull Requests

```bash
# Create a PR
gh pr create --title "Title" --body "Description"

# List PRs
gh pr list --state open

# Review a PR
gh pr review <number> --approve|--request-changes

# Merge a PR
gh pr merge <number> --merge|--squash
```

### Issues

```bash
# Create an issue
gh issue create --title "Title" --body "Description"

# List issues
gh issue list --state open

# Close an issue
gh issue close <number>
```

### Code Search

```bash
# Search code
gh search code "<query>" --repo <owner>/<repo>

# Search issues
gh search issues "<query>"
```

## Best Practices

1. Always use descriptive PR titles and include context in descriptions
2. Reference issues in commits using `#<issue-number>`
3. Request reviews from appropriate team members
4. Use draft PRs for work-in-progress
