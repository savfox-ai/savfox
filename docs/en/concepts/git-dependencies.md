# Git Dependency Policy

Savfox intentionally carries a small set of git dependencies and git patches. That flexibility must stay governed.

## Why git dependencies exist here

Current use cases include:
- tracking upstream fixes not yet released on crates.io
- keeping protocol- or transport-specific forks aligned with Savfox behavior
- pinning exact revisions for compatibility-sensitive integrations

Examples in the workspace include OpenTelemetry git patches, eventsource-related forks, and a few platform/integration dependencies pinned to upstream commits or branches.

## Required metadata for each git dependency

Each git dependency should have a recorded answer for:
1. why crates.io is insufficient right now
2. whether the dependency is a short-term bridge or a long-term fork
3. what upstream release, commit, or issue would let us remove it
4. who owns upgrades or breakage response

## Allowed categories

- exact commit pin for reproducibility
- branch tracking only when there is a clear operational reason
- `[patch.crates-io]` use when the workspace must force a coherent transitive version

## Preferred policy

Prefer, in order:
1. crates.io release
2. exact git revision
3. tracked branch
4. floating upstream main branch only when unavoidable

## Upgrade workflow

When changing a git dependency:
- document the reason in the PR
- note blast radius in the test slice
- prefer a commit pin after validating a branch update
- keep removal opportunities visible in maintenance review
