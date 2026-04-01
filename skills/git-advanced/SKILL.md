---
name: git-advanced
description: Advanced git operations beyond basic commit/push workflows.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🔀"
    requires:
      bins:
        - git
      env: []
    install: []
---

# Git Advanced Skill

Advanced git operations for complex workflows.

## Interactive Rebase

Squash last N commits:
```bash
git rebase -i HEAD~N
```

Rebase onto main:
```bash
git rebase main
```

## Cherry Pick

Apply a specific commit:
```bash
git cherry-pick <commit-hash>
```

Cherry pick without committing:
```bash
git cherry-pick --no-commit <commit-hash>
```

## Stash

Save work in progress:
```bash
git stash push -m "description"
```

List stashes:
```bash
git stash list
```

Apply and drop:
```bash
git stash pop
```

Apply specific stash:
```bash
git stash apply stash@{2}
```

## Bisect

Find the commit that introduced a bug:
```bash
git bisect start
git bisect bad          # current commit is bad
git bisect good v1.0    # known good commit
# ... test each suggested commit ...
git bisect good/bad     # mark each
git bisect reset        # when done
```

## Worktrees

Work on multiple branches simultaneously:
```bash
git worktree add ../feature-branch feature-branch
git worktree list
git worktree remove ../feature-branch
```

## Log and History

Pretty log:
```bash
git log --oneline --graph --all --decorate
```

Search commits by message:
```bash
git log --grep="fix" --oneline
```

Find who changed a line:
```bash
git blame -L 10,20 file.rs
```

Search for string in history:
```bash
git log -S "function_name" --oneline
```

## Reflog

Recover lost commits:
```bash
git reflog
git checkout <lost-hash>
```

## Guidelines

- Never force push to shared branches without coordination
- Use `git stash` before switching branches with uncommitted changes
- Use `git bisect` to efficiently find bug-introducing commits
- Use `git worktree` instead of cloning for parallel branch work
- Always backup before destructive operations (rebase, reset --hard)
