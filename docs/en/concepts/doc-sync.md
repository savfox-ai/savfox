# Documentation Sync

User-visible and contract-visible changes must update documentation in the same change set whenever practical.

## Must-sync surfaces

Keep English and Chinese docs aligned for:
- CLI commands and flags
- gateway behavior and deployment flows
- app-server and protocol contracts
- configuration semantics
- TUI interaction model when the behavior is user-visible

## Process

1. update the code or contract
2. update the English doc entrypoint
3. update the Chinese counterpart, or explicitly mark why none exists yet
4. update summary/navigation files when adding new pages

## PR checklist

PRs should answer:
- which user-facing docs changed?
- which Chinese counterpart changed?
- if none, why is the change internal-only?

## What does not require bilingual sync every time

These can stay English-only unless they become user-facing policy:
- highly internal crate notes
- temporary engineering scratch docs
- implementation-only refactor notes

Even then, the public surface docs must remain consistent.
