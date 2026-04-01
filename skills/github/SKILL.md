---
name: github
description: "Interact with GitHub repositories, issues, pull requests, and more using the gh CLI"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\U0001F419"
    requires:
      bins: ["gh"]
      env: ["GITHUB_TOKEN"]
    install:
      - id: brew
        kind: brew
        formula: gh
        bins: [gh]
        label: "Install GitHub CLI via Homebrew"
      - id: apt
        kind: apt
        package: gh
        bins: [gh]
        label: "Install GitHub CLI via apt"
      - id: winget
        kind: winget
        package: GitHub.cli
        bins: [gh]
        label: "Install GitHub CLI via winget"
      - id: scoop
        kind: scoop
        package: gh
        bins: [gh]
        label: "Install GitHub CLI via Scoop"
---
# GitHub Skill

You have access to the GitHub CLI (`gh`) to interact with GitHub repositories, issues, pull requests, releases, and more.

## Authentication

Before using any commands, verify that authentication is set up:

```bash
gh auth status
```

If not authenticated, the user will need to run `gh auth login` interactively.

## Common Operations

### Issues

- **List issues:** `gh issue list` (add `--state open|closed|all`, `--label <label>`, `--assignee <user>`)
- **View an issue:** `gh issue view <number>`
- **Create an issue:** `gh issue create --title "<title>" --body "<body>"` (add `--label`, `--assignee`, `--milestone` as needed)
- **Close an issue:** `gh issue close <number>`
- **Reopen an issue:** `gh issue reopen <number>`
- **Add a comment:** `gh issue comment <number> --body "<comment>"`

### Pull Requests

- **List PRs:** `gh pr list` (add `--state`, `--author`, `--base`, `--head`)
- **View a PR:** `gh pr view <number>`
- **Create a PR:** `gh pr create --title "<title>" --body "<body>"` (add `--base <branch>`, `--draft`, `--reviewer <user>`)
- **Checkout a PR locally:** `gh pr checkout <number>`
- **Merge a PR:** `gh pr merge <number>` (add `--merge`, `--squash`, or `--rebase`)
- **Review a PR:** `gh pr review <number> --approve` or `--request-changes --body "<feedback>"`
- **View PR diff:** `gh pr diff <number>`
- **View PR checks:** `gh pr checks <number>`

### Repositories

- **View repo info:** `gh repo view`
- **Clone a repo:** `gh repo clone <owner/repo>`
- **Fork a repo:** `gh repo fork <owner/repo>`
- **Create a repo:** `gh repo create <name> --public|--private`
- **List repos:** `gh repo list <owner>`

### Releases

- **List releases:** `gh release list`
- **View a release:** `gh release view <tag>`
- **Create a release:** `gh release create <tag> --title "<title>" --notes "<notes>"`
- **Download release assets:** `gh release download <tag>`

### Gists

- **List gists:** `gh gist list`
- **Create a gist:** `gh gist create <file> --public --desc "<description>"`
- **View a gist:** `gh gist view <id>`

### Actions / Workflows

- **List workflow runs:** `gh run list`
- **View a run:** `gh run view <run-id>`
- **Watch a run:** `gh run watch <run-id>`
- **Rerun a failed run:** `gh run rerun <run-id>`
- **List workflows:** `gh workflow list`

### API Access

For anything not covered by a dedicated subcommand, use the generic API:

```bash
gh api repos/{owner}/{repo}/commits --paginate
gh api graphql -f query='{ viewer { login } }'
```

## Guidelines

1. Always use `--json` output format when you need to process the result programmatically: `gh issue list --json number,title,state`.
2. Prefer `gh` over raw `curl` calls to the GitHub API -- it handles authentication and pagination automatically.
3. When creating issues or PRs, always include a descriptive body.
4. Before pushing or merging, verify the current branch and remote with `gh repo view` and `git status`.
5. If a command fails with an auth error, suggest the user run `gh auth login`.
